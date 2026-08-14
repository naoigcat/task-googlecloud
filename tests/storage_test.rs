use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use task_googlecloud::{AppError, Cloud, ObjectPath, StorageApi, StorageClient};
use tempfile::{NamedTempFile, tempdir};

const SERVER_TIMEOUT: Duration = Duration::from_secs(5);

fn test_server(responses: Vec<(u16, String)>) -> (String, Arc<Mutex<Vec<String>>>, JoinHandle<()>) {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    listener.set_nonblocking(true).unwrap();
    let address = format!("http://{}/storage/v1", listener.local_addr().unwrap());
    let requests = Arc::new(Mutex::new(Vec::new()));
    let recorded_requests = Arc::clone(&requests);
    let handle = thread::spawn(move || {
        for (status, body) in responses {
            let mut stream = accept_connection(&listener);
            let request = read_request(&mut stream);
            recorded_requests.lock().unwrap().push(request);
            let reason = match status {
                200 => "OK",
                403 => "Forbidden",
                404 => "Not Found",
                _ => "Error",
            };
            let response = format!(
                "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            stream.write_all(response.as_bytes()).unwrap();
        }
    });
    (address, requests, handle)
}

fn incomplete_response_server() -> (String, Arc<Mutex<Vec<String>>>, JoinHandle<()>) {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    listener.set_nonblocking(true).unwrap();
    let address = format!("http://{}/storage/v1", listener.local_addr().unwrap());
    let requests = Arc::new(Mutex::new(Vec::new()));
    let recorded_requests = Arc::clone(&requests);
    let handle = thread::spawn(move || {
        let mut stream = accept_connection(&listener);
        recorded_requests
            .lock()
            .unwrap()
            .push(read_request(&mut stream));
        stream
            .write_all(
                b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 10\r\nConnection: close\r\n\r\n{}",
            )
            .unwrap();
    });
    (address, requests, handle)
}

fn accept_connection(listener: &TcpListener) -> TcpStream {
    let deadline = Instant::now() + SERVER_TIMEOUT;
    loop {
        match listener.accept() {
            Ok((stream, _)) => {
                stream.set_read_timeout(Some(SERVER_TIMEOUT)).unwrap();
                stream.set_write_timeout(Some(SERVER_TIMEOUT)).unwrap();
                return stream;
            }
            Err(error)
                if error.kind() == std::io::ErrorKind::WouldBlock && Instant::now() < deadline =>
            {
                thread::sleep(Duration::from_millis(10));
            }
            Err(error) => panic!("failed to accept test request: {error}"),
        }
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

fn request_line(request: &str) -> &str {
    request.split_once("\r\n").map_or(request, |(line, _)| line)
}

fn storage(base: &str) -> StorageApi {
    StorageApi::with_endpoints(Cloud::new(), base, base, Some("token".to_string()))
}

#[test]
fn parses_and_round_trips_storage_uris() {
    let path = ObjectPath::parse("gs://bucket/folder*?[]#/object").unwrap();
    assert_eq!(path.bucket, "bucket");
    assert_eq!(path.object, "folder*?[]#/object");
    assert_eq!(path.uri(), "gs://bucket/folder*?[]#/object");
}

#[test]
fn encodes_object_names_as_path_data() {
    let (base, requests, server) = test_server(vec![(
        200,
        r#"{"done":true,"resource":{"generation":"456"}}"#.to_string(),
    )]);
    let storage = storage(&base);
    let source = ObjectPath::parse("gs://bucket/folder*?[]#/source").unwrap();
    let target = ObjectPath::parse("gs://bucket/folder*?[]#/target").unwrap();

    storage.copy_object(&source, &target, None, None).unwrap();
    server.join().unwrap();

    let requests = requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(
        request_line(&requests[0]),
        "POST /storage/v1/b/bucket/o/folder%2A%3F%5B%5D%23%2Fsource/rewriteTo/b/bucket/o/folder%2A%3F%5B%5D%23%2Ftarget HTTP/1.1"
    );
}

#[test]
fn keeps_leading_hyphens_in_object_names() {
    let (base, requests, server) = test_server(vec![(
        200,
        r#"{"done":true,"resource":{"generation":"456"}}"#.to_string(),
    )]);
    let storage = storage(&base);
    let source = ObjectPath::parse("gs://bucket/source").unwrap();
    let target = ObjectPath::parse("gs://bucket/-target?").unwrap();

    storage
        .copy_object(&source, &target, Some("123"), Some("0"))
        .unwrap();
    server.join().unwrap();

    let requests = requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(
        request_line(&requests[0]),
        "POST /storage/v1/b/bucket/o/source/rewriteTo/b/bucket/o/-target%3F?sourceGeneration=123&ifSourceGenerationMatch=123&ifGenerationMatch=0 HTTP/1.1"
    );
}

#[test]
fn lists_empty_and_paginated_responses() {
    let (base, requests, server) = test_server(vec![(200, "{}".to_string())]);
    let objects = storage(&base).list_objects("bucket").unwrap();
    server.join().unwrap();
    assert!(objects.is_empty());
    let requests = requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(
        request_line(&requests[0]),
        "GET /storage/v1/b/bucket/o?maxResults=1000 HTTP/1.1"
    );

    let (base, requests, server) = test_server(vec![
        (
            200,
            r#"{"items":[{"name":"folder*?[]#/é.txt"}],"nextPageToken":"next"}"#.to_string(),
        ),
        (200, r#"{"items":[{"name":"plain.txt"}]}"#.to_string()),
    ]);
    let objects = storage(&base).list_objects("bucket").unwrap();
    server.join().unwrap();

    assert_eq!(
        objects,
        vec![
            "gs://bucket/folder*?[]#/é.txt".to_string(),
            "gs://bucket/plain.txt".to_string(),
        ]
    );
    let requests = requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert_eq!(
        request_line(&requests[0]),
        "GET /storage/v1/b/bucket/o?maxResults=1000 HTTP/1.1"
    );
    assert_eq!(
        request_line(&requests[1]),
        "GET /storage/v1/b/bucket/o?maxResults=1000&pageToken=next HTTP/1.1"
    );
    assert!(
        requests[0]
            .to_ascii_lowercase()
            .contains("authorization: bearer token")
    );
}

#[test]
fn reports_api_error_messages() {
    let (base, requests, server) = test_server(vec![(
        403,
        r#"{"error":{"message":"permission denied"}}"#.to_string(),
    )]);

    let error = storage(&base).list_objects("bucket").unwrap_err();
    server.join().unwrap();

    assert!(error.to_string().contains("permission denied"));
    let requests = requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(
        request_line(&requests[0]),
        "GET /storage/v1/b/bucket/o?maxResults=1000 HTTP/1.1"
    );
}

#[test]
fn uploads_files_and_preserves_generation_preconditions() {
    let (base, requests, server) = test_server(vec![(200, r#"{"generation":"789"}"#.to_string())]);
    let source = NamedTempFile::new().unwrap();
    std::fs::write(source.path(), b"contents").unwrap();
    let target = ObjectPath::parse("gs://bucket/target.txt").unwrap();

    let generation = storage(&base).upload_file(source.path(), &target).unwrap();
    server.join().unwrap();

    assert_eq!(generation, "789");
    let requests = requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(
        request_line(&requests[0]),
        "POST /storage/v1/b/bucket/o?uploadType=media&name=target.txt&ifGenerationMatch=0 HTTP/1.1"
    );
    let request = requests[0].to_ascii_lowercase();
    assert!(request.contains("uploadtype=media"));
    assert!(request.contains("name=target.txt"));
    assert!(request.contains("ifgenerationmatch=0"));
    assert!(request.contains("content-length: 8"));
    assert!(request.ends_with("\r\n\r\ncontents"));
}

#[cfg(unix)]
#[test]
fn refuses_symlinked_upload_sources() {
    use std::os::unix::fs::symlink;

    let directory = tempdir().unwrap();
    let source = NamedTempFile::new().unwrap();
    let linked_file = directory.path().join("linked.txt");
    symlink(source.path(), &linked_file).unwrap();
    let target = ObjectPath::parse("gs://bucket/target.txt").unwrap();
    let storage = StorageApi::with_endpoints(
        Cloud::new(),
        "http://127.0.0.1:1/storage/v1",
        "http://127.0.0.1:1/storage/v1",
        Some("token".to_string()),
    );

    let error = storage.upload_file(&linked_file, &target).unwrap_err();

    assert!(matches!(error, AppError::UploadSource(_)), "{error}");
}

#[test]
fn does_not_confirm_operations_that_never_reached_storage() {
    // A dead endpoint proves no request is attempted: any lookup would fail.
    let storage = StorageApi::with_endpoints(
        Cloud::new(),
        "http://127.0.0.1:1/storage/v1",
        "http://127.0.0.1:1/storage/v1",
        Some("token".to_string()),
    );
    let source = ObjectPath::parse("gs://bucket/source").unwrap();
    let target = ObjectPath::parse("gs://bucket/target").unwrap();
    let unreadable_source = AppError::UploadSource(std::io::Error::new(
        std::io::ErrorKind::NotFound,
        "not a regular file",
    ));
    let unreachable_cloud = AppError::Command {
        operation: "Cloud command".to_string(),
        status: "255".to_string(),
        details: ": ssh: connect to host googlecloud port 22: Connection refused".to_string(),
    };

    storage
        .confirm_write_after_failure(&target, &unreadable_source)
        .unwrap();
    storage
        .confirm_write_after_failure(&target, &unreachable_cloud)
        .unwrap();
    storage
        .confirm_move_after_failure(&source, &target, &unreachable_cloud)
        .unwrap();
}

#[test]
fn reports_upload_server_errors() {
    let (base, requests, server) = test_server(vec![(
        500,
        r#"{"error":{"message":"backend unavailable"}}"#.to_string(),
    )]);
    let source = NamedTempFile::new().unwrap();
    std::fs::write(source.path(), b"contents").unwrap();
    let target = ObjectPath::parse("gs://bucket/target.txt").unwrap();

    let error = storage(&base)
        .upload_file(source.path(), &target)
        .unwrap_err();
    server.join().unwrap();

    assert!(matches!(
        error,
        AppError::Storage {
            status: 500,
            message: _
        }
    ));
    assert!(error.to_string().contains("backend unavailable"));
    let requests = requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(
        request_line(&requests[0]),
        "POST /storage/v1/b/bucket/o?uploadType=media&name=target.txt&ifGenerationMatch=0 HTTP/1.1"
    );
}

#[test]
fn moves_objects_and_confirms_source_deletion() {
    let (base, requests, server) = test_server(vec![
        (200, r#"{"generation":"11"}"#.to_string()),
        (
            200,
            r#"{"done":true,"resource":{"generation":"22"}}"#.to_string(),
        ),
        (200, "{}".to_string()),
        (404, "{}".to_string()),
    ]);
    let storage = storage(&base);
    let source = ObjectPath::parse("gs://bucket/source").unwrap();
    let target = ObjectPath::parse("gs://bucket/target").unwrap();

    let generation = storage.move_object(&source, &target).unwrap();
    server.join().unwrap();

    assert_eq!(generation, "22");
    let requests = requests.lock().unwrap();
    assert_eq!(requests.len(), 4);
    assert_eq!(
        request_line(&requests[0]),
        "GET /storage/v1/b/bucket/o/source HTTP/1.1"
    );
    assert_eq!(
        request_line(&requests[1]),
        "POST /storage/v1/b/bucket/o/source/rewriteTo/b/bucket/o/target?sourceGeneration=11&ifSourceGenerationMatch=11&ifGenerationMatch=0 HTTP/1.1"
    );
    assert_eq!(
        request_line(&requests[2]),
        "DELETE /storage/v1/b/bucket/o/source?generation=11 HTTP/1.1"
    );
    assert_eq!(
        request_line(&requests[3]),
        "GET /storage/v1/b/bucket/o/source HTTP/1.1"
    );
}

#[test]
fn rejects_a_move_when_the_source_remains_after_delete() {
    let (base, requests, server) = test_server(vec![
        (200, r#"{"generation":"11"}"#.to_string()),
        (
            200,
            r#"{"done":true,"resource":{"generation":"22"}}"#.to_string(),
        ),
        (200, "{}".to_string()),
        (200, r#"{"generation":"11"}"#.to_string()),
    ]);
    let source = ObjectPath::parse("gs://bucket/source").unwrap();
    let target = ObjectPath::parse("gs://bucket/target").unwrap();

    let error = storage(&base).move_object(&source, &target).unwrap_err();
    server.join().unwrap();

    assert!(error.to_string().contains("Source object remains"));
    let requests = requests.lock().unwrap();
    assert_eq!(requests.len(), 4);
    assert_eq!(
        request_line(&requests[0]),
        "GET /storage/v1/b/bucket/o/source HTTP/1.1"
    );
    assert_eq!(
        request_line(&requests[1]),
        "POST /storage/v1/b/bucket/o/source/rewriteTo/b/bucket/o/target?sourceGeneration=11&ifSourceGenerationMatch=11&ifGenerationMatch=0 HTTP/1.1"
    );
    assert_eq!(
        request_line(&requests[2]),
        "DELETE /storage/v1/b/bucket/o/source?generation=11 HTTP/1.1"
    );
    assert_eq!(
        request_line(&requests[3]),
        "GET /storage/v1/b/bucket/o/source HTTP/1.1"
    );
}

#[test]
fn rolls_back_objects_and_confirms_target_deletion() {
    let (base, requests, server) = test_server(vec![
        (404, "{}".to_string()),
        (200, r#"{"generation":"22"}"#.to_string()),
        (
            200,
            r#"{"done":true,"resource":{"generation":"33"}}"#.to_string(),
        ),
        (200, "{}".to_string()),
        (404, "{}".to_string()),
    ]);
    let storage = storage(&base);
    let source = ObjectPath::parse("gs://bucket/source").unwrap();
    let target = ObjectPath::parse("gs://bucket/target").unwrap();

    let generation = storage.rollback_object(&source, &target, "22").unwrap();
    server.join().unwrap();

    assert_eq!(generation, "33");
    let requests = requests.lock().unwrap();
    assert_eq!(requests.len(), 5);
    assert_eq!(
        request_line(&requests[0]),
        "GET /storage/v1/b/bucket/o/source HTTP/1.1"
    );
    assert_eq!(
        request_line(&requests[1]),
        "GET /storage/v1/b/bucket/o/target?generation=22 HTTP/1.1"
    );
    assert_eq!(
        request_line(&requests[2]),
        "POST /storage/v1/b/bucket/o/target/rewriteTo/b/bucket/o/source?sourceGeneration=22&ifSourceGenerationMatch=22&ifGenerationMatch=0 HTTP/1.1"
    );
    assert_eq!(
        request_line(&requests[3]),
        "DELETE /storage/v1/b/bucket/o/target?generation=22 HTTP/1.1"
    );
    assert_eq!(
        request_line(&requests[4]),
        "GET /storage/v1/b/bucket/o/target HTTP/1.1"
    );
}

#[test]
fn rejects_a_rollback_when_the_target_remains_after_delete() {
    let (base, requests, server) = test_server(vec![
        (404, "{}".to_string()),
        (200, r#"{"generation":"22"}"#.to_string()),
        (
            200,
            r#"{"done":true,"resource":{"generation":"33"}}"#.to_string(),
        ),
        (200, "{}".to_string()),
        (200, r#"{"generation":"22"}"#.to_string()),
    ]);
    let source = ObjectPath::parse("gs://bucket/source").unwrap();
    let target = ObjectPath::parse("gs://bucket/target").unwrap();

    let error = storage(&base)
        .rollback_object(&source, &target, "22")
        .unwrap_err();
    server.join().unwrap();

    assert!(error.to_string().contains("Rollback target remains"));
    let requests = requests.lock().unwrap();
    assert_eq!(requests.len(), 5);
    assert_eq!(
        request_line(&requests[0]),
        "GET /storage/v1/b/bucket/o/source HTTP/1.1"
    );
    assert_eq!(
        request_line(&requests[1]),
        "GET /storage/v1/b/bucket/o/target?generation=22 HTTP/1.1"
    );
    assert_eq!(
        request_line(&requests[2]),
        "POST /storage/v1/b/bucket/o/target/rewriteTo/b/bucket/o/source?sourceGeneration=22&ifSourceGenerationMatch=22&ifGenerationMatch=0 HTTP/1.1"
    );
    assert_eq!(
        request_line(&requests[3]),
        "DELETE /storage/v1/b/bucket/o/target?generation=22 HTTP/1.1"
    );
    assert_eq!(
        request_line(&requests[4]),
        "GET /storage/v1/b/bucket/o/target HTTP/1.1"
    );
}

#[test]
fn cleans_up_owned_objects_and_accepts_missing_objects() {
    let (base, requests, server) = test_server(vec![
        (200, r#"{"generation":"22"}"#.to_string()),
        (200, "{}".to_string()),
        (404, "{}".to_string()),
    ]);
    let api = storage(&base);
    let target = ObjectPath::parse("gs://bucket/target").unwrap();

    api.cleanup_object(&target, "22").unwrap();
    server.join().unwrap();

    let requests = requests.lock().unwrap();
    assert_eq!(requests.len(), 3);
    assert_eq!(
        request_line(&requests[0]),
        "GET /storage/v1/b/bucket/o/target?generation=22 HTTP/1.1"
    );
    assert_eq!(
        request_line(&requests[1]),
        "DELETE /storage/v1/b/bucket/o/target?generation=22 HTTP/1.1"
    );
    assert_eq!(
        request_line(&requests[2]),
        "GET /storage/v1/b/bucket/o/target HTTP/1.1"
    );

    let (base, requests, server) =
        test_server(vec![(404, "{}".to_string()), (404, "{}".to_string())]);
    storage(&base).cleanup_object(&target, "22").unwrap();
    server.join().unwrap();
    let requests = requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert_eq!(
        request_line(&requests[0]),
        "GET /storage/v1/b/bucket/o/target?generation=22 HTTP/1.1"
    );
    assert_eq!(
        request_line(&requests[1]),
        "GET /storage/v1/b/bucket/o/target HTTP/1.1"
    );
}

#[test]
fn rejects_cleanup_when_the_target_remains_after_delete() {
    let (base, requests, server) = test_server(vec![
        (200, r#"{"generation":"22"}"#.to_string()),
        (200, "{}".to_string()),
        (200, r#"{"generation":"22"}"#.to_string()),
    ]);
    let target = ObjectPath::parse("gs://bucket/target").unwrap();

    let error = storage(&base).cleanup_object(&target, "22").unwrap_err();
    server.join().unwrap();

    assert!(error.to_string().contains("Cleanup target remains"));
    let requests = requests.lock().unwrap();
    assert_eq!(requests.len(), 3);
    assert_eq!(
        request_line(&requests[0]),
        "GET /storage/v1/b/bucket/o/target?generation=22 HTTP/1.1"
    );
    assert_eq!(
        request_line(&requests[1]),
        "DELETE /storage/v1/b/bucket/o/target?generation=22 HTTP/1.1"
    );
    assert_eq!(
        request_line(&requests[2]),
        "GET /storage/v1/b/bucket/o/target HTTP/1.1"
    );
}

#[test]
fn confirms_missing_objects_after_non_http_failures() {
    let (base, requests, server) = test_server(vec![
        (200, r#"{"generation":"11"}"#.to_string()),
        (404, "{}".to_string()),
        (404, "{}".to_string()),
    ]);
    let storage = storage(&base);
    let source = ObjectPath::parse("gs://bucket/source").unwrap();
    let target = ObjectPath::parse("gs://bucket/target").unwrap();

    storage
        .confirm_move_after_failure(
            &source,
            &target,
            &AppError::Message("move interrupted".to_string()),
        )
        .unwrap();
    storage
        .confirm_write_after_failure(
            &target,
            &AppError::Message("upload interrupted".to_string()),
        )
        .unwrap();
    server.join().unwrap();

    let requests = requests.lock().unwrap();
    assert_eq!(requests.len(), 3);
    assert_eq!(
        request_line(&requests[0]),
        "GET /storage/v1/b/bucket/o/source HTTP/1.1"
    );
    assert_eq!(
        request_line(&requests[1]),
        "GET /storage/v1/b/bucket/o/target HTTP/1.1"
    );
    assert_eq!(
        request_line(&requests[2]),
        "GET /storage/v1/b/bucket/o/target HTTP/1.1"
    );
}

#[test]
fn requires_recovery_for_http_failures_with_unknown_state() {
    let (base, requests, server) = test_server(vec![
        (200, r#"{"generation":"11"}"#.to_string()),
        (404, "{}".to_string()),
    ]);
    let source = ObjectPath::parse("gs://bucket/source").unwrap();
    let target = ObjectPath::parse("gs://bucket/target").unwrap();

    let error = storage(&base)
        .confirm_move_after_failure(
            &source,
            &target,
            &AppError::Storage {
                status: 503,
                message: "service unavailable".to_string(),
            },
        )
        .unwrap_err();
    server.join().unwrap();

    assert!(matches!(error, AppError::Recovery { .. }));
    let requests = requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert_eq!(
        request_line(&requests[0]),
        "GET /storage/v1/b/bucket/o/source HTTP/1.1"
    );
    assert_eq!(
        request_line(&requests[1]),
        "GET /storage/v1/b/bucket/o/target HTTP/1.1"
    );
}

#[test]
fn requires_recovery_for_http_upload_failures_with_existing_state() {
    let (base, requests, server) = test_server(vec![(200, r#"{"generation":"44"}"#.to_string())]);
    let target = ObjectPath::parse("gs://bucket/target").unwrap();

    let error = storage(&base)
        .confirm_write_after_failure(
            &target,
            &AppError::Storage {
                status: 503,
                message: "service unavailable".to_string(),
            },
        )
        .unwrap_err();
    server.join().unwrap();

    assert!(matches!(error, AppError::Recovery { .. }));
    let requests = requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(
        request_line(&requests[0]),
        "GET /storage/v1/b/bucket/o/target HTTP/1.1"
    );
}

#[test]
fn requires_recovery_when_state_confirmation_gets_a_server_error() {
    let (base, requests, server) = test_server(vec![(
        503,
        r#"{"error":{"message":"service unavailable"}}"#.to_string(),
    )]);
    let target = ObjectPath::parse("gs://bucket/target").unwrap();

    let error = storage(&base)
        .confirm_write_after_failure(&target, &AppError::Message("upload failed".to_string()))
        .unwrap_err();
    server.join().unwrap();

    assert!(matches!(error, AppError::Recovery { .. }));
    assert!(error.to_string().contains("state unknown"));
    assert!(error.to_string().contains("service unavailable"));
    let requests = requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(
        request_line(&requests[0]),
        "GET /storage/v1/b/bucket/o/target HTTP/1.1"
    );
}

#[test]
fn reports_an_incomplete_http_response() {
    let (base, requests, server) = incomplete_response_server();

    let error = storage(&base).list_objects("bucket").unwrap_err();
    server.join().unwrap();

    assert!(matches!(error, AppError::Http(_)));
    let requests = requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(
        request_line(&requests[0]),
        "GET /storage/v1/b/bucket/o?maxResults=1000 HTTP/1.1"
    );
}

#[test]
fn rewrites_objects_with_generations_and_continuation_tokens() {
    let (base, requests, server) = test_server(vec![
        (
            200,
            r#"{"done":false,"rewriteToken":"continue token"}"#.to_string(),
        ),
        (
            200,
            r#"{"done":true,"resource":{"generation":"456"}}"#.to_string(),
        ),
    ]);
    let storage = storage(&base);
    let source = ObjectPath::parse("gs://bucket/folder*?[]#/source").unwrap();
    let target = ObjectPath::parse("gs://bucket/folder*?[]#/target").unwrap();

    let generation = storage
        .copy_object(&source, &target, Some("123"), Some("0"))
        .unwrap();
    server.join().unwrap();

    assert_eq!(generation, "456");
    let requests = requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert_eq!(
        request_line(&requests[0]),
        "POST /storage/v1/b/bucket/o/folder%2A%3F%5B%5D%23%2Fsource/rewriteTo/b/bucket/o/folder%2A%3F%5B%5D%23%2Ftarget?sourceGeneration=123&ifSourceGenerationMatch=123&ifGenerationMatch=0 HTTP/1.1"
    );
    assert_eq!(
        request_line(&requests[1]),
        "POST /storage/v1/b/bucket/o/folder%2A%3F%5B%5D%23%2Fsource/rewriteTo/b/bucket/o/folder%2A%3F%5B%5D%23%2Ftarget?sourceGeneration=123&ifSourceGenerationMatch=123&ifGenerationMatch=0&rewriteToken=continue+token HTTP/1.1"
    );
}
