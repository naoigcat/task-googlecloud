use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::{Path, PathBuf};

use crate::InterruptFlag;
use crate::cloud::Cloud;
use crate::error::AppError;
use crate::local;
use crate::normalization_plan;
use crate::object_move;
use crate::storage::{ObjectPath, StorageClient};

#[derive(Clone, Debug)]
pub struct RemoteChange {
    pub source: ObjectPath,
    pub target: ObjectPath,
    pub generation: String,
}

pub fn run<S: StorageClient>(
    cloud: &Cloud,
    storage: &S,
    interrupt: &InterruptFlag,
    project: &str,
) -> Result<(), AppError> {
    let files_by_directory = upload_files_by_directory(Path::new("uploads"))?;
    let files = files_by_directory
        .values()
        .flatten()
        .cloned()
        .collect::<Vec<_>>();
    let names = files
        .iter()
        .map(|file| local::path_string(file))
        .collect::<Result<Vec<_>, _>>()?;
    let plan = normalization_plan::build(&names)?;

    cloud.login()?;
    cloud.set_project(project)?;

    let normalized_files = local::apply_normalization(&plan, interrupt)?;

    let result =
        upload_normalized_files(storage, interrupt, &files_by_directory, &normalized_files);
    match result {
        Ok(()) => Ok(()),
        Err(error) => {
            let errors = local::rollback_normalization(
                &normalized_files
                    .iter()
                    .map(|(source, target)| (source.clone(), target.clone()))
                    .collect::<Vec<_>>(),
            );
            Err(AppError::rollback(error, errors))
        }
    }
}

pub fn upload_files_by_directory(root: &Path) -> Result<BTreeMap<PathBuf, Vec<PathBuf>>, AppError> {
    let mut directories = BTreeMap::new();
    if !root.exists() {
        return Ok(directories);
    }
    for directory in fs::read_dir(root)? {
        let directory = directory?.path();
        if !directory.is_dir() {
            continue;
        }
        let mut files = fs::read_dir(&directory)?
            .map(|entry| entry.map(|entry| entry.path()))
            .collect::<Result<Vec<_>, _>>()?;
        files.retain(|file| {
            file.is_file()
                && file
                    .file_name()
                    .is_some_and(|name| !name.to_string_lossy().starts_with('.'))
        });
        files.sort();
        directories.insert(directory, files);
    }
    Ok(directories)
}

fn upload_normalized_files<S: StorageClient>(
    storage: &S,
    interrupt: &InterruptFlag,
    files_by_directory: &BTreeMap<PathBuf, Vec<PathBuf>>,
    normalized_files: &HashMap<PathBuf, PathBuf>,
) -> Result<(), AppError> {
    let staging_prefix = format!(
        ".task-googlecloud-staging/{}",
        uuid::Uuid::new_v4().simple()
    );
    let mut staged = Vec::new();
    let mut finalized = Vec::new();

    let operation = (|| {
        for (directory, files) in files_by_directory {
            println!("{}", directory.display());
            let bucket = directory
                .file_name()
                .and_then(|name| name.to_str())
                .ok_or_else(|| {
                    AppError::Message(format!("Directory is not valid UTF-8: {directory:?}"))
                })?;
            for file in files {
                let normalized_file = normalized_files.get(file).ok_or_else(|| {
                    AppError::Message(format!("Missing normalized path for {file:?}"))
                })?;
                let object_name = normalized_file
                    .file_name()
                    .and_then(|name| name.to_str())
                    .ok_or_else(|| {
                        AppError::Message(format!("File is not valid UTF-8: {normalized_file:?}"))
                    })?;
                let staging =
                    ObjectPath::parse(&format!("gs://{bucket}/{staging_prefix}/{object_name}"))?;
                let final_path = ObjectPath::parse(&format!("gs://{bucket}/{object_name}"))?;
                let generation =
                    object_move::execute_upload(storage, interrupt, normalized_file, &staging)?;
                staged.push(RemoteChange {
                    source: staging.clone(),
                    target: final_path.clone(),
                    generation,
                });
                interrupt.check()?;
            }
        }

        for change in &staged {
            let generation =
                object_move::execute(storage, interrupt, &change.source, &change.target)?;
            finalized.push(RemoteChange {
                source: change.source.clone(),
                target: change.target.clone(),
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
            rollback_remote(storage, &staged, &finalized),
        )),
    }
}

pub fn rollback_remote<S: StorageClient>(
    storage: &S,
    staged: &[RemoteChange],
    finalized: &[RemoteChange],
) -> Vec<AppError> {
    let mut errors = Vec::new();
    for change in finalized.iter().rev() {
        match storage.rollback_object(&change.source, &change.target, &change.generation) {
            Ok(restored_generation) => {
                if let Err(error) = storage.cleanup_object(&change.source, &restored_generation) {
                    errors.push(error);
                }
            }
            Err(error) => errors.push(error),
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
        if let Err(error) = storage.cleanup_object(&change.source, &change.generation) {
            errors.push(error);
        }
    }
    errors
}
