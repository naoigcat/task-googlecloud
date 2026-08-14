use crate::InterruptFlag;
use crate::error::AppError;
use crate::storage::{ObjectPath, StorageClient};

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
        Err(error) => match storage.confirm_move_after_failure(source, target, &error) {
            Ok(()) => Err(error),
            Err(recovery) => Err(recovery),
        },
    }
}

pub fn execute_upload<S: StorageClient>(
    storage: &S,
    interrupt: &InterruptFlag,
    source: &std::path::Path,
    target: &ObjectPath,
) -> Result<String, AppError> {
    interrupt.check()?;
    match storage.upload_file(source, target) {
        Ok(generation) => Ok(generation),
        Err(error) => match storage.confirm_write_after_failure(target, &error) {
            Ok(()) => Err(error),
            Err(recovery) => Err(recovery),
        },
    }
}
