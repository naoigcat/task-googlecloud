use std::cell::RefCell;
use std::path::Path;

use task_googlecloud::{
    AppError, ObjectPath, RemoteChange, StorageClient, rollback_remote, upload_files_by_directory,
};
use tempfile::tempdir;

struct FakeStorage {
    uploads: RefCell<Vec<String>>,
    moves: RefCell<Vec<String>>,
    rollbacks: RefCell<Vec<String>>,
    cleanups: RefCell<Vec<String>>,
    fail_on_move: Option<usize>,
}

impl StorageClient for FakeStorage {
    fn list_objects(&self, _bucket: &str) -> Result<Vec<String>, AppError> {
        Ok(Vec::new())
    }

    fn upload_file(&self, _source: &Path, target: &ObjectPath) -> Result<String, AppError> {
        self.uploads.borrow_mut().push(target.uri());
        Ok("101".to_string())
    }

    fn move_object(&self, _source: &ObjectPath, target: &ObjectPath) -> Result<String, AppError> {
        let mut moves = self.moves.borrow_mut();
        moves.push(target.uri());
        if self.fail_on_move == Some(moves.len()) {
            return Err(AppError::Message("finalization failed".to_string()));
        }
        Ok((200 + moves.len()).to_string())
    }

    fn rollback_object(
        &self,
        _source: &ObjectPath,
        target: &ObjectPath,
        _generation: &str,
    ) -> Result<String, AppError> {
        self.rollbacks.borrow_mut().push(target.uri());
        Ok("301".to_string())
    }

    fn cleanup_object(
        &self,
        target: &ObjectPath,
        _target_generation: &str,
    ) -> Result<(), AppError> {
        self.cleanups.borrow_mut().push(target.uri());
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
fn discovers_files_by_bucket_directory() {
    let directory = tempdir().unwrap();
    let bucket = directory.path().join("bucket");
    std::fs::create_dir(&bucket).unwrap();
    std::fs::write(bucket.join("one.txt"), "one").unwrap();
    std::fs::write(bucket.join("two.txt"), "two").unwrap();
    std::fs::write(bucket.join(".DS_Store"), "metadata").unwrap();

    let found = upload_files_by_directory(directory.path()).unwrap();

    assert_eq!(
        found.get(&bucket).unwrap(),
        &vec![bucket.join("one.txt"), bucket.join("two.txt")]
    );
}

#[test]
fn reports_a_file_as_an_invalid_upload_root() {
    let directory = tempdir().unwrap();
    let root = directory.path().join("uploads");
    std::fs::write(&root, "not a directory").unwrap();

    let error = upload_files_by_directory(&root).unwrap_err();

    assert!(matches!(
        error,
        AppError::Io(error) if error.kind() == std::io::ErrorKind::NotADirectory
    ));
}

#[cfg(unix)]
#[test]
fn ignores_symlinked_buckets_and_files() {
    use std::os::unix::fs::symlink;

    let root = tempdir().unwrap();
    let outside = tempdir().unwrap();
    let bucket = root.path().join("bucket");
    let outside_bucket = outside.path().join("outside-bucket");
    let outside_file = outside.path().join("secret.txt");
    let linked_bucket = root.path().join("linked-bucket");

    std::fs::create_dir(&bucket).unwrap();
    std::fs::create_dir(&outside_bucket).unwrap();
    std::fs::write(bucket.join("regular.txt"), "regular").unwrap();
    std::fs::write(&outside_file, "secret").unwrap();
    std::fs::write(outside_bucket.join("outside.txt"), "outside").unwrap();
    symlink(&outside_file, bucket.join("linked.txt")).unwrap();
    symlink(&outside_bucket, &linked_bucket).unwrap();

    let found = upload_files_by_directory(root.path()).unwrap();

    assert_eq!(found.get(&bucket), Some(&vec![bucket.join("regular.txt")]));
    assert!(!found.contains_key(&linked_bucket));
}

#[test]
fn rolls_back_finalized_and_staged_remote_changes() {
    let storage = FakeStorage {
        uploads: RefCell::new(Vec::new()),
        moves: RefCell::new(Vec::new()),
        rollbacks: RefCell::new(Vec::new()),
        cleanups: RefCell::new(Vec::new()),
        fail_on_move: None,
    };
    let staging = ObjectPath::parse("gs://bucket/.task-googlecloud-staging/token/file").unwrap();
    let final_path = ObjectPath::parse("gs://bucket/file").unwrap();
    let staged = vec![RemoteChange {
        source: staging.clone(),
        target: final_path.clone(),
        generation: "101".to_string(),
    }];
    let finalized = vec![RemoteChange {
        source: staging,
        target: final_path,
        generation: "201".to_string(),
    }];

    let errors = rollback_remote(&storage, &staged, &finalized);

    assert!(errors.is_empty());
    assert_eq!(storage.rollbacks.borrow().len(), 1);
    assert_eq!(storage.cleanups.borrow().len(), 1);
}
