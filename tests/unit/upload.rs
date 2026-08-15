use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::sync::{Arc, atomic::AtomicBool};
use std::thread;

use super::{
    MAX_OBJECT_NAME_BYTES, PlannedUpload, plan_uploads, staging_path, upload_planned_files,
};
use crate::InterruptFlag;
use crate::cloud::Cloud;
use crate::error::AppError;
use crate::storage::{ObjectPath, StorageApi, StorageClient};

const STAGING_PREFIX: &str = ".task-googlecloud-staging/0123456789abcdef0123456789abcdef";

fn object_name_with_bytes(length: usize) -> String {
    let mut object = "é".repeat(length / 2);
    if object.len() < length {
        object.push('a');
    }
    object
}

fn lock_conflict_server() -> (
    String,
    Arc<std::sync::Mutex<Vec<String>>>,
    thread::JoinHandle<()>,
) {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let address = format!("http://{}/storage/v1", listener.local_addr().unwrap());
    let requests = Arc::new(std::sync::Mutex::new(Vec::new()));
    let recorded_requests = Arc::clone(&requests);
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
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
        recorded_requests
            .lock()
            .unwrap()
            .push(String::from_utf8(request).unwrap());
        let body = r#"{"error":{"message":"bucket is locked"}}"#;
        let response = format!(
            "HTTP/1.1 412 Error\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        stream.write_all(response.as_bytes()).unwrap();
    });
    (address, requests, server)
}

struct FakeStorage {
    move_generations: RefCell<Vec<Option<String>>>,
}

impl StorageClient for FakeStorage {
    fn list_objects(&self, _bucket: &str) -> Result<Vec<String>, AppError> {
        Ok(Vec::new())
    }

    fn upload_file(&self, _source: &Path, _target: &ObjectPath) -> Result<String, AppError> {
        Ok("101".to_string())
    }

    fn move_object(
        &self,
        _source: &ObjectPath,
        _target: &ObjectPath,
        expected_source_generation: Option<&str>,
    ) -> Result<String, AppError> {
        self.move_generations
            .borrow_mut()
            .push(expected_source_generation.map(str::to_string));
        Ok("202".to_string())
    }

    fn rollback_object(
        &self,
        _source: &ObjectPath,
        _target: &ObjectPath,
        _target_generation: &str,
    ) -> Result<String, AppError> {
        Ok("rollback".to_string())
    }

    fn cleanup_object(
        &self,
        _target: &ObjectPath,
        _target_generation: &str,
    ) -> Result<(), AppError> {
        Ok(())
    }

    fn confirm_move_after_failure(
        &self,
        _source: &ObjectPath,
        _target: &ObjectPath,
        _operation: &AppError,
    ) -> Result<(), AppError> {
        Ok(())
    }

    fn confirm_write_after_failure(
        &self,
        _target: &ObjectPath,
        _operation: &AppError,
    ) -> Result<(), AppError> {
        Ok(())
    }
}

#[test]
fn finalizes_uploaded_objects_using_their_uploaded_generation() {
    let storage = FakeStorage {
        move_generations: RefCell::new(Vec::new()),
    };
    let interrupt = InterruptFlag::from_atomic(Arc::new(AtomicBool::new(false)));
    let staging = ObjectPath::parse("gs://bucket/.task-googlecloud-staging/file").unwrap();
    let target = ObjectPath::parse("gs://bucket/file").unwrap();
    let uploads = vec![(
        PathBuf::from("uploads/bucket"),
        vec![PlannedUpload {
            file: PathBuf::from("uploads/bucket/file"),
            staging,
            target,
        }],
    )];

    upload_planned_files(&storage, &interrupt, &uploads).unwrap();

    assert_eq!(
        *storage.move_generations.borrow(),
        vec![Some("101".to_string())]
    );
}

#[test]
fn returns_lock_conflicts_without_confirming_uploaded_objects() {
    let (base, requests, server) = lock_conflict_server();
    let storage =
        StorageApi::with_endpoints(Cloud::new(), base.clone(), base, Some("token".to_string()));
    let source = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(source.path(), b"contents").unwrap();
    let uploads = vec![(
        PathBuf::from("uploads/bucket"),
        vec![PlannedUpload {
            file: source.path().to_path_buf(),
            staging: ObjectPath::parse("gs://bucket/staging").unwrap(),
            target: ObjectPath::parse("gs://bucket/target").unwrap(),
        }],
    )];
    let interrupt = InterruptFlag::from_atomic(Arc::new(AtomicBool::new(false)));

    let error = upload_planned_files(&storage, &interrupt, &uploads).unwrap_err();
    server.join().unwrap();

    assert!(!matches!(error, AppError::Recovery { .. }), "{error}");
    assert_eq!(requests.lock().unwrap().len(), 1);
}

#[test]
fn rejects_object_names_that_would_overflow_the_staging_prefix() {
    let object_name = object_name_with_bytes(MAX_OBJECT_NAME_BYTES - STAGING_PREFIX.len());

    let error = staging_path("bucket", STAGING_PREFIX, &object_name).unwrap_err();

    assert!(matches!(error, AppError::Message(message) if message.contains("temporary staging")));
}

#[test]
fn accepts_object_names_at_the_staging_limit() {
    let object_name = object_name_with_bytes(MAX_OBJECT_NAME_BYTES - STAGING_PREFIX.len() - 1);

    let staging = staging_path("bucket", STAGING_PREFIX, &object_name).unwrap();

    assert_eq!(staging.object.len(), MAX_OBJECT_NAME_BYTES);
}

#[test]
fn plans_uploads_from_discovery_alone() {
    let bucket = PathBuf::from("uploads/bucket");
    let source = bucket.join("e\u{301}.txt");
    let target = bucket.join("\u{e9}.txt");
    let files_by_directory = BTreeMap::from([(bucket, vec![source.clone()])]);
    let normalized_files = HashMap::from([(source, target)]);

    let planned = plan_uploads(&files_by_directory, &normalized_files, STAGING_PREFIX).unwrap();

    assert_eq!(planned.len(), 1);
    let uploads = &planned[0].1;
    assert_eq!(uploads.len(), 1);
    assert_eq!(uploads[0].target.uri(), "gs://bucket/\u{e9}.txt");
    assert_eq!(
        uploads[0].staging.uri(),
        format!("gs://bucket/{STAGING_PREFIX}/\u{e9}.txt")
    );
}

#[test]
fn rejects_a_plan_before_any_file_is_renamed_or_uploaded() {
    let bucket = PathBuf::from("uploads/bucket");
    let object_name = object_name_with_bytes(MAX_OBJECT_NAME_BYTES - STAGING_PREFIX.len());
    let source = bucket.join(&object_name);
    let files_by_directory = BTreeMap::from([(bucket, vec![source.clone()])]);
    let normalized_files = HashMap::from([(source.clone(), source)]);

    let error = plan_uploads(&files_by_directory, &normalized_files, STAGING_PREFIX).unwrap_err();

    assert!(matches!(error, AppError::Message(message) if message.contains("temporary staging")));
}
