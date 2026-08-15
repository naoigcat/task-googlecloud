use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
    mpsc,
};
use std::thread;
use std::time::Instant;

use tempfile::NamedTempFile;

use super::{Cloud, Duration, ObjectPath, StorageApi, StorageClient};

fn read_headers(stream: &mut TcpStream) {
    let mut request = Vec::new();
    let mut buffer = [0_u8; 1];
    while !request.ends_with(b"\r\n\r\n") {
        stream.read_exact(&mut buffer).unwrap();
        request.push(buffer[0]);
    }
}

fn read_request(stream: &mut TcpStream) -> String {
    let mut request = Vec::new();
    let mut buffer = [0_u8; 1];
    while !request.ends_with(b"\r\n\r\n") {
        stream.read_exact(&mut buffer).unwrap();
        request.push(buffer[0]);
    }
    let content_length = String::from_utf8_lossy(&request)
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse::<usize>().unwrap())
        })
        .unwrap_or(0);
    let mut body = vec![0; content_length];
    stream.read_exact(&mut body).unwrap();
    request.extend(body);
    String::from_utf8(request).unwrap()
}

fn write_json(stream: &mut TcpStream, body: &str) {
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(response.as_bytes()).unwrap();
}

#[test]
fn uploads_files_with_a_longer_timeout_than_api_requests() {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let base = format!("http://{}/storage/v1", listener.local_addr().unwrap());
    let server = thread::spawn(move || {
        let (mut lock_stream, _) = listener.accept().unwrap();
        read_headers(&mut lock_stream);
        write_json(&mut lock_stream, r#"{"generation":"lock"}"#);

        let (mut stream, _) = listener.accept().unwrap();
        read_headers(&mut stream);
        thread::sleep(Duration::from_millis(100));
        write_json(&mut stream, r#"{"generation":"456"}"#);

        let (mut release_stream, _) = listener.accept().unwrap();
        read_headers(&mut release_stream);
        write_json(&mut release_stream, "{}");
    });
    let source = NamedTempFile::new().unwrap();
    std::fs::write(source.path(), b"contents").unwrap();
    let target = ObjectPath::parse("gs://bucket/target").unwrap();
    let storage = StorageApi::with_endpoint_options(
        Cloud::new(),
        base.clone(),
        base,
        Some("token".to_string()),
        Duration::from_millis(10),
    );

    assert_eq!(storage.upload_file(source.path(), &target).unwrap(), "456");
    server.join().unwrap();
}

#[test]
fn times_out_uploads_that_stop_responding() {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let base = format!("http://{}/storage/v1", listener.local_addr().unwrap());
    let (headers_sent, headers_received) = mpsc::channel();
    let (release_sender, release_receiver) = mpsc::channel();
    let server = thread::spawn(move || {
        let (mut lock_stream, _) = listener.accept().unwrap();
        read_headers(&mut lock_stream);
        write_json(&mut lock_stream, r#"{"generation":"lock"}"#);

        let (mut stream, _) = listener.accept().unwrap();
        read_headers(&mut stream);
        headers_sent.send(()).unwrap();
        release_receiver.recv().unwrap();
        let (mut release_stream, _) = listener.accept().unwrap();
        read_headers(&mut release_stream);
        write_json(&mut release_stream, "{}");
    });
    let source = NamedTempFile::new().unwrap();
    std::fs::write(source.path(), b"contents").unwrap();
    let target = ObjectPath::parse("gs://bucket/target").unwrap();
    let storage = StorageApi::with_endpoint_options_and_upload_timeout(
        Cloud::new(),
        base.clone(),
        base,
        Some("token".to_string()),
        Duration::from_secs(30),
        Duration::from_millis(10),
    );
    let (result_sender, result_receiver) = mpsc::channel();
    let upload = thread::spawn(move || {
        result_sender
            .send(storage.upload_file(source.path(), &target))
            .unwrap();
    });

    headers_received
        .recv_timeout(Duration::from_secs(1))
        .unwrap();
    release_sender.send(()).unwrap();
    let result = result_receiver.recv_timeout(Duration::from_secs(1));
    upload.join().unwrap();
    server.join().unwrap();

    let error = result.unwrap().unwrap_err();

    assert!(matches!(error, super::AppError::Http(_)));
}

#[test]
fn interrupts_a_blocking_cloud_storage_request() {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let base = format!("http://{}/storage/v1", listener.local_addr().unwrap());
    let (request_sender, request_received) = mpsc::channel();
    let server = thread::spawn(move || {
        let (mut lock_stream, _) = listener.accept().unwrap();
        read_headers(&mut lock_stream);
        write_json(&mut lock_stream, r#"{"generation":"lock"}"#);

        let (mut stream, _) = listener.accept().unwrap();
        read_headers(&mut stream);
        request_sender.send(()).unwrap();

        let (mut release_stream, _) = listener.accept().unwrap();
        read_headers(&mut release_stream);
        write_json(&mut release_stream, "{}");
    });
    let interrupted = Arc::new(AtomicBool::new(false));
    let interrupt = super::InterruptFlag::from_atomic(Arc::clone(&interrupted));
    let storage = StorageApi::with_endpoint_options(
        Cloud::with_interrupt(interrupt),
        base.clone(),
        base,
        Some("token".to_string()),
        Duration::from_secs(30),
    );
    let (result_sender, result_receiver) = mpsc::channel();
    let request = thread::spawn(move || {
        result_sender.send(storage.list_objects("bucket")).unwrap();
    });

    request_received
        .recv_timeout(Duration::from_secs(1))
        .unwrap();
    let started = Instant::now();
    interrupted.store(true, Ordering::Relaxed);
    let result = result_receiver
        .recv_timeout(Duration::from_millis(500))
        .unwrap();

    assert!(started.elapsed() < Duration::from_millis(500));
    assert!(matches!(
        result,
        Err(super::AppError::InterruptedAfterRequest)
    ));
    request.join().unwrap();
    server.join().unwrap();
}

#[test]
fn distinguishes_interrupts_before_and_after_a_storage_request() {
    let interrupted = Arc::new(AtomicBool::new(true));
    let storage = StorageApi::with_endpoint_options(
        Cloud::with_interrupt(super::InterruptFlag::from_atomic(interrupted)),
        "http://127.0.0.1:1/storage/v1",
        "http://127.0.0.1:1/storage/v1",
        Some("token".to_string()),
        Duration::from_secs(30),
    );
    let before_request = storage
        .transport
        .send_body(storage.transport.client().get("http://127.0.0.1:1"))
        .unwrap_err();

    assert!(matches!(&before_request, super::AppError::Interrupted));
    assert!(!before_request.may_have_sent_storage_request());

    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let base = format!("http://{}/storage/v1", listener.local_addr().unwrap());
    let request_url = format!("{base}/b/bucket/o/object");
    let (request_sender, request_received) = mpsc::channel();
    let (release_sender, release_receiver) = mpsc::channel();
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        read_headers(&mut stream);
        request_sender.send(()).unwrap();
        release_receiver.recv().unwrap();
    });
    let interrupted = Arc::new(AtomicBool::new(false));
    let storage = StorageApi::with_endpoint_options(
        Cloud::with_interrupt(super::InterruptFlag::from_atomic(Arc::clone(&interrupted))),
        base.clone(),
        base,
        Some("token".to_string()),
        Duration::from_secs(30),
    );
    let (result_sender, result_receiver) = mpsc::channel();
    let request = thread::spawn(move || {
        result_sender
            .send(
                storage
                    .transport
                    .send_body(storage.transport.client().get(request_url)),
            )
            .unwrap();
    });

    request_received
        .recv_timeout(Duration::from_secs(1))
        .unwrap();
    interrupted.store(true, Ordering::Relaxed);
    let after_request = result_receiver
        .recv_timeout(Duration::from_millis(500))
        .unwrap()
        .unwrap_err();
    release_sender.send(()).unwrap();
    request.join().unwrap();
    server.join().unwrap();

    assert!(matches!(
        &after_request,
        super::AppError::InterruptedAfterRequest
    ));
    assert!(after_request.may_have_sent_storage_request());
}

#[test]
fn releases_a_bucket_lock_after_an_interrupted_upload() {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let base = format!("http://{}/storage/v1", listener.local_addr().unwrap());
    let (request_sender, request_received) = mpsc::channel();
    let server = thread::spawn(move || {
        let (mut lock_stream, _) = listener.accept().unwrap();
        read_headers(&mut lock_stream);
        write_json(&mut lock_stream, r#"{"generation":"lock"}"#);

        let (mut upload_stream, _) = listener.accept().unwrap();
        read_headers(&mut upload_stream);
        request_sender.send(()).unwrap();

        let (mut release_stream, _) = listener.accept().unwrap();
        read_headers(&mut release_stream);
        write_json(&mut release_stream, "{}");
    });
    let interrupted = Arc::new(AtomicBool::new(false));
    let interrupt = super::InterruptFlag::from_atomic(Arc::clone(&interrupted));
    let storage = StorageApi::with_endpoint_options(
        Cloud::with_interrupt(interrupt),
        base.clone(),
        base,
        Some("token".to_string()),
        Duration::from_secs(30),
    );
    let source = NamedTempFile::new().unwrap();
    std::fs::write(source.path(), b"contents").unwrap();
    let target = ObjectPath::parse("gs://bucket/target").unwrap();
    let upload = thread::spawn(move || storage.upload_file(source.path(), &target));

    request_received
        .recv_timeout(Duration::from_secs(1))
        .unwrap();
    interrupted.store(true, Ordering::Relaxed);
    let error = upload
        .join()
        .unwrap()
        .expect_err("interrupted upload should fail");

    assert!(matches!(error, super::AppError::InterruptedAfterRequest));
    server.join().unwrap();
}

#[test]
fn removes_a_bucket_lock_after_an_unacknowledged_response() {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let base = format!("http://{}/storage/v1", listener.local_addr().unwrap());
    let server = thread::spawn(move || {
        let (mut create_stream, _) = listener.accept().unwrap();
        let request = read_request(&mut create_stream);
        let token = request
            .split_once("\r\n\r\n")
            .map_or("", |(_, body)| body)
            .to_string();
        write_json(&mut create_stream, "not-json");

        let (mut media_stream, _) = listener.accept().unwrap();
        read_headers(&mut media_stream);
        write_json(&mut media_stream, &token);

        let (mut metadata_stream, _) = listener.accept().unwrap();
        read_headers(&mut metadata_stream);
        write_json(&mut metadata_stream, r#"{"generation":"lock"}"#);

        let (mut delete_stream, _) = listener.accept().unwrap();
        read_headers(&mut delete_stream);
        write_json(&mut delete_stream, "{}");
    });
    let storage =
        StorageApi::with_endpoints(Cloud::new(), base.clone(), base, Some("token".to_string()));
    let error = storage.acquire_bucket_lock("bucket").unwrap_err();

    server.join().unwrap();

    assert!(error.to_string().contains("bucket lock"), "{error}");
}

#[test]
fn preserves_recovery_details_for_generation_confirmation_token_failures() {
    let interrupted = Arc::new(AtomicBool::new(true));
    let storage = StorageApi::with_endpoints(
        Cloud::with_interrupt(super::InterruptFlag::from_atomic(interrupted)),
        "http://127.0.0.1:1/storage/v1",
        "http://127.0.0.1:1/storage/v1",
        None,
    );
    let object = ObjectPath::parse("gs://bucket/object").unwrap();

    let error = storage
        .confirm_object_generation(&object, "22", "move object")
        .unwrap_err();

    assert!(
        matches!(
            &error,
            super::AppError::Recovery {
                operation,
                details,
                ..
            } if operation == "move object"
                && details.contains("Expected generation 22")
                && details.contains("Cloud Storage token retrieval failed")
        ),
        "{error}"
    );
}

#[cfg(unix)]
#[test]
fn refuses_upload_sources_through_parent_symlinks() {
    use std::os::unix::fs::symlink;

    let root = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(root.path()).unwrap();
    let outside = tempfile::tempdir().unwrap();
    let source = outside.path().join("secret.txt");
    std::fs::write(&source, "secret").unwrap();
    let linked_bucket = root.join("linked-bucket");
    symlink(outside.path(), &linked_bucket).unwrap();
    let source_through_link = linked_bucket.join("secret.txt");
    let target = ObjectPath::parse("gs://bucket/target").unwrap();
    let mut storage = StorageApi::with_endpoint_options(
        Cloud::new(),
        "http://127.0.0.1:1/storage/v1",
        "http://127.0.0.1:1/storage/v1",
        Some("token".to_string()),
        Duration::from_millis(10),
    );
    storage.upload_root = Some(root);

    let error = storage
        .upload_file(&source_through_link, &target)
        .unwrap_err();

    assert!(matches!(error, super::AppError::UploadSource(_)), "{error}");
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn refuses_upload_sources_after_upload_root_is_replaced() {
    use crate::atomic_rename::directory_identity_from_path;

    let parent = tempfile::tempdir().unwrap();
    let root = parent.path().join("uploads");
    std::fs::create_dir(&root).unwrap();
    let expected_root = directory_identity_from_path(&root).unwrap();
    let replacement = parent.path().join("replacement");
    std::fs::create_dir(&replacement).unwrap();
    std::fs::remove_dir(&root).unwrap();
    std::fs::rename(&replacement, &root).unwrap();

    let source = root.join("bucket/file.txt");
    std::fs::create_dir(source.parent().unwrap()).unwrap();
    std::fs::write(&source, "replacement").unwrap();
    let target = ObjectPath::parse("gs://bucket/target").unwrap();
    let mut storage = StorageApi::with_endpoint_options(
        Cloud::new(),
        "http://127.0.0.1:1/storage/v1",
        "http://127.0.0.1:1/storage/v1",
        Some("token".to_string()),
        Duration::from_millis(10),
    );
    storage.upload_root = Some(root);
    storage
        .set_upload_root_identity(Some(expected_root))
        .unwrap();

    let error = storage.upload_file(&source, &target).unwrap_err();

    assert!(matches!(error, super::AppError::UploadSource(_)), "{error}");
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn refuses_upload_sources_after_a_file_or_bucket_is_replaced() {
    use crate::atomic_rename::{directory_identity_from_path, file_identity_from_path};

    for replace_bucket in [false, true] {
        let parent = tempfile::tempdir().unwrap();
        let root = parent.path().join("uploads");
        let bucket = root.join("bucket");
        let source = bucket.join("file.txt");
        std::fs::create_dir_all(&bucket).unwrap();
        std::fs::write(&source, "original").unwrap();
        let expected_root = directory_identity_from_path(&root).unwrap();
        let expected_directory = directory_identity_from_path(&bucket).unwrap();
        let expected_file = file_identity_from_path(&source).unwrap();

        std::fs::remove_file(&source).unwrap();
        if replace_bucket {
            std::fs::remove_dir(&bucket).unwrap();
            std::fs::create_dir(&bucket).unwrap();
        }
        std::fs::write(&source, "replacement").unwrap();

        let target = ObjectPath::parse("gs://bucket/target").unwrap();
        let mut storage = StorageApi::with_endpoint_options(
            Cloud::new(),
            "http://127.0.0.1:1/storage/v1",
            "http://127.0.0.1:1/storage/v1",
            Some("token".to_string()),
            Duration::from_millis(10),
        );
        storage.upload_root = Some(root);
        storage
            .set_upload_root_identity(Some(expected_root))
            .unwrap();
        let identity = super::UploadSourceIdentity {
            file: expected_file,
            directory: expected_directory,
        };

        let error = storage
            .upload_file_with_identity(&source, &target, Some(identity))
            .unwrap_err();

        assert!(matches!(error, super::AppError::UploadSource(_)), "{error}");
    }
}
