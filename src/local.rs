use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};

use crate::InterruptFlag;
use crate::atomic_rename::{
    DirectoryIdentity, FileIdentity, rename_without_overwrite,
    rename_without_overwrite_with_identity,
};
use crate::error::AppError;
use crate::normalization_plan::Entry;

#[derive(Clone, Debug)]
struct RenameRecord {
    source: PathBuf,
    target: PathBuf,
}

/// Applies the planned local renames without overwriting a target.
///
/// On failure, already completed renames are rolled back in reverse order.
pub fn apply_normalization(
    root: &Path,
    entries: &[Entry],
    interrupt: &InterruptFlag,
) -> Result<HashMap<PathBuf, PathBuf>, AppError> {
    apply_normalization_with_identity(root, entries, None, interrupt)
}

pub(crate) fn apply_normalization_with_identity(
    root: &Path,
    entries: &[Entry],
    expected_root: Option<DirectoryIdentity>,
    interrupt: &InterruptFlag,
) -> Result<HashMap<PathBuf, PathBuf>, AppError> {
    apply_normalization_with_path_identities(root, entries, expected_root, None, None, interrupt)
}

pub(crate) fn apply_normalization_with_path_identities(
    root: &Path,
    entries: &[Entry],
    expected_root: Option<DirectoryIdentity>,
    expected_files: Option<&HashMap<PathBuf, FileIdentity>>,
    expected_directories: Option<&HashMap<PathBuf, DirectoryIdentity>>,
    interrupt: &InterruptFlag,
) -> Result<HashMap<PathBuf, PathBuf>, AppError> {
    // Preserve the planned identities so rollback can refuse to overwrite a
    // path that another process replaced while this transaction was running.
    let mut renamed = Vec::new();
    let operation = (|| {
        for entry in entries {
            let source = PathBuf::from(&entry.source);
            let target = PathBuf::from(&entry.target);
            if source == target {
                continue;
            }
            rename(
                root,
                expected_root.clone(),
                expected_files.and_then(|files| files.get(&source).cloned()),
                expected_directories.and_then(|directories| {
                    source
                        .parent()
                        .and_then(|parent| directories.get(parent).cloned())
                }),
                expected_directories.and_then(|directories| {
                    target
                        .parent()
                        .and_then(|parent| directories.get(parent).cloned())
                }),
                &source,
                &target,
            )?;
            renamed.push(RenameRecord { source, target });
            interrupt.check()?;
        }
        Ok::<(), AppError>(())
    })();

    if let Err(error) = operation {
        return Err(AppError::rollback(
            error,
            rollback(
                root,
                expected_root.clone(),
                expected_files,
                expected_directories,
                &renamed,
            ),
        ));
    }

    Ok(entries
        .iter()
        .map(|entry| (PathBuf::from(&entry.source), PathBuf::from(&entry.target)))
        .collect())
}

/// Attempts to restore each `(source, target)` pair produced by normalization.
pub fn rollback_normalization(root: &Path, entries: &[(PathBuf, PathBuf)]) -> Vec<AppError> {
    rollback_normalization_with_identity(root, None, entries)
}

pub(crate) fn rollback_normalization_with_identity(
    root: &Path,
    expected_root: Option<DirectoryIdentity>,
    entries: &[(PathBuf, PathBuf)],
) -> Vec<AppError> {
    rollback_normalization_with_path_identities(root, expected_root, None, None, entries)
}

pub(crate) fn rollback_normalization_with_path_identities(
    root: &Path,
    expected_root: Option<DirectoryIdentity>,
    expected_files: Option<&HashMap<PathBuf, FileIdentity>>,
    expected_directories: Option<&HashMap<PathBuf, DirectoryIdentity>>,
    entries: &[(PathBuf, PathBuf)],
) -> Vec<AppError> {
    let records = entries
        .iter()
        .map(|(source, target)| RenameRecord {
            source: source.clone(),
            target: target.clone(),
        })
        .collect::<Vec<_>>();
    rollback(
        root,
        expected_root.clone(),
        expected_files,
        expected_directories,
        &records,
    )
}

fn rollback(
    root: &Path,
    expected_root: Option<DirectoryIdentity>,
    expected_files: Option<&HashMap<PathBuf, FileIdentity>>,
    expected_directories: Option<&HashMap<PathBuf, DirectoryIdentity>>,
    entries: &[RenameRecord],
) -> Vec<AppError> {
    let mut errors = Vec::new();
    // Reverse order restores chained renames without making an earlier source
    // name collide with a later rename that has not been undone yet.
    for entry in entries.iter().rev() {
        if entry.source == entry.target {
            continue;
        }
        if let Err(error) = rename(
            root,
            expected_root.clone(),
            expected_files.and_then(|files| files.get(&entry.source).cloned()),
            expected_directories.and_then(|directories| {
                entry
                    .target
                    .parent()
                    .and_then(|parent| directories.get(parent).cloned())
            }),
            expected_directories.and_then(|directories| {
                entry
                    .source
                    .parent()
                    .and_then(|parent| directories.get(parent).cloned())
            }),
            &entry.target,
            &entry.source,
        ) {
            if matches!(&error, AppError::Io(error) if error.kind() == io::ErrorKind::AlreadyExists)
            {
                errors.push(
                    io::Error::new(io::ErrorKind::AlreadyExists, "source and target both exist")
                        .into(),
                );
            } else if matches!(&error, AppError::Io(error) if error.kind() == io::ErrorKind::NotFound)
            {
                errors
                    .push(io::Error::new(io::ErrorKind::NotFound, "target does not exist").into());
            } else {
                errors.push(error);
            }
        }
    }
    errors
}

/// Converts a local path to the UTF-8 object name expected by the API.
pub fn path_string(path: &Path) -> Result<String, AppError> {
    path.to_str()
        .map(str::to_string)
        .ok_or_else(|| AppError::Message(format!("Path is not valid UTF-8: {path:?}")))
}

fn rename(
    root: &Path,
    expected_root: Option<DirectoryIdentity>,
    expected_source: Option<FileIdentity>,
    expected_source_parent: Option<DirectoryIdentity>,
    expected_target_parent: Option<DirectoryIdentity>,
    source: &Path,
    target: &Path,
) -> Result<(), AppError> {
    match expected_root {
        Some(expected_root) => rename_without_overwrite_with_identity(
            root,
            Some(expected_root),
            expected_source,
            expected_source_parent,
            expected_target_parent,
            source,
            target,
        ),
        None => rename_without_overwrite(root, source, target),
    }
}

#[cfg(test)]
#[path = "../tests/unit/local.rs"]
mod tests;
