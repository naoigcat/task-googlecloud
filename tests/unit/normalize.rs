use std::cell::Cell;
#[cfg(unix)]
use std::ffi::OsString;
use std::fs;
use std::path::Path;
use std::sync::{Arc, atomic::AtomicBool};
#[cfg(unix)]
use std::sync::{Mutex, MutexGuard};

use super::{MAX_OBJECT_NAME_BYTES, run, temporary_path, temporary_suffix};
use crate::cloud::Cloud;
use crate::error::AppError;
use crate::normalization_plan::normalized;
use crate::storage::{ObjectPath, StorageClient};
use tempfile::tempdir;

#[cfg(unix)]
static PATH_LOCK: Mutex<()> = Mutex::new(());

#[cfg(unix)]
struct PathGuard {
    original: Option<OsString>,
    _lock: MutexGuard<'static, ()>,
}

#[cfg(unix)]
impl PathGuard {
    fn prepend(directory: &Path) -> Self {
        let lock = PATH_LOCK.lock().unwrap_or_else(|error| error.into_inner());
        let original = std::env::var_os("PATH");
        let path = match &original {
            Some(path) => format!("{}:{}", directory.display(), path.to_string_lossy()),
            None => directory.display().to_string(),
        };
        // Serialize PATH changes and restore them through Drop so parallel or failing tests stay isolated.
        unsafe { std::env::set_var("PATH", path) };
        Self {
            original,
            _lock: lock,
        }
    }
}

#[cfg(unix)]
impl Drop for PathGuard {
    fn drop(&mut self) {
        match self.original.take() {
            Some(path) => unsafe { std::env::set_var("PATH", path) },
            None => unsafe { std::env::remove_var("PATH") },
        }
    }
}

struct CountingStorage {
    objects: Vec<String>,
    move_calls: Cell<usize>,
}

impl StorageClient for CountingStorage {
    fn list_objects(&self, _bucket: &str) -> Result<Vec<String>, AppError> {
        Ok(self.objects.clone())
    }

    fn upload_file(&self, _source: &Path, _target: &ObjectPath) -> Result<String, AppError> {
        unreachable!()
    }

    fn move_object(
        &self,
        _source: &ObjectPath,
        _target: &ObjectPath,
        _expected_source_generation: Option<&str>,
    ) -> Result<String, AppError> {
        self.move_calls.set(self.move_calls.get() + 1);
        Ok("generation".to_string())
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

fn object_name_with_bytes(length: usize) -> String {
    let mut object = "é".repeat(length / 2);
    if object.len() < length {
        object.push('a');
    }
    object
}

#[test]
fn rejects_object_names_that_would_overflow_temporary_staging() {
    let suffix_len = temporary_suffix().len();
    let source = ObjectPath {
        bucket: "bucket".to_string(),
        object: object_name_with_bytes(MAX_OBJECT_NAME_BYTES - suffix_len + 1),
    };

    let error = temporary_path(&source).unwrap_err();

    assert!(matches!(error, AppError::Message(message) if message.contains("temporary staging")));
}

#[test]
fn accepts_object_names_at_the_temporary_staging_limit() {
    let suffix_len = temporary_suffix().len();
    let source = ObjectPath {
        bucket: "bucket".to_string(),
        object: object_name_with_bytes(MAX_OBJECT_NAME_BYTES - suffix_len),
    };

    let temporary = temporary_path(&source).unwrap();

    assert_eq!(temporary.object.len(), MAX_OBJECT_NAME_BYTES);
}

#[cfg(unix)]
#[test]
fn rejects_normalized_targets_before_remote_moves() {
    // This canonical sequence expands under NFC, so the final target can exceed the byte limit.
    let expanding_sequence = "\u{1d1bc}\u{1d16f}".repeat(13);
    let suffix_len = temporary_suffix().len();
    let source = ObjectPath {
        bucket: "bucket".to_string(),
        object: format!(
            "{}{}",
            "a".repeat(MAX_OBJECT_NAME_BYTES - suffix_len - expanding_sequence.len()),
            expanding_sequence
        ),
    };
    let target = ObjectPath {
        bucket: source.bucket.clone(),
        object: normalized(&source.object),
    };
    let ssh_directory = tempdir().unwrap();
    let ssh_path = ssh_directory.path().join("ssh");
    fs::write(&ssh_path, "#!/bin/sh\nexit 0\n").unwrap();
    use std::os::unix::fs::PermissionsExt;
    let mut permissions = fs::metadata(&ssh_path).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&ssh_path, permissions).unwrap();
    let _path_guard = PathGuard::prepend(ssh_directory.path());
    let storage = CountingStorage {
        objects: vec![source.uri()],
        move_calls: Cell::new(0),
    };
    let interrupt = crate::InterruptFlag::from_atomic(Arc::new(AtomicBool::new(false)));

    assert_eq!(source.object.len() + suffix_len, MAX_OBJECT_NAME_BYTES);
    assert!(target.object.len() > MAX_OBJECT_NAME_BYTES);

    let result = run(&Cloud::new(), &storage, &interrupt, "project", "bucket");

    assert!(
        matches!(result, Err(AppError::Message(message)) if message.contains("normalized target"))
    );
    assert_eq!(storage.move_calls.get(), 0);
}
