use crate::InterruptFlag;
use crate::cloud::Cloud;
use crate::error::AppError;
use crate::normalization_plan;
use crate::object_move;
use crate::storage::{MAX_OBJECT_NAME_BYTES, ObjectPath, StorageClient};
use crate::transaction::{RemoteChange, rollback_moves};

const TEMPORARY_SUFFIX_PREFIX: &str = ".task-googlecloud-";

pub fn run<S: StorageClient>(
    cloud: &Cloud,
    storage: &S,
    interrupt: &InterruptFlag,
    project: &str,
    bucket: &str,
) -> Result<(), AppError> {
    cloud.login()?;
    cloud.set_project(project)?;
    let files = storage.list_objects(bucket)?;
    let plan = normalization_plan::build(&files)?;
    let moves = plan_moves(plan)?;
    process_moves(storage, interrupt, moves)
}

fn plan_moves(
    plan: Vec<normalization_plan::Entry>,
) -> Result<Vec<(ObjectPath, ObjectPath, ObjectPath)>, AppError> {
    plan.into_iter()
        .filter(|entry| entry.source != entry.target)
        .map(|entry| {
            let source = ObjectPath::parse(&entry.source)?;
            let target = ObjectPath::parse(&entry.target)?;
            validate_target_path(&target)?;
            let temporary = temporary_path(&source)?;
            Ok((source, target, temporary))
        })
        .collect()
}

fn validate_target_path(target: &ObjectPath) -> Result<(), AppError> {
    if target.object.len() > MAX_OBJECT_NAME_BYTES {
        return Err(AppError::Message(format!(
            "Object name is too long for normalized target: {}",
            target.uri()
        )));
    }
    Ok(())
}

pub fn process_moves<S: StorageClient>(
    storage: &S,
    interrupt: &InterruptFlag,
    moves: Vec<(ObjectPath, ObjectPath, ObjectPath)>,
) -> Result<(), AppError> {
    let mut staged = Vec::new();
    let mut finalized = Vec::new();

    let operation = (|| {
        for (source, _target, temporary) in &moves {
            let generation = object_move::execute(storage, interrupt, source, temporary, None)?;
            staged.push(RemoteChange {
                source: source.clone(),
                target: temporary.clone(),
                generation,
            });
            interrupt.check()?;
        }

        for (change, (_, target, _)) in staged.iter().zip(&moves) {
            let generation = object_move::execute(
                storage,
                interrupt,
                &change.target,
                target,
                Some(&change.generation),
            )?;
            finalized.push(RemoteChange {
                source: change.source.clone(),
                target: target.clone(),
                generation,
            });
            interrupt.check()?;
        }
        Ok::<(), AppError>(())
    })();

    match operation {
        Ok(()) => Ok(()),
        Err(error) => {
            interrupt.clear_for_rollback();
            Err(AppError::rollback(
                error,
                rollback_moves(storage, &staged, &finalized),
            ))
        }
    }
}

fn temporary_path(source: &ObjectPath) -> Result<ObjectPath, AppError> {
    let suffix = temporary_suffix();
    if source.object.len() + suffix.len() > MAX_OBJECT_NAME_BYTES {
        return Err(AppError::Message(format!(
            "Object name is too long for temporary staging: {}",
            source.uri()
        )));
    }

    Ok(ObjectPath {
        bucket: source.bucket.clone(),
        object: format!("{}{}", source.object, suffix),
    })
}

fn temporary_suffix() -> String {
    format!("{TEMPORARY_SUFFIX_PREFIX}{}", uuid::Uuid::new_v4().simple())
}

#[cfg(test)]
#[path = "../tests/unit/normalize.rs"]
mod tests;
