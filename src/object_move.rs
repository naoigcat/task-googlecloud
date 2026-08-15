use crate::InterruptFlag;
use crate::error::AppError;
use crate::storage::{ObjectPath, StorageClient};
use crate::upload_source::UploadSourceIdentity;

/// Executes one remote move and confirms object state when the request fails.
///
/// A failed HTTP response is not enough to prove that copy/delete did not run,
/// so the confirmation path distinguishes a safe retry from manual recovery.
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
        Err(error) if error.restored_move_generation().is_some() => {
            // Storage already restored this move and carries the new source
            // generation; another confirmation would hide that state.
            Err(error)
        }
        Err(error) => Err(confirmed_failure(interrupt, error, |operation| {
            storage.confirm_move_after_failure(source, target, operation)
        })),
    }
}

/// Uploads one local file and applies the same post-failure confirmation rules
/// as a remote move.
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
        Err(error) => Err(confirmed_failure(interrupt, error, |operation| {
            storage.confirm_write_after_failure(target, operation)
        })),
    }
}

fn confirmed_failure<F>(interrupt: &InterruptFlag, error: AppError, confirm: F) -> AppError
where
    F: FnOnce(&AppError) -> Result<(), AppError>,
{
    if error.is_bucket_lock_conflict() {
        return error;
    }
    if error.is_interrupted() {
        interrupt.clear_for_rollback();
    }
    // A failed request may have changed remote state, so confirmation must
    // happen before the original error is returned to the transaction.
    match confirm(&error) {
        Ok(()) => error,
        Err(recovery) => recovery,
    }
}
