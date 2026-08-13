use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};

use task_googlecloud::{Cloud, ObjectPath, StorageApi, StorageClient};

fn test_server(responses: Vec<(u16, String)>) -> (String, Arc<Mutex<Vec<String>>>, JoinHandle<()>) {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let address = format!("http://{}/storage/v1", listener.local_addr().unwrap());
    let requests = Arc::new(Mutex::new(Vec::new()));
    let recorded_requests = Arc::clone(&requests);
    let handle = thread::spawn(move || {
        for (status, body) in responses {
            let (mut stream, _) = listener.accept().unwrap();
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

fn read_request(stream: &mut TcpStream) -> String {
    let mut request = Vec::new();
    let mut buffer = [0_u8; 1];
    while !request.ends_with(b"\r\n\r\n") {
        stream.read_exact(&mut buffer).unwrap();
        request.push(buffer[0]);
    }
    String::from_utf8(request).unwrap()
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
    assert!(requests[0].contains(
        "folder%2A%3F%5B%5D%23%2Fsource/rewriteTo/b/bucket/o/folder%2A%3F%5B%5D%23%2Ftarget"
    ));
    assert!(!requests[0].contains("?[]#"));
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
    assert!(requests[0].contains("/o/-target%3F?"));
    assert!(requests[0].contains("sourceGeneration=123"));
    assert!(requests[0].contains("ifSourceGenerationMatch=123"));
    assert!(requests[0].contains("ifGenerationMatch=0"));
}

#[test]
fn lists_empty_and_paginated_responses() {
    let (base, requests, server) = test_server(vec![(200, "{}".to_string())]);
    let objects = storage(&base).list_objects("bucket").unwrap();
    server.join().unwrap();
    assert!(objects.is_empty());
    assert_eq!(requests.lock().unwrap().len(), 1);

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
    assert!(requests[0].starts_with("GET /storage/v1/b/bucket/o?maxResults=1000 "));
    assert!(requests[1].starts_with("GET /storage/v1/b/bucket/o?maxResults=1000&pageToken=next "));
    assert!(
        requests[0]
            .to_ascii_lowercase()
            .contains("authorization: bearer token")
    );
}

#[test]
fn reports_api_error_messages() {
    let (base, _requests, server) = test_server(vec![(
        403,
        r#"{"error":{"message":"permission denied"}}"#.to_string(),
    )]);

    let error = storage(&base).list_objects("bucket").unwrap_err();
    server.join().unwrap();

    assert!(error.to_string().contains("permission denied"));
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
    assert!(requests[0].contains("sourceGeneration=123"));
    assert!(requests[0].contains("ifSourceGenerationMatch=123"));
    assert!(requests[0].contains("ifGenerationMatch=0"));
    assert!(requests[1].contains("rewriteToken=continue+token"));
}
