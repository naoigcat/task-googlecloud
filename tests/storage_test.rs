use std::env;
use std::ffi::OsString;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex, atomic::AtomicBool};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use task_googlecloud::{
    AppError, Cloud, InterruptFlag, ObjectPath, StorageApi, StorageClient, process_moves,
};
use tempfile::{NamedTempFile, TempDir, tempdir};

const SERVER_TIMEOUT: Duration = Duration::from_secs(5);
const EARLY_CLOSE_TIMEOUT: Duration = Duration::from_millis(100);

fn test_server(responses: Vec<(u16, String)>) -> (String, Arc<Mutex<Vec<String>>>, JoinHandle<()>) {
    response_server(responses, SERVER_TIMEOUT)
}

fn test_server_allowing_early_close(
    responses: Vec<(u16, String)>,
) -> (String, Arc<Mutex<Vec<String>>>, JoinHandle<()>) {
    response_server(responses, EARLY_CLOSE_TIMEOUT)
}

fn response_server(
    responses: Vec<(u16, String)>,
    accept_timeout: Duration,
) -> (String, Arc<Mutex<Vec<String>>>, JoinHandle<()>) {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    listener.set_nonblocking(true).unwrap();
    let address = format!("http://{}/storage/v1", listener.local_addr().unwrap());
    let requests = Arc::new(Mutex::new(Vec::new()));
    let recorded_requests = Arc::clone(&requests);
    let handle = thread::spawn(move || {
        for (status, body) in responses {
            let Some(mut stream) = accept_connection(&listener, accept_timeout) else {
                break;
            };
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
        let mut stream = accept_connection(&listener, SERVER_TIMEOUT).unwrap();
        recorded_requests
            .lock()
            .unwrap()
            .push(read_request(&mut stream));
        stream
            .write_all(
                b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 21\r\nConnection: close\r\n\r\n{\"generation\":\"lock\"}",
            )
            .unwrap();

        let mut stream = accept_connection(&listener, SERVER_TIMEOUT).unwrap();
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

fn accept_connection(listener: &TcpListener, timeout: Duration) -> Option<TcpStream> {
    let deadline = Instant::now() + timeout;
    loop {
        match listener.accept() {
            Ok((stream, _)) => {
                stream.set_read_timeout(Some(SERVER_TIMEOUT)).unwrap();
                stream.set_write_timeout(Some(SERVER_TIMEOUT)).unwrap();
                return Some(stream);
            }
            Err(error)
                if error.kind() == std::io::ErrorKind::WouldBlock && Instant::now() < deadline =>
            {
                thread::sleep(Duration::from_millis(10));
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => return None,
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

fn bucket_lock_created() -> (u16, String) {
    (200, r#"{"generation":"lock"}"#.to_string())
}

fn bucket_lock_deleted() -> (u16, String) {
    (200, "{}".to_string())
}

#[cfg(unix)]
static TOKEN_ENVIRONMENT_LOCK: Mutex<()> = Mutex::new(());

#[cfg(unix)]
struct TokenEnvironment {
    _directory: TempDir,
    old_path: Option<OsString>,
    old_counter: Option<OsString>,
    old_failure: Option<OsString>,
}

#[cfg(unix)]
impl TokenEnvironment {
    fn new(failure_attempt: usize) -> Self {
        use std::os::unix::fs::PermissionsExt;

        const COUNTER: &str = "TASK_GOOGLECLOUD_TEST_TOKEN_COUNTER";
        const FAILURE: &str = "TASK_GOOGLECLOUD_TEST_TOKEN_FAILURE_ATTEMPT";
        let directory = tempdir().unwrap();
        let counter = directory.path().join("token-attempt");
        std::fs::write(&counter, "0").unwrap();
        let ssh = directory.path().join("ssh");
        std::fs::write(
            &ssh,
            format!(
                "#!/bin/sh\nset -eu\nattempt=$(cat \"${COUNTER}\")\nattempt=$((attempt + 1))\nprintf '%s' \"$attempt\" > \"${COUNTER}\"\nif [ \"$attempt\" -eq \"${FAILURE}\" ]; then exit 1; fi\nprintf 'token\\n'\n"
            ),
        )
        .unwrap();
        let mut permissions = std::fs::metadata(&ssh).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&ssh, permissions).unwrap();

        let old_path = env::var_os("PATH");
        let old_counter = env::var_os(COUNTER);
        let old_failure = env::var_os(FAILURE);
        let mut path = OsString::from(directory.path());
        path.push(":");
        if let Some(old_path) = &old_path {
            path.push(old_path);
        }
        // The production client invokes SSH through PATH; isolate a deterministic
        // token failure here without changing the authentication implementation.
        unsafe {
            env::set_var("PATH", path);
            env::set_var(COUNTER, counter);
            env::set_var(FAILURE, failure_attempt.to_string());
        }

        Self {
            _directory: directory,
            old_path,
            old_counter,
            old_failure,
        }
    }
}

#[cfg(unix)]
impl Drop for TokenEnvironment {
    fn drop(&mut self) {
        const COUNTER: &str = "TASK_GOOGLECLOUD_TEST_TOKEN_COUNTER";
        const FAILURE: &str = "TASK_GOOGLECLOUD_TEST_TOKEN_FAILURE_ATTEMPT";

        unsafe {
            match self.old_path.take() {
                Some(value) => env::set_var("PATH", value),
                None => env::remove_var("PATH"),
            }
            match self.old_counter.take() {
                Some(value) => env::set_var(COUNTER, value),
                None => env::remove_var(COUNTER),
            }
            match self.old_failure.take() {
                Some(value) => env::set_var(FAILURE, value),
                None => env::remove_var(FAILURE),
            }
        }
    }
}

#[cfg(unix)]
fn move_with_token_failure(
    failure_attempt: usize,
    responses: Vec<(u16, String)>,
) -> (AppError, Vec<String>) {
    let _environment_lock = TOKEN_ENVIRONMENT_LOCK.lock().unwrap();
    let _token_environment = TokenEnvironment::new(failure_attempt);
    let (base, requests, server) = test_server(responses);
    let storage = StorageApi::with_endpoints(Cloud::new(), base.clone(), base, None);
    let source = ObjectPath::parse("gs://bucket/source").unwrap();
    let target = ObjectPath::parse("gs://bucket/target").unwrap();

    let error = storage.move_object(&source, &target, None).unwrap_err();
    server.join().unwrap();

    (error, requests.lock().unwrap().clone())
}

#[cfg(unix)]
fn rollback_with_token_failure(
    failure_attempt: usize,
    responses: Vec<(u16, String)>,
) -> (AppError, Vec<String>) {
    let _environment_lock = TOKEN_ENVIRONMENT_LOCK.lock().unwrap();
    let _token_environment = TokenEnvironment::new(failure_attempt);
    let (base, requests, server) = test_server(responses);
    let storage = StorageApi::with_endpoints(Cloud::new(), base.clone(), base, None);
    let source = ObjectPath::parse("gs://bucket/source").unwrap();
    let target = ObjectPath::parse("gs://bucket/target").unwrap();

    let error = storage.rollback_object(&source, &target, "22").unwrap_err();
    server.join().unwrap();

    (error, requests.lock().unwrap().clone())
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
    let (base, requests, server) = test_server(vec![
        bucket_lock_created(),
        (
            200,
            r#"{"done":true,"resource":{"generation":"456"}}"#.to_string(),
        ),
        bucket_lock_deleted(),
    ]);
    let storage = storage(&base);
    let source = ObjectPath::parse("gs://bucket/folder*?[]#/source").unwrap();
    let target = ObjectPath::parse("gs://bucket/folder*?[]#/target").unwrap();

    storage.copy_object(&source, &target, None, None).unwrap();
    server.join().unwrap();

    let requests = requests.lock().unwrap();
    assert_eq!(requests.len(), 3);
    assert_eq!(
        request_line(&requests[1]),
        "POST /storage/v1/b/bucket/o/folder%2A%3F%5B%5D%23%2Fsource/rewriteTo/b/bucket/o/folder%2A%3F%5B%5D%23%2Ftarget HTTP/1.1"
    );
}

#[test]
fn keeps_leading_hyphens_in_object_names() {
    let (base, requests, server) = test_server(vec![
        bucket_lock_created(),
        (
            200,
            r#"{"done":true,"resource":{"generation":"456"}}"#.to_string(),
        ),
        bucket_lock_deleted(),
    ]);
    let storage = storage(&base);
    let source = ObjectPath::parse("gs://bucket/source").unwrap();
    let target = ObjectPath::parse("gs://bucket/-target?").unwrap();

    storage
        .copy_object(&source, &target, Some("123"), Some("0"))
        .unwrap();
    server.join().unwrap();

    let requests = requests.lock().unwrap();
    assert_eq!(requests.len(), 3);
    assert_eq!(
        request_line(&requests[1]),
        "POST /storage/v1/b/bucket/o/source/rewriteTo/b/bucket/o/-target%3F?sourceGeneration=123&ifSourceGenerationMatch=123&ifGenerationMatch=0 HTTP/1.1"
    );
}

#[test]
fn refuses_to_modify_the_reserved_bucket_lock_object() {
    let storage = StorageApi::with_endpoints(
        Cloud::new(),
        "http://127.0.0.1:1/storage/v1",
        "http://127.0.0.1:1/storage/v1",
        Some("token".to_string()),
    );
    let source = ObjectPath::parse("gs://bucket/source").unwrap();
    let lock = ObjectPath::parse("gs://bucket/.task-googlecloud-lock").unwrap();

    let error = storage.copy_object(&source, &lock, None, None).unwrap_err();

    assert!(
        matches!(error, AppError::Message(ref message) if message.contains("reserved bucket lock")),
        "{error}"
    );
}

#[test]
fn lists_empty_and_paginated_responses() {
    let (base, requests, server) = test_server(vec![
        bucket_lock_created(),
        (200, "{}".to_string()),
        bucket_lock_deleted(),
    ]);
    let objects = storage(&base).list_objects("bucket").unwrap();
    server.join().unwrap();
    assert!(objects.is_empty());
    let requests = requests.lock().unwrap();
    assert_eq!(requests.len(), 3);
    assert_eq!(
        request_line(&requests[1]),
        "GET /storage/v1/b/bucket/o?maxResults=1000 HTTP/1.1"
    );

    let (base, requests, server) = test_server(vec![
        bucket_lock_created(),
        (
            200,
            r#"{"items":[{"name":"folder*?[]#/é.txt"},{"name":".task-googlecloud-lock"}],"nextPageToken":"next"}"#.to_string(),
        ),
        (200, r#"{"items":[{"name":"plain.txt"}]}"#.to_string()),
        bucket_lock_deleted(),
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
    assert_eq!(requests.len(), 4);
    assert_eq!(
        request_line(&requests[1]),
        "GET /storage/v1/b/bucket/o?maxResults=1000 HTTP/1.1"
    );
    assert_eq!(
        request_line(&requests[2]),
        "GET /storage/v1/b/bucket/o?maxResults=1000&pageToken=next HTTP/1.1"
    );
    assert!(
        requests[1]
            .to_ascii_lowercase()
            .contains("authorization: bearer token")
    );
}

#[test]
fn reports_api_error_messages() {
    let (base, requests, server) = test_server(vec![
        bucket_lock_created(),
        (
            403,
            r#"{"error":{"message":"permission denied"}}"#.to_string(),
        ),
        bucket_lock_deleted(),
    ]);

    let error = storage(&base).list_objects("bucket").unwrap_err();
    server.join().unwrap();

    assert!(error.to_string().contains("permission denied"));
    let requests = requests.lock().unwrap();
    assert_eq!(requests.len(), 3);
    assert_eq!(
        request_line(&requests[1]),
        "GET /storage/v1/b/bucket/o?maxResults=1000 HTTP/1.1"
    );
}

#[test]
fn uploads_files_and_preserves_generation_preconditions() {
    let (base, requests, server) = test_server(vec![
        bucket_lock_created(),
        (200, r#"{"generation":"789"}"#.to_string()),
        bucket_lock_deleted(),
    ]);
    let source = NamedTempFile::new().unwrap();
    std::fs::write(source.path(), b"contents").unwrap();
    let target = ObjectPath::parse("gs://bucket/target.txt").unwrap();

    let generation = storage(&base).upload_file(source.path(), &target).unwrap();
    server.join().unwrap();

    assert_eq!(generation, "789");
    let requests = requests.lock().unwrap();
    assert_eq!(requests.len(), 3);
    assert_eq!(
        request_line(&requests[1]),
        "POST /storage/v1/b/bucket/o?uploadType=media&name=target.txt&ifGenerationMatch=0 HTTP/1.1"
    );
    let request = requests[1].to_ascii_lowercase();
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
    let token_io = AppError::Token(
        Box::new(AppError::Io(std::io::Error::new(
            std::io::ErrorKind::BrokenPipe,
            "token pipe closed",
        ))),
        false,
    );
    let token_utf8 = AppError::Token(
        Box::new(AppError::Message(
            "Cloud command returned invalid UTF-8".to_string(),
        )),
        false,
    );

    storage
        .confirm_write_after_failure(&target, &unreadable_source)
        .unwrap();
    storage
        .confirm_write_after_failure(&target, &unreachable_cloud)
        .unwrap();
    storage
        .confirm_move_after_failure(&source, &target, &unreachable_cloud)
        .unwrap();
    storage
        .confirm_write_after_failure(&target, &token_io)
        .unwrap();
    storage
        .confirm_write_after_failure(&target, &token_utf8)
        .unwrap();
    storage
        .confirm_move_after_failure(&source, &target, &token_io)
        .unwrap();
    storage
        .confirm_move_after_failure(&source, &target, &token_utf8)
        .unwrap();
}

#[test]
fn confirms_state_after_token_failure_following_a_remote_request() {
    let (base, requests, server) = test_server(vec![
        (200, r#"{"generation":"11"}"#.to_string()),
        (404, "{}".to_string()),
    ]);
    let storage = storage(&base);
    let source = ObjectPath::parse("gs://bucket/source").unwrap();
    let target = ObjectPath::parse("gs://bucket/target").unwrap();
    let operation = AppError::Token(
        Box::new(AppError::Io(std::io::Error::new(
            std::io::ErrorKind::BrokenPipe,
            "token pipe closed",
        ))),
        true,
    );

    let error = storage
        .confirm_move_after_failure(&source, &target, &operation)
        .unwrap_err();
    server.join().unwrap();

    assert!(matches!(error, AppError::Recovery { .. }));
    let requests = requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
}

#[test]
fn reports_upload_server_errors() {
    let (base, requests, server) = test_server(vec![
        bucket_lock_created(),
        (
            500,
            r#"{"error":{"message":"backend unavailable"}}"#.to_string(),
        ),
        bucket_lock_deleted(),
    ]);
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
    assert_eq!(requests.len(), 3);
    assert_eq!(
        request_line(&requests[1]),
        "POST /storage/v1/b/bucket/o?uploadType=media&name=target.txt&ifGenerationMatch=0 HTTP/1.1"
    );
}

#[test]
fn moves_objects_and_confirms_source_deletion() {
    let (base, requests, server) = test_server(vec![
        bucket_lock_created(),
        (200, r#"{"generation":"11"}"#.to_string()),
        (
            200,
            r#"{"done":true,"resource":{"generation":"22"}}"#.to_string(),
        ),
        (200, r#"{"generation":"22"}"#.to_string()),
        (200, "{}".to_string()),
        (404, "{}".to_string()),
        (200, r#"{"generation":"22"}"#.to_string()),
        bucket_lock_deleted(),
    ]);
    let storage = storage(&base);
    let source = ObjectPath::parse("gs://bucket/source").unwrap();
    let target = ObjectPath::parse("gs://bucket/target").unwrap();

    let generation = storage.move_object(&source, &target, None).unwrap();
    server.join().unwrap();

    assert_eq!(generation, "22");
    let requests = requests.lock().unwrap();
    assert_eq!(requests.len(), 8);
    assert_eq!(
        request_line(&requests[1]),
        "GET /storage/v1/b/bucket/o/source HTTP/1.1"
    );
    assert_eq!(
        request_line(&requests[2]),
        "POST /storage/v1/b/bucket/o/source/rewriteTo/b/bucket/o/target?sourceGeneration=11&ifSourceGenerationMatch=11&ifGenerationMatch=0 HTTP/1.1"
    );
    assert_eq!(
        request_line(&requests[3]),
        "GET /storage/v1/b/bucket/o/target HTTP/1.1"
    );
    assert_eq!(
        request_line(&requests[4]),
        "DELETE /storage/v1/b/bucket/o/source?generation=11 HTTP/1.1"
    );
    assert_eq!(
        request_line(&requests[5]),
        "GET /storage/v1/b/bucket/o/source HTTP/1.1"
    );
    assert_eq!(
        request_line(&requests[6]),
        "GET /storage/v1/b/bucket/o/target HTTP/1.1"
    );
    assert_eq!(
        request_line(&requests[7]),
        "DELETE /storage/v1/b/bucket/o/.task-googlecloud-lock?generation=lock HTTP/1.1"
    );
}

#[test]
fn moves_objects_using_the_expected_source_generation() {
    let (base, requests, server) = test_server(vec![
        bucket_lock_created(),
        (
            200,
            r#"{"done":true,"resource":{"generation":"22"}}"#.to_string(),
        ),
        (200, r#"{"generation":"22"}"#.to_string()),
        (200, "{}".to_string()),
        (404, "{}".to_string()),
        (200, r#"{"generation":"22"}"#.to_string()),
        bucket_lock_deleted(),
    ]);
    let storage = storage(&base);
    let source = ObjectPath::parse("gs://bucket/source").unwrap();
    let target = ObjectPath::parse("gs://bucket/target").unwrap();

    let generation = storage.move_object(&source, &target, Some("11")).unwrap();
    server.join().unwrap();

    assert_eq!(generation, "22");
    let requests = requests.lock().unwrap();
    assert_eq!(requests.len(), 7);
    assert_eq!(
        request_line(&requests[1]),
        "POST /storage/v1/b/bucket/o/source/rewriteTo/b/bucket/o/target?sourceGeneration=11&ifSourceGenerationMatch=11&ifGenerationMatch=0 HTTP/1.1"
    );
    assert_eq!(
        request_line(&requests[2]),
        "GET /storage/v1/b/bucket/o/target HTTP/1.1"
    );
    assert_eq!(
        request_line(&requests[3]),
        "DELETE /storage/v1/b/bucket/o/source?generation=11 HTTP/1.1"
    );
    assert_eq!(
        request_line(&requests[4]),
        "GET /storage/v1/b/bucket/o/source HTTP/1.1"
    );
    assert_eq!(
        request_line(&requests[5]),
        "GET /storage/v1/b/bucket/o/target HTTP/1.1"
    );
    assert_eq!(
        request_line(&requests[6]),
        "DELETE /storage/v1/b/bucket/o/.task-googlecloud-lock?generation=lock HTTP/1.1"
    );
}

#[test]
fn keeps_one_bucket_lock_for_all_moves_in_a_transaction() {
    let mut responses = vec![bucket_lock_created()];
    for (source_generation, staged_generation) in [("11", "12"), ("21", "22")] {
        responses.extend([
            (200, format!(r#"{{"generation":"{source_generation}"}}"#)),
            (
                200,
                format!(r#"{{"done":true,"resource":{{"generation":"{staged_generation}"}}}}"#),
            ),
            (200, format!(r#"{{"generation":"{staged_generation}"}}"#)),
            (200, "{}".to_string()),
            (404, "{}".to_string()),
            (200, format!(r#"{{"generation":"{staged_generation}"}}"#)),
        ]);
    }
    for finalized_generation in ["13", "23"] {
        responses.extend([
            (
                200,
                format!(r#"{{"done":true,"resource":{{"generation":"{finalized_generation}"}}}}"#),
            ),
            (200, format!(r#"{{"generation":"{finalized_generation}"}}"#)),
            (200, "{}".to_string()),
            (404, "{}".to_string()),
            (200, format!(r#"{{"generation":"{finalized_generation}"}}"#)),
        ]);
    }
    responses.push(bucket_lock_deleted());

    let (base, requests, server) = test_server(responses);
    let storage = storage(&base);
    let interrupt = InterruptFlag::from_atomic(Arc::new(AtomicBool::new(false)));
    let moves = vec![
        (
            ObjectPath::parse("gs://bucket/first-source").unwrap(),
            ObjectPath::parse("gs://bucket/first-target").unwrap(),
            ObjectPath::parse("gs://bucket/first-temporary").unwrap(),
        ),
        (
            ObjectPath::parse("gs://bucket/second-source").unwrap(),
            ObjectPath::parse("gs://bucket/second-target").unwrap(),
            ObjectPath::parse("gs://bucket/second-temporary").unwrap(),
        ),
    ];

    process_moves(&storage, &interrupt, moves).unwrap();
    server.join().unwrap();

    let requests = requests.lock().unwrap();
    assert_eq!(requests.len(), 24);
    assert_eq!(
        request_line(&requests[0]),
        "POST /storage/v1/b/bucket/o?uploadType=media&name=.task-googlecloud-lock&ifGenerationMatch=0 HTTP/1.1"
    );
    assert_eq!(
        request_line(&requests[23]),
        "DELETE /storage/v1/b/bucket/o/.task-googlecloud-lock?generation=lock HTTP/1.1"
    );
}

#[cfg(unix)]
#[test]
fn requires_recovery_when_move_confirmation_fails_after_copy() {
    let (error, requests) = move_with_token_failure(
        4,
        vec![
            bucket_lock_created(),
            (200, r#"{"generation":"11"}"#.to_string()),
            (
                200,
                r#"{"done":true,"resource":{"generation":"22"}}"#.to_string(),
            ),
            bucket_lock_deleted(),
        ],
    );

    assert!(matches!(error, AppError::Recovery { .. }), "{error}");
    assert_eq!(requests.len(), 4);
    assert!(request_line(&requests[2]).contains("rewriteTo"));
    assert!(request_line(&requests[3]).contains(".task-googlecloud-lock"));
}

#[cfg(unix)]
#[test]
fn requires_recovery_when_move_confirmation_fails_after_source_deletion() {
    let (error, requests) = move_with_token_failure(
        7,
        vec![
            bucket_lock_created(),
            (200, r#"{"generation":"11"}"#.to_string()),
            (
                200,
                r#"{"done":true,"resource":{"generation":"22"}}"#.to_string(),
            ),
            (200, r#"{"generation":"22"}"#.to_string()),
            (200, "{}".to_string()),
            (404, "{}".to_string()),
            bucket_lock_deleted(),
        ],
    );

    assert!(matches!(error, AppError::Recovery { .. }), "{error}");
    assert_eq!(requests.len(), 7);
    assert!(request_line(&requests[4]).contains("DELETE"));
    assert!(request_line(&requests[6]).contains(".task-googlecloud-lock"));
}

#[test]
fn rejects_a_move_when_the_target_generation_changes_after_copy() {
    let (base, requests, server) = test_server_allowing_early_close(vec![
        bucket_lock_created(),
        (200, r#"{"generation":"11"}"#.to_string()),
        (
            200,
            r#"{"done":true,"resource":{"generation":"22"}}"#.to_string(),
        ),
        (200, r#"{"generation":"23"}"#.to_string()),
        bucket_lock_deleted(),
    ]);
    let source = ObjectPath::parse("gs://bucket/source").unwrap();
    let target = ObjectPath::parse("gs://bucket/target").unwrap();

    let error = storage(&base)
        .move_object(&source, &target, None)
        .unwrap_err();
    server.join().unwrap();

    assert!(matches!(error, AppError::Recovery { .. }));
    let requests = requests.lock().unwrap();
    assert_eq!(requests.len(), 5);
    assert_eq!(
        request_line(&requests[3]),
        "GET /storage/v1/b/bucket/o/target HTTP/1.1"
    );
}

#[test]
fn rejects_a_move_when_the_target_generation_changes_after_source_deletion() {
    let (base, requests, server) = test_server_allowing_early_close(vec![
        bucket_lock_created(),
        (200, r#"{"generation":"11"}"#.to_string()),
        (
            200,
            r#"{"done":true,"resource":{"generation":"22"}}"#.to_string(),
        ),
        (200, r#"{"generation":"22"}"#.to_string()),
        (200, "{}".to_string()),
        (404, "{}".to_string()),
        (200, r#"{"generation":"23"}"#.to_string()),
        bucket_lock_deleted(),
    ]);
    let source = ObjectPath::parse("gs://bucket/source").unwrap();
    let target = ObjectPath::parse("gs://bucket/target").unwrap();

    let error = storage(&base)
        .move_object(&source, &target, None)
        .unwrap_err();
    server.join().unwrap();

    assert!(matches!(error, AppError::Recovery { .. }));
    let requests = requests.lock().unwrap();
    assert_eq!(requests.len(), 8);
    assert_eq!(
        request_line(&requests[6]),
        "GET /storage/v1/b/bucket/o/target HTTP/1.1"
    );
}

#[test]
fn rejects_a_move_when_the_expected_source_generation_is_stale() {
    let (base, requests, server) = test_server(vec![
        bucket_lock_created(),
        (
            412,
            r#"{"error":{"message":"generation condition not met"}}"#.to_string(),
        ),
        bucket_lock_deleted(),
    ]);
    let storage = storage(&base);
    let source = ObjectPath::parse("gs://bucket/source").unwrap();
    let target = ObjectPath::parse("gs://bucket/target").unwrap();

    let error = storage
        .move_object(&source, &target, Some("11"))
        .unwrap_err();
    server.join().unwrap();

    assert!(matches!(error, AppError::Storage { status: 412, .. }));
    let requests = requests.lock().unwrap();
    assert_eq!(requests.len(), 3);
    assert_eq!(
        request_line(&requests[1]),
        "POST /storage/v1/b/bucket/o/source/rewriteTo/b/bucket/o/target?sourceGeneration=11&ifSourceGenerationMatch=11&ifGenerationMatch=0 HTTP/1.1"
    );
}

#[test]
fn does_not_move_when_the_bucket_lock_cannot_be_acquired() {
    let (base, requests, server) = test_server(vec![(
        412,
        r#"{"error":{"message":"bucket is locked"}}"#.to_string(),
    )]);
    let source = ObjectPath::parse("gs://bucket/source").unwrap();
    let target = ObjectPath::parse("gs://bucket/target").unwrap();

    let error = storage(&base)
        .move_object(&source, &target, None)
        .unwrap_err();
    server.join().unwrap();

    assert!(matches!(error, AppError::BucketLockConflict(_)));
    let requests = requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(
        request_line(&requests[0]),
        "POST /storage/v1/b/bucket/o?uploadType=media&name=.task-googlecloud-lock&ifGenerationMatch=0 HTTP/1.1"
    );
}

#[test]
fn releases_earlier_bucket_locks_when_a_later_lock_cannot_be_acquired() {
    let (base, requests, server) = test_server(vec![
        bucket_lock_created(),
        (
            412,
            r#"{"error":{"message":"bucket is locked"}}"#.to_string(),
        ),
        bucket_lock_deleted(),
    ]);
    let source = ObjectPath::parse("gs://bucket-b/source").unwrap();
    let target = ObjectPath::parse("gs://bucket-a/target").unwrap();

    let error = storage(&base)
        .move_object(&source, &target, Some("11"))
        .unwrap_err();
    server.join().unwrap();

    assert!(matches!(error, AppError::BucketLockConflict(_)));
    let requests = requests.lock().unwrap();
    assert_eq!(requests.len(), 3);
    assert_eq!(
        request_line(&requests[0]),
        "POST /storage/v1/b/bucket-a/o?uploadType=media&name=.task-googlecloud-lock&ifGenerationMatch=0 HTTP/1.1"
    );
    assert_eq!(
        request_line(&requests[1]),
        "POST /storage/v1/b/bucket-b/o?uploadType=media&name=.task-googlecloud-lock&ifGenerationMatch=0 HTTP/1.1"
    );
    assert_eq!(
        request_line(&requests[2]),
        "DELETE /storage/v1/b/bucket-a/o/.task-googlecloud-lock?generation=lock HTTP/1.1"
    );
}

#[test]
fn process_moves_returns_lock_conflicts_without_confirming_objects() {
    let (base, requests, server) = test_server(vec![(
        412,
        r#"{"error":{"message":"bucket is locked"}}"#.to_string(),
    )]);
    let storage = storage(&base);
    let interrupt = InterruptFlag::from_atomic(Arc::new(std::sync::atomic::AtomicBool::new(false)));
    let source = ObjectPath::parse("gs://bucket/source").unwrap();
    let target = ObjectPath::parse("gs://bucket/target").unwrap();
    let temporary = ObjectPath::parse("gs://bucket/temporary").unwrap();

    let error = process_moves(&storage, &interrupt, vec![(source, target, temporary)]).unwrap_err();
    server.join().unwrap();

    assert!(!matches!(error, AppError::Recovery { .. }), "{error}");
    assert_eq!(requests.lock().unwrap().len(), 1);
}

#[test]
fn rejects_a_move_when_the_source_remains_after_delete() {
    let (base, requests, server) = test_server(vec![
        bucket_lock_created(),
        (200, r#"{"generation":"11"}"#.to_string()),
        (
            200,
            r#"{"done":true,"resource":{"generation":"22"}}"#.to_string(),
        ),
        (200, r#"{"generation":"22"}"#.to_string()),
        (200, "{}".to_string()),
        (200, r#"{"generation":"11"}"#.to_string()),
        (200, r#"{"generation":"22"}"#.to_string()),
        bucket_lock_deleted(),
    ]);
    let source = ObjectPath::parse("gs://bucket/source").unwrap();
    let target = ObjectPath::parse("gs://bucket/target").unwrap();

    let error = storage(&base)
        .move_object(&source, &target, None)
        .unwrap_err();
    server.join().unwrap();

    assert!(error.to_string().contains("Source object remains"));
    let requests = requests.lock().unwrap();
    assert_eq!(requests.len(), 8);
    assert_eq!(
        request_line(&requests[1]),
        "GET /storage/v1/b/bucket/o/source HTTP/1.1"
    );
    assert_eq!(
        request_line(&requests[2]),
        "POST /storage/v1/b/bucket/o/source/rewriteTo/b/bucket/o/target?sourceGeneration=11&ifSourceGenerationMatch=11&ifGenerationMatch=0 HTTP/1.1"
    );
    assert_eq!(
        request_line(&requests[3]),
        "GET /storage/v1/b/bucket/o/target HTTP/1.1"
    );
    assert_eq!(
        request_line(&requests[4]),
        "DELETE /storage/v1/b/bucket/o/source?generation=11 HTTP/1.1"
    );
    assert_eq!(
        request_line(&requests[5]),
        "GET /storage/v1/b/bucket/o/source HTTP/1.1"
    );
    assert_eq!(
        request_line(&requests[6]),
        "GET /storage/v1/b/bucket/o/target HTTP/1.1"
    );
    assert_eq!(
        request_line(&requests[7]),
        "DELETE /storage/v1/b/bucket/o/.task-googlecloud-lock?generation=lock HTTP/1.1"
    );
}

#[test]
fn rolls_back_objects_and_confirms_target_deletion() {
    let (base, requests, server) = test_server(vec![
        bucket_lock_created(),
        (404, "{}".to_string()),
        (200, r#"{"generation":"22"}"#.to_string()),
        (
            200,
            r#"{"done":true,"resource":{"generation":"33"}}"#.to_string(),
        ),
        (200, r#"{"generation":"33"}"#.to_string()),
        (200, "{}".to_string()),
        (404, "{}".to_string()),
        (200, r#"{"generation":"33"}"#.to_string()),
        bucket_lock_deleted(),
    ]);
    let storage = storage(&base);
    let source = ObjectPath::parse("gs://bucket/source").unwrap();
    let target = ObjectPath::parse("gs://bucket/target").unwrap();

    let generation = storage.rollback_object(&source, &target, "22").unwrap();
    server.join().unwrap();

    assert_eq!(generation, "33");
    let requests = requests.lock().unwrap();
    assert_eq!(requests.len(), 9);
    assert_eq!(
        request_line(&requests[1]),
        "GET /storage/v1/b/bucket/o/source HTTP/1.1"
    );
    assert_eq!(
        request_line(&requests[2]),
        "GET /storage/v1/b/bucket/o/target?generation=22 HTTP/1.1"
    );
    assert_eq!(
        request_line(&requests[3]),
        "POST /storage/v1/b/bucket/o/target/rewriteTo/b/bucket/o/source?sourceGeneration=22&ifSourceGenerationMatch=22&ifGenerationMatch=0 HTTP/1.1"
    );
    assert_eq!(
        request_line(&requests[4]),
        "GET /storage/v1/b/bucket/o/source HTTP/1.1"
    );
    assert_eq!(
        request_line(&requests[5]),
        "DELETE /storage/v1/b/bucket/o/target?generation=22 HTTP/1.1"
    );
    assert_eq!(
        request_line(&requests[6]),
        "GET /storage/v1/b/bucket/o/target HTTP/1.1"
    );
    assert_eq!(
        request_line(&requests[7]),
        "GET /storage/v1/b/bucket/o/source HTTP/1.1"
    );
    assert_eq!(
        request_line(&requests[8]),
        "DELETE /storage/v1/b/bucket/o/.task-googlecloud-lock?generation=lock HTTP/1.1"
    );
}

#[cfg(unix)]
#[test]
fn requires_recovery_when_rollback_confirmation_fails_after_copy() {
    let (error, requests) = rollback_with_token_failure(
        5,
        vec![
            bucket_lock_created(),
            (404, "{}".to_string()),
            (200, r#"{"generation":"22"}"#.to_string()),
            (
                200,
                r#"{"done":true,"resource":{"generation":"33"}}"#.to_string(),
            ),
            bucket_lock_deleted(),
        ],
    );

    assert!(matches!(error, AppError::Recovery { .. }), "{error}");
    assert_eq!(requests.len(), 5);
    assert!(request_line(&requests[3]).contains("rewriteTo"));
    assert!(request_line(&requests[4]).contains(".task-googlecloud-lock"));
}

#[cfg(unix)]
#[test]
fn requires_recovery_when_rollback_confirmation_fails_after_target_deletion() {
    let (error, requests) = rollback_with_token_failure(
        8,
        vec![
            bucket_lock_created(),
            (404, "{}".to_string()),
            (200, r#"{"generation":"22"}"#.to_string()),
            (
                200,
                r#"{"done":true,"resource":{"generation":"33"}}"#.to_string(),
            ),
            (200, r#"{"generation":"33"}"#.to_string()),
            (200, "{}".to_string()),
            (404, "{}".to_string()),
            bucket_lock_deleted(),
        ],
    );

    assert!(matches!(error, AppError::Recovery { .. }), "{error}");
    assert_eq!(requests.len(), 8);
    assert!(request_line(&requests[5]).contains("DELETE"));
    assert!(request_line(&requests[7]).contains(".task-googlecloud-lock"));
}

#[test]
fn rejects_a_rollback_when_the_source_generation_changes_after_copy() {
    let (base, requests, server) = test_server_allowing_early_close(vec![
        bucket_lock_created(),
        (404, "{}".to_string()),
        (200, r#"{"generation":"22"}"#.to_string()),
        (
            200,
            r#"{"done":true,"resource":{"generation":"33"}}"#.to_string(),
        ),
        (200, r#"{"generation":"34"}"#.to_string()),
        bucket_lock_deleted(),
    ]);
    let source = ObjectPath::parse("gs://bucket/source").unwrap();
    let target = ObjectPath::parse("gs://bucket/target").unwrap();

    let error = storage(&base)
        .rollback_object(&source, &target, "22")
        .unwrap_err();
    server.join().unwrap();

    assert!(matches!(error, AppError::Recovery { .. }));
    let requests = requests.lock().unwrap();
    assert_eq!(requests.len(), 6);
    assert_eq!(
        request_line(&requests[4]),
        "GET /storage/v1/b/bucket/o/source HTTP/1.1"
    );
}

#[test]
fn does_not_rollback_when_the_bucket_lock_cannot_be_acquired() {
    let (base, requests, server) = test_server(vec![(
        412,
        r#"{"error":{"message":"bucket is locked"}}"#.to_string(),
    )]);
    let source = ObjectPath::parse("gs://bucket/source").unwrap();
    let target = ObjectPath::parse("gs://bucket/target").unwrap();

    let error = storage(&base)
        .rollback_object(&source, &target, "22")
        .unwrap_err();
    server.join().unwrap();

    assert!(matches!(error, AppError::BucketLockConflict(_)));
    let requests = requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(
        request_line(&requests[0]),
        "POST /storage/v1/b/bucket/o?uploadType=media&name=.task-googlecloud-lock&ifGenerationMatch=0 HTTP/1.1"
    );
}

#[test]
fn rejects_a_rollback_when_the_source_generation_changes_after_target_deletion() {
    let (base, requests, server) = test_server_allowing_early_close(vec![
        bucket_lock_created(),
        (404, "{}".to_string()),
        (200, r#"{"generation":"22"}"#.to_string()),
        (
            200,
            r#"{"done":true,"resource":{"generation":"33"}}"#.to_string(),
        ),
        (200, r#"{"generation":"33"}"#.to_string()),
        (200, "{}".to_string()),
        (404, "{}".to_string()),
        (200, r#"{"generation":"34"}"#.to_string()),
        bucket_lock_deleted(),
    ]);
    let source = ObjectPath::parse("gs://bucket/source").unwrap();
    let target = ObjectPath::parse("gs://bucket/target").unwrap();

    let error = storage(&base)
        .rollback_object(&source, &target, "22")
        .unwrap_err();
    server.join().unwrap();

    assert!(matches!(error, AppError::Recovery { .. }));
    let requests = requests.lock().unwrap();
    assert_eq!(requests.len(), 9);
    assert_eq!(
        request_line(&requests[7]),
        "GET /storage/v1/b/bucket/o/source HTTP/1.1"
    );
}

#[test]
fn rejects_a_rollback_when_the_target_remains_after_delete() {
    let (base, requests, server) = test_server(vec![
        bucket_lock_created(),
        (404, "{}".to_string()),
        (200, r#"{"generation":"22"}"#.to_string()),
        (
            200,
            r#"{"done":true,"resource":{"generation":"33"}}"#.to_string(),
        ),
        (200, r#"{"generation":"33"}"#.to_string()),
        (200, "{}".to_string()),
        (200, r#"{"generation":"22"}"#.to_string()),
        (200, r#"{"generation":"33"}"#.to_string()),
        bucket_lock_deleted(),
    ]);
    let source = ObjectPath::parse("gs://bucket/source").unwrap();
    let target = ObjectPath::parse("gs://bucket/target").unwrap();

    let error = storage(&base)
        .rollback_object(&source, &target, "22")
        .unwrap_err();
    server.join().unwrap();

    assert!(error.to_string().contains("Rollback target remains"));
    let requests = requests.lock().unwrap();
    assert_eq!(requests.len(), 9);
    assert_eq!(
        request_line(&requests[1]),
        "GET /storage/v1/b/bucket/o/source HTTP/1.1"
    );
    assert_eq!(
        request_line(&requests[2]),
        "GET /storage/v1/b/bucket/o/target?generation=22 HTTP/1.1"
    );
    assert_eq!(
        request_line(&requests[3]),
        "POST /storage/v1/b/bucket/o/target/rewriteTo/b/bucket/o/source?sourceGeneration=22&ifSourceGenerationMatch=22&ifGenerationMatch=0 HTTP/1.1"
    );
    assert_eq!(
        request_line(&requests[4]),
        "GET /storage/v1/b/bucket/o/source HTTP/1.1"
    );
    assert_eq!(
        request_line(&requests[5]),
        "DELETE /storage/v1/b/bucket/o/target?generation=22 HTTP/1.1"
    );
    assert_eq!(
        request_line(&requests[6]),
        "GET /storage/v1/b/bucket/o/target HTTP/1.1"
    );
    assert_eq!(
        request_line(&requests[7]),
        "GET /storage/v1/b/bucket/o/source HTTP/1.1"
    );
    assert_eq!(
        request_line(&requests[8]),
        "DELETE /storage/v1/b/bucket/o/.task-googlecloud-lock?generation=lock HTTP/1.1"
    );
}

#[test]
fn cleans_up_owned_objects_and_accepts_missing_objects() {
    let (base, requests, server) = test_server(vec![
        bucket_lock_created(),
        (200, r#"{"generation":"22"}"#.to_string()),
        (200, "{}".to_string()),
        (404, "{}".to_string()),
        bucket_lock_deleted(),
    ]);
    let api = storage(&base);
    let target = ObjectPath::parse("gs://bucket/target").unwrap();

    api.cleanup_object(&target, "22").unwrap();
    server.join().unwrap();

    let requests = requests.lock().unwrap();
    assert_eq!(requests.len(), 5);
    assert_eq!(
        request_line(&requests[1]),
        "GET /storage/v1/b/bucket/o/target?generation=22 HTTP/1.1"
    );
    assert_eq!(
        request_line(&requests[2]),
        "DELETE /storage/v1/b/bucket/o/target?generation=22 HTTP/1.1"
    );
    assert_eq!(
        request_line(&requests[3]),
        "GET /storage/v1/b/bucket/o/target HTTP/1.1"
    );

    let (base, requests, server) = test_server(vec![
        bucket_lock_created(),
        (404, "{}".to_string()),
        (404, "{}".to_string()),
        bucket_lock_deleted(),
    ]);
    storage(&base).cleanup_object(&target, "22").unwrap();
    server.join().unwrap();
    let requests = requests.lock().unwrap();
    assert_eq!(requests.len(), 4);
    assert_eq!(
        request_line(&requests[1]),
        "GET /storage/v1/b/bucket/o/target?generation=22 HTTP/1.1"
    );
    assert_eq!(
        request_line(&requests[2]),
        "GET /storage/v1/b/bucket/o/target HTTP/1.1"
    );
}

#[test]
fn rejects_cleanup_when_the_target_remains_after_delete() {
    let (base, requests, server) = test_server(vec![
        bucket_lock_created(),
        (200, r#"{"generation":"22"}"#.to_string()),
        (200, "{}".to_string()),
        (200, r#"{"generation":"22"}"#.to_string()),
        bucket_lock_deleted(),
    ]);
    let target = ObjectPath::parse("gs://bucket/target").unwrap();

    let error = storage(&base).cleanup_object(&target, "22").unwrap_err();
    server.join().unwrap();

    assert!(error.to_string().contains("Cleanup target remains"));
    let requests = requests.lock().unwrap();
    assert_eq!(requests.len(), 5);
    assert_eq!(
        request_line(&requests[1]),
        "GET /storage/v1/b/bucket/o/target?generation=22 HTTP/1.1"
    );
    assert_eq!(
        request_line(&requests[2]),
        "DELETE /storage/v1/b/bucket/o/target?generation=22 HTTP/1.1"
    );
    assert_eq!(
        request_line(&requests[3]),
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

    assert!(matches!(
        error,
        AppError::Rollback { original, .. } if matches!(*original, AppError::Http(_))
    ));
    let requests = requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert_eq!(
        request_line(&requests[1]),
        "GET /storage/v1/b/bucket/o?maxResults=1000 HTTP/1.1"
    );
}

#[test]
fn rewrites_objects_with_generations_and_continuation_tokens() {
    let (base, requests, server) = test_server(vec![
        bucket_lock_created(),
        (
            200,
            r#"{"done":false,"rewriteToken":"continue token"}"#.to_string(),
        ),
        (
            200,
            r#"{"done":true,"resource":{"generation":"456"}}"#.to_string(),
        ),
        bucket_lock_deleted(),
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
    assert_eq!(requests.len(), 4);
    assert_eq!(
        request_line(&requests[1]),
        "POST /storage/v1/b/bucket/o/folder%2A%3F%5B%5D%23%2Fsource/rewriteTo/b/bucket/o/folder%2A%3F%5B%5D%23%2Ftarget?sourceGeneration=123&ifSourceGenerationMatch=123&ifGenerationMatch=0 HTTP/1.1"
    );
    assert_eq!(
        request_line(&requests[2]),
        "POST /storage/v1/b/bucket/o/folder%2A%3F%5B%5D%23%2Fsource/rewriteTo/b/bucket/o/folder%2A%3F%5B%5D%23%2Ftarget?sourceGeneration=123&ifSourceGenerationMatch=123&ifGenerationMatch=0&rewriteToken=continue+token HTTP/1.1"
    );
}
