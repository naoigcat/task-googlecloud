use crate::error::AppError;
use crate::storage::{ObjectPath, StorageClient};

#[derive(Clone, Debug)]
pub struct RemoteChange {
    pub source: ObjectPath,
    pub target: ObjectPath,
    pub generation: String,
}

pub(crate) fn rollback_moves<S: StorageClient>(
    storage: &S,
    staged: &[RemoteChange],
    finalized: &[RemoteChange],
) -> Vec<AppError> {
    let mut errors = Vec::new();
    for change in finalized.iter().rev() {
        if let Err(error) =
            storage.rollback_object(&change.source, &change.target, &change.generation)
        {
            errors.push(error);
        }
    }

    let finalized_sources = finalized
        .iter()
        .map(|change| &change.source)
        .collect::<Vec<_>>();
    for change in staged.iter().rev() {
        if finalized_sources.contains(&&change.source) {
            continue;
        }
        if let Err(error) =
            storage.rollback_object(&change.source, &change.target, &change.generation)
        {
            errors.push(error);
        }
    }
    errors
}
