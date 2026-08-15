use crate::InterruptFlag;
use crate::cloud::Cloud;
use crate::error::AppError;
use crate::normalization_plan;
use crate::object_move;
use crate::storage::{ObjectPath, StorageClient};
use crate::transaction::{RemoteChange, rollback_moves};

#[cfg(test)]
pub(crate) use crate::object_path::MAX_OBJECT_NAME_BYTES;

const TEMPORARY_SUFFIX_PREFIX: &str = ".task-googlecloud-";

#[derive(Clone, Debug)]
struct PlannedMove {
    source: ObjectPath,
    target: ObjectPath,
    temporary: ObjectPath,
}

pub fn run<S: StorageClient>(
    cloud: &Cloud,
    storage: &S,
    interrupt: &InterruptFlag,
    project: &str,
    bucket: &str,
) -> Result<(), AppError> {
    cloud.login()?;
    cloud.set_project(project)?;
    storage.with_bucket_locks(&[bucket], || {
        let files = storage.list_objects(bucket)?;
        let plan = normalization_plan::build(&files)?;
        let moves = plan_moves(plan)?;
        process_moves_unlocked(storage, interrupt, &moves)
    })
}

fn plan_moves(plan: Vec<normalization_plan::Entry>) -> Result<Vec<PlannedMove>, AppError> {
    plan.into_iter()
        .filter(|entry| entry.source != entry.target)
        .map(|entry| {
            let source = ObjectPath::parse(&entry.source)?;
            let target = ObjectPath::parse(&entry.target)?;
            target.validate_name_length("normalized target")?;
            let temporary = temporary_path(&source)?;
            Ok(PlannedMove {
                source,
                target,
                temporary,
            })
        })
        .collect()
}

pub fn process_moves<S: StorageClient>(
    storage: &S,
    interrupt: &InterruptFlag,
    moves: Vec<(ObjectPath, ObjectPath, ObjectPath)>,
) -> Result<(), AppError> {
    let moves = moves
        .into_iter()
        .map(|(source, target, temporary)| PlannedMove {
            source,
            target,
            temporary,
        })
        .collect::<Vec<_>>();
    let bucket_names = moves
        .iter()
        .flat_map(|planned| {
            [
                planned.source.bucket.clone(),
                planned.target.bucket.clone(),
                planned.temporary.bucket.clone(),
            ]
        })
        .collect::<Vec<_>>();
    let buckets = bucket_names.iter().map(String::as_str).collect::<Vec<_>>();
    storage.with_bucket_locks(&buckets, || {
        process_moves_unlocked(storage, interrupt, &moves)
    })
}

fn process_moves_unlocked<S: StorageClient>(
    storage: &S,
    interrupt: &InterruptFlag,
    moves: &[PlannedMove],
) -> Result<(), AppError> {
    let mut staged = Vec::new();
    let mut finalized = Vec::new();

    let operation = (|| {
        for planned in moves {
            let generation = object_move::execute(
                storage,
                interrupt,
                &planned.source,
                &planned.temporary,
                None,
            )?;
            staged.push(RemoteChange {
                source: planned.source.clone(),
                target: planned.temporary.clone(),
                generation,
            });
            interrupt.check()?;
        }

        for (change, planned) in staged.iter().zip(moves) {
            let generation = object_move::execute(
                storage,
                interrupt,
                &change.target,
                &planned.target,
                Some(&change.generation),
            )?;
            finalized.push(RemoteChange {
                source: change.source.clone(),
                target: planned.target.clone(),
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
    let temporary = ObjectPath::from_parts(&source.bucket, &format!("{}{}", source.object, suffix));
    temporary.validate_name_length("temporary staging")?;
    Ok(temporary)
}

fn temporary_suffix() -> String {
    format!("{TEMPORARY_SUFFIX_PREFIX}{}", uuid::Uuid::new_v4().simple())
}

#[cfg(test)]
#[path = "../tests/unit/normalize.rs"]
mod tests;
