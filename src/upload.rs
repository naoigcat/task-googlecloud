use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::{Path, PathBuf};

use crate::InterruptFlag;
use crate::atomic_rename::{
    DirectoryIdentity, FileIdentity, directory_identity_from_path, file_identity_from_path,
};
use crate::cloud::Cloud;
use crate::error::AppError;
use crate::local;
use crate::normalization_plan;
use crate::object_move;
use crate::storage::{ObjectPath, StorageClient, UPLOAD_ROOT};
use crate::transaction::{RemoteChange, RemoteTransaction};
use crate::upload_source::UploadSourceIdentity;

#[cfg(test)]
pub(crate) use crate::object_path::MAX_OBJECT_NAME_BYTES;

/// Normalizes discovered local filenames, uploads them to per-directory
/// buckets, and rolls back local and remote changes if the transaction fails.
pub fn run<S: StorageClient>(
    cloud: &Cloud,
    storage: &S,
    interrupt: &InterruptFlag,
    project: &str,
) -> Result<(), AppError> {
    let discovery = discover_uploads(Path::new(UPLOAD_ROOT))?;
    let files_by_directory = discovery.files_by_directory;
    let expected_root = discovery.root_identity.clone();
    let directory_identities = discovery.directory_identities;
    let file_identities = discovery.file_identities;
    storage.set_upload_root_identity(expected_root.clone())?;
    let bucket_names = files_by_directory
        .keys()
        .map(|directory| {
            directory
                .file_name()
                .and_then(|name| name.to_str())
                .map(str::to_string)
                .ok_or_else(|| {
                    AppError::Message(format!("Directory is not valid UTF-8: {directory:?}"))
                })
        })
        .collect::<Result<Vec<_>, _>>()?;
    let buckets = bucket_names.iter().map(String::as_str).collect::<Vec<_>>();

    storage.with_bucket_locks(&buckets, || {
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
        // Derive every object name while the run is still a no-op, so a name Cloud
        // Storage cannot accept stops it before any file is renamed or uploaded.
        let planned_names = plan
            .iter()
            .map(|entry| (PathBuf::from(&entry.source), PathBuf::from(&entry.target)))
            .collect::<HashMap<_, _>>();
        let uploads = plan_uploads_with_identities(
            &files_by_directory,
            &planned_names,
            &file_identities,
            &directory_identities,
            &staging_prefix(),
        )?;

        cloud.login()?;
        cloud.set_project(project)?;

        let normalized_files = local::apply_normalization_with_path_identities(
            Path::new(UPLOAD_ROOT),
            &plan,
            expected_root.clone(),
            Some(&file_identities),
            Some(&directory_identities),
            interrupt,
        )?;

        let result = upload_planned_files_unlocked(storage, interrupt, &uploads);
        match result {
            Ok(()) => Ok(()),
            Err(error) => {
                let errors = local::rollback_normalization_with_path_identities(
                    Path::new(UPLOAD_ROOT),
                    expected_root.clone(),
                    Some(&file_identities),
                    Some(&directory_identities),
                    &normalized_files
                        .iter()
                        .map(|(source, target)| (source.clone(), target.clone()))
                        .collect::<Vec<_>>(),
                );
                Err(AppError::rollback(error, errors))
            }
        }
    })
}

struct UploadDiscovery {
    files_by_directory: BTreeMap<PathBuf, Vec<PathBuf>>,
    // Keep discovery tied to the same directory for every later filesystem access.
    root_identity: Option<DirectoryIdentity>,
    directory_identities: HashMap<PathBuf, DirectoryIdentity>,
    file_identities: HashMap<PathBuf, FileIdentity>,
}

pub fn upload_files_by_directory(root: &Path) -> Result<BTreeMap<PathBuf, Vec<PathBuf>>, AppError> {
    Ok(discover_uploads(root)?.files_by_directory)
}

