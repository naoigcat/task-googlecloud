use crate::InterruptFlag;
use crate::cloud::Cloud;
use crate::error::AppError;
use crate::normalization_plan;
use crate::object_move;
use crate::storage::{ObjectPath, StorageClient};

#[derive(Clone, Debug)]
struct RemoteMove {
    source: ObjectPath,
    target: ObjectPath,
    generation: String,
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
    let files = storage.list_objects(bucket)?;
    let plan = normalization_plan::build(&files)?;
    let moves = plan
        .into_iter()
        .filter(|entry| entry.source != entry.target)
        .map(|entry| {
            Ok((
                ObjectPath::parse(&entry.source)?,
                ObjectPath::parse(&entry.target)?,
                temporary_path(&entry.source),
            ))
        })
        .collect::<Result<Vec<_>, AppError>>()?;
    process_moves(storage, interrupt, moves)
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
            let generation = object_move::execute(storage, interrupt, source, temporary)?;
            staged.push(RemoteMove {
                source: source.clone(),
                target: temporary.clone(),
                generation,
            });
            interrupt.check()?;
        }

        for (source, target, temporary) in &moves {
            let generation = object_move::execute(storage, interrupt, temporary, target)?;
            finalized.push(RemoteMove {
                source: source.clone(),
                target: target.clone(),
                generation,
            });
            interrupt.check()?;
        }
        Ok::<(), AppError>(())
    })();

    match operation {
        Ok(()) => Ok(()),
        Err(error) => Err(AppError::rollback(
            error,
            rollback(storage, &staged, &finalized),
        )),
    }
}

fn rollback<S: StorageClient>(
    storage: &S,
    staged: &[RemoteMove],
    finalized: &[RemoteMove],
) -> Vec<AppError> {
    let mut errors = Vec::new();
    for move_record in finalized.iter().rev() {
        if let Err(error) = storage.rollback_object(
            &move_record.source,
            &move_record.target,
            &move_record.generation,
        ) {
            errors.push(error);
        }
    }

    let finalized_sources = finalized
        .iter()
        .map(|record| &record.source)
        .collect::<Vec<_>>();
    for move_record in staged.iter().rev() {
        if finalized_sources.contains(&&move_record.source) {
            continue;
        }
        if let Err(error) = storage.rollback_object(
            &move_record.source,
            &move_record.target,
            &move_record.generation,
        ) {
            errors.push(error);
        }
    }
    errors
}

fn temporary_path(source: &str) -> ObjectPath {
    ObjectPath::parse(&format!(
        "{source}.task-googlecloud-{}",
        uuid::Uuid::new_v4().simple()
    ))
    .expect("source is already a valid Cloud Storage URI")
}
