use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::{Path, PathBuf};

use crate::InterruptFlag;
use crate::cloud::Cloud;
use crate::error::AppError;
use crate::local;
use crate::normalization_plan;
use crate::object_move;
use crate::storage::{MAX_OBJECT_NAME_BYTES, ObjectPath, StorageClient, UPLOAD_ROOT};

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
    let files_by_directory = upload_files_by_directory(Path::new(UPLOAD_ROOT))?;
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
    let root_metadata = match fs::symlink_metadata(root) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(directories),
        Err(error) => return Err(error.into()),
    };
    if root_metadata.file_type().is_symlink() {
        return Ok(directories);
    }
    if !root_metadata.file_type().is_dir() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotADirectory,
            format!("Upload root is not a directory: {root:?}"),
        )
        .into());
    }
    for directory in fs::read_dir(root)? {
        let directory = directory?.path();
        if !is_real_directory(&directory) {
            continue;
        }
        let mut files = fs::read_dir(&directory)?
            .map(|entry| entry.map(|entry| entry.path()))
            .collect::<Result<Vec<_>, _>>()?;
        files.retain(|file| {
            is_regular_file(file)
                && file
                    .file_name()
                    .is_some_and(|name| !name.to_string_lossy().starts_with('.'))
        });
        files.sort();
        directories.insert(directory, files);
    }
    Ok(directories)
}

fn is_real_directory(path: &Path) -> bool {
    fs::symlink_metadata(path)
        .map(|metadata| metadata.file_type().is_dir())
        .unwrap_or(false)
}

fn is_regular_file(path: &Path) -> bool {
    fs::symlink_metadata(path)
        .map(|metadata| metadata.file_type().is_file())
        .unwrap_or(false)
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
                let staging = staging_path(bucket, &staging_prefix, object_name)?;
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

/// The staging prefix lengthens the object name, so a name that Cloud Storage
/// accepts on its own can still overflow the limit once staged.
fn staging_path(
    bucket: &str,
    staging_prefix: &str,
    object_name: &str,
) -> Result<ObjectPath, AppError> {
    let staging_name = format!("{staging_prefix}/{object_name}");
    if staging_name.len() > MAX_OBJECT_NAME_BYTES {
        return Err(AppError::Message(format!(
            "Object name is too long for temporary staging: gs://{bucket}/{object_name}"
        )));
    }
    ObjectPath::parse(&format!("gs://{bucket}/{staging_name}"))
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

#[cfg(test)]
mod tests {
    use super::{MAX_OBJECT_NAME_BYTES, staging_path};
    use crate::error::AppError;

    const STAGING_PREFIX: &str = ".task-googlecloud-staging/0123456789abcdef0123456789abcdef";

    fn object_name_with_bytes(length: usize) -> String {
        let mut object = "é".repeat(length / 2);
        if object.len() < length {
            object.push('a');
        }
        object
    }

    #[test]
    fn rejects_object_names_that_would_overflow_the_staging_prefix() {
        let object_name = object_name_with_bytes(MAX_OBJECT_NAME_BYTES - STAGING_PREFIX.len());

        let error = staging_path("bucket", STAGING_PREFIX, &object_name).unwrap_err();

        assert!(
            matches!(error, AppError::Message(message) if message.contains("temporary staging"))
        );
    }

    #[test]
    fn accepts_object_names_at_the_staging_limit() {
        let object_name = object_name_with_bytes(MAX_OBJECT_NAME_BYTES - STAGING_PREFIX.len() - 1);

        let staging = staging_path("bucket", STAGING_PREFIX, &object_name).unwrap();

        assert_eq!(staging.object.len(), MAX_OBJECT_NAME_BYTES);
    }
}