fn discover_uploads(root: &Path) -> Result<UploadDiscovery, AppError> {
    let mut directories = BTreeMap::new();
    let root_metadata = match fs::symlink_metadata(root) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(UploadDiscovery {
                files_by_directory: directories,
                root_identity: None,
                directory_identities: HashMap::new(),
                file_identities: HashMap::new(),
            });
        }
        Err(error) => return Err(error.into()),
    };
    if root_metadata.file_type().is_symlink() {
        return Ok(UploadDiscovery {
            files_by_directory: directories,
            root_identity: None,
            directory_identities: HashMap::new(),
            file_identities: HashMap::new(),
        });
    }
    if !root_metadata.file_type().is_dir() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotADirectory,
            format!("Upload root is not a directory: {root:?}"),
        )
        .into());
    }
    let root_identity = directory_identity_from_path(root).map_err(AppError::from)?;
    let mut directory_identities = HashMap::new();
    let mut file_identities = HashMap::new();
    // Discovery records identities as well as paths because a later open by
    // pathname must not silently accept a replacement file or directory.
    for directory in fs::read_dir(root)? {
        let directory = directory?.path();
        let directory_metadata = fs::symlink_metadata(&directory)?;
        if !directory_metadata.file_type().is_dir() {
            continue;
        }
        directory_identities.insert(
            directory.clone(),
            directory_identity_from_path(&directory).map_err(AppError::from)?,
        );
        let mut files = Vec::new();
        for entry in fs::read_dir(&directory)? {
            let file = entry?.path();
            let file_metadata = fs::symlink_metadata(&file)?;
            if file_metadata.file_type().is_file()
                && file
                    .file_name()
                    .is_some_and(|name| !name.to_string_lossy().starts_with('.'))
            {
                file_identities.insert(
                    file.clone(),
                    file_identity_from_path(&file).map_err(AppError::from)?,
                );
                files.push(file);
            }
        }
        files.sort();
        directories.insert(directory, files);
    }
    let current_metadata = fs::symlink_metadata(root)?;
    if !current_metadata.file_type().is_dir()
        || !directory_identity_from_path(root)
            .map_err(AppError::from)?
            .eq(&root_identity)
    {
        return Err(AppError::Message(format!(
            "Upload root changed during discovery: {root:?}"
        )));
    }
    Ok(UploadDiscovery {
        files_by_directory: directories,
        root_identity: Some(root_identity),
        directory_identities,
        file_identities,
    })
}

#[cfg(test)]
fn is_real_directory(path: &Path) -> Result<bool, AppError> {
    Ok(fs::symlink_metadata(path)?.file_type().is_dir())
}

#[cfg(test)]
fn is_regular_file(path: &Path) -> Result<bool, AppError> {
    Ok(fs::symlink_metadata(path)?.file_type().is_file())
}

#[derive(Clone, Debug)]
struct PlannedUpload {
    file: PathBuf,
    staging: ObjectPath,
    target: ObjectPath,
    source_identity: Option<UploadSourceIdentity>,
}

#[derive(Clone, Debug)]
struct PlannedDirectoryUploads {
    directory: PathBuf,
    uploads: Vec<PlannedUpload>,
}

fn staging_prefix() -> String {
    // A run-specific prefix keeps abandoned uploads distinguishable during
    // manual recovery after a forced termination.
    format!(
        ".task-googlecloud-staging/{}",
        uuid::Uuid::new_v4().simple()
    )
}

#[cfg(test)]
fn plan_uploads(
    files_by_directory: &BTreeMap<PathBuf, Vec<PathBuf>>,
    normalized_files: &HashMap<PathBuf, PathBuf>,
    staging_prefix: &str,
) -> Result<Vec<PlannedDirectoryUploads>, AppError> {
    plan_uploads_with_identities(
        files_by_directory,
        normalized_files,
        &HashMap::new(),
        &HashMap::new(),
        staging_prefix,
    )
}

