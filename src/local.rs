use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};

use crate::InterruptFlag;
use crate::atomic_rename::{
    DirectoryIdentity, rename_without_overwrite, rename_without_overwrite_with_identity,
};
use crate::error::AppError;
use crate::normalization_plan::Entry;

#[derive(Clone, Debug)]
struct RenameRecord {
    source: PathBuf,
    target: PathBuf,
}

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
    let mut renamed = Vec::new();
    let operation = (|| {
        for entry in entries {
            let source = PathBuf::from(&entry.source);
            let target = PathBuf::from(&entry.target);
            if source == target {
                continue;
            }
            rename(root, expected_root, &source, &target)?;
            renamed.push(RenameRecord { source, target });
            interrupt.check()?;
        }
        Ok::<(), AppError>(())
    })();

    if let Err(error) = operation {
        return Err(AppError::rollback(
            error,
            rollback(root, expected_root, &renamed),
        ));
    }

    Ok(entries
        .iter()
        .map(|entry| (PathBuf::from(&entry.source), PathBuf::from(&entry.target)))
        .collect())
}

pub fn rollback_normalization(root: &Path, entries: &[(PathBuf, PathBuf)]) -> Vec<AppError> {
    rollback_normalization_with_identity(root, None, entries)
}

pub(crate) fn rollback_normalization_with_identity(
    root: &Path,
    expected_root: Option<DirectoryIdentity>,
    entries: &[(PathBuf, PathBuf)],
) -> Vec<AppError> {
    let records = entries
        .iter()
        .map(|(source, target)| RenameRecord {
            source: source.clone(),
            target: target.clone(),
        })
        .collect::<Vec<_>>();
    rollback(root, expected_root, &records)
}

fn rollback(
    root: &Path,
    expected_root: Option<DirectoryIdentity>,
    entries: &[RenameRecord],
) -> Vec<AppError> {
    let mut errors = Vec::new();
    for entry in entries.iter().rev() {
        if entry.source == entry.target {
            continue;
        }
        if let Err(error) = rename(root, expected_root, &entry.target, &entry.source) {
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

pub fn path_string(path: &Path) -> Result<String, AppError> {
    path.to_str()
        .map(str::to_string)
        .ok_or_else(|| AppError::Message(format!("Path is not valid UTF-8: {path:?}")))
}

fn rename(
    root: &Path,
    expected_root: Option<DirectoryIdentity>,
    source: &Path,
    target: &Path,
) -> Result<(), AppError> {
    match expected_root {
        Some(expected_root) => {
            rename_without_overwrite_with_identity(root, Some(expected_root), source, target)
        }
        None => rename_without_overwrite(root, source, target),
    }
}

#[cfg(test)]
#[path = "../tests/unit/local.rs"]
mod tests;
