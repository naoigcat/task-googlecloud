use crate::InterruptFlag;
use crate::error::AppError;
use crate::storage::{ObjectPath, StorageClient};
use crate::upload_source::UploadSourceIdentity;

pub fn execute<S: StorageClient>(
    storage: &S,
    interrupt: &InterruptFlag,
    source: &ObjectPath,
    target: &ObjectPath,
    expected_source_generation: Option<&str>,
) -> Result<String, AppError> {
    interrupt.check()?;
    match storage.move_object(source, target, expected_source_generation) {
        Ok(generation) => Ok(generation),
        Err(error) => {
            if error.is_bucket_lock_conflict() {
                return Err(error);
            }
            if error.is_interrupted() {
                interrupt.clear_for_rollback();
            }
            match storage.confirm_move_after_failure(source, target, &error) {
                Ok(()) => Err(error),
                Err(recovery) => Err(recovery),
            }
        }
    }
}

pub fn execute_upload<S: StorageClient>(
    storage: &S,
    interrupt: &InterruptFlag,
    source: &std::path::Path,
    target: &ObjectPath,
) -> Result<String, AppError> {
    execute_upload_with_identity(storage, interrupt, source, target, None)
}

pub(crate) fn execute_upload_with_identity<S: StorageClient>(
    storage: &S,
    interrupt: &InterruptFlag,
    source: &std::path::Path,
    target: &ObjectPath,
    expected_source: Option<UploadSourceIdentity>,
) -> Result<String, AppError> {
    interrupt.check()?;
    match storage.upload_file_with_identity(source, target, expected_source) {
        Ok(generation) => Ok(generation),
        Err(error) => {
            if error.is_bucket_lock_conflict() {
                return Err(error);
            }
            if error.is_interrupted() {
                interrupt.clear_for_rollback();
            }
            match storage.confirm_write_after_failure(target, &error) {
                Ok(()) => Err(error),
                Err(recovery) => Err(recovery),
            }
        }
    }
}