fn plan_uploads_with_identities(
    files_by_directory: &BTreeMap<PathBuf, Vec<PathBuf>>,
    normalized_files: &HashMap<PathBuf, PathBuf>,
    file_identities: &HashMap<PathBuf, FileIdentity>,
    directory_identities: &HashMap<PathBuf, DirectoryIdentity>,
    staging_prefix: &str,
) -> Result<Vec<PlannedDirectoryUploads>, AppError> {
    let mut planned = Vec::new();
    for (directory, files) in files_by_directory {
        let bucket = directory
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| {
                AppError::Message(format!("Directory is not valid UTF-8: {directory:?}"))
            })?;
        let mut uploads = Vec::new();
        for file in files {
            // Keep the original discovered identity attached to the normalized
            // path so a replacement cannot be uploaded under the old name.
            let normalized_file = normalized_files.get(file).ok_or_else(|| {
                AppError::Message(format!("Missing normalized path for {file:?}"))
            })?;
            let object_name = normalized_file
                .file_name()
                .and_then(|name| name.to_str())
                .ok_or_else(|| {
                    AppError::Message(format!("File is not valid UTF-8: {normalized_file:?}"))
                })?;
            let source_identity = if file_identities.is_empty() && directory_identities.is_empty() {
                None
            } else {
                Some(UploadSourceIdentity {
                    file: file_identities.get(file).cloned().ok_or_else(|| {
                        AppError::Message(format!("Missing file identity for {file:?}"))
                    })?,
                    directory: directory_identities
                        .get(directory)
                        .cloned()
                        .ok_or_else(|| {
                            AppError::Message(format!(
                                "Missing directory identity for {directory:?}"
                            ))
                        })?,
                })
            };
            uploads.push(PlannedUpload {
                file: normalized_file.clone(),
                staging: staging_path(bucket, staging_prefix, object_name)?,
                target: ObjectPath::parse(&format!("gs://{bucket}/{object_name}"))?,
                source_identity,
            });
        }
        planned.push(PlannedDirectoryUploads {
            directory: directory.clone(),
            uploads,
        });
    }
    Ok(planned)
}

#[cfg(test)]
fn upload_planned_files<S: StorageClient>(
    storage: &S,
    interrupt: &InterruptFlag,
    uploads: &[PlannedDirectoryUploads],
) -> Result<(), AppError> {
    let bucket_names = uploads
        .iter()
        .flat_map(|planned| {
            planned
                .uploads
                .iter()
                .map(|upload| upload.target.bucket.clone())
        })
        .collect::<Vec<_>>();
    let buckets = bucket_names.iter().map(String::as_str).collect::<Vec<_>>();
    storage.with_bucket_locks(&buckets, || {
        upload_planned_files_unlocked(storage, interrupt, uploads)
    })
}

fn upload_planned_files_unlocked<S: StorageClient>(
    storage: &S,
    interrupt: &InterruptFlag,
    uploads: &[PlannedDirectoryUploads],
) -> Result<(), AppError> {
    let mut transaction = RemoteTransaction::new();

    let operation = (|| {
        // Upload everything to private staging names first. Finalization is
        // then generation-guarded, so partial runs can be identified and
        // rolled back without touching another writer's object.
        for planned in uploads {
            println!("{}", planned.directory.display());
            for upload in &planned.uploads {
                let generation = match upload.source_identity.as_ref() {
                    Some(identity) => object_move::execute_upload_with_identity(
                        storage,
                        interrupt,
                        &upload.file,
                        &upload.staging,
                        Some(identity.clone()),
                    )?,
                    None => object_move::execute_upload(
                        storage,
                        interrupt,
                        &upload.file,
                        &upload.staging,
                    )?,
                };
                transaction.stage(RemoteChange {
                    source: upload.staging.clone(),
                    target: upload.target.clone(),
                    generation,
                });
                interrupt.check()?;
            }
        }

        transaction.finalize(interrupt, |_, change| {
            let generation = object_move::execute(
                storage,
                interrupt,
                &change.source,
                &change.target,
                Some(&change.generation),
            )?;
            Ok(RemoteChange {
                source: change.source.clone(),
                target: change.target.clone(),
                generation,
            })
        })?;
        Ok::<(), AppError>(())
    })();

    match operation {
        Ok(()) => Ok(()),
        Err(error) => {
            interrupt.clear_for_rollback();
            Err(AppError::rollback(
                error,
                rollback_remote(storage, transaction.staged(), transaction.finalized()),
            ))
        }
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
    let staging = ObjectPath::from_parts(bucket, &staging_name);
    staging.validate_name_length("temporary staging")?;
    Ok(staging)
}

/// Restores finalized remote objects and removes uploads that never finalized.
///
/// Finalized changes are rolled back first because their staging objects have
/// been consumed by the move; remaining staged changes can then be deleted.
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
#[path = "../tests/unit/upload.rs"]
mod tests;
