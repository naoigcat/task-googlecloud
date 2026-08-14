use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};

use crate::InterruptFlag;
use crate::atomic_rename::{path_entry_exists, rename_without_overwrite, same_path_entry};
use crate::error::AppError;
use crate::normalization_plan::Entry;

#[derive(Clone, Debug)]
struct RenameRecord {
    source: PathBuf,
    target: PathBuf,
}

pub fn apply_normalization(
    entries: &[Entry],
    interrupt: &InterruptFlag,
) -> Result<HashMap<PathBuf, PathBuf>, AppError> {
    let mut renamed = Vec::new();
    for entry in entries {
        let source = PathBuf::from(&entry.source);
        let target = PathBuf::from(&entry.target);
        if source == target {
            continue;
        }
        if let Err(error) = rename_without_overwrite(&source, &target) {
            let rollback_errors = rollback(&renamed);
            return Err(AppError::rollback(error, rollback_errors));
        }
        renamed.push(RenameRecord { source, target });
        if let Err(error) = interrupt.check() {
            let rollback_errors = rollback(&renamed);
            return Err(AppError::rollback(error, rollback_errors));
        }
    }

    Ok(entries
        .iter()
        .map(|entry| (PathBuf::from(&entry.source), PathBuf::from(&entry.target)))
        .collect())
}

pub fn rollback_normalization(entries: &[(PathBuf, PathBuf)]) -> Vec<AppError> {
    let records = entries
        .iter()
        .map(|(source, target)| RenameRecord {
            source: source.clone(),
            target: target.clone(),
        })
        .collect::<Vec<_>>();
    rollback(&records)
}

fn rollback(entries: &[RenameRecord]) -> Vec<AppError> {
    let mut errors = Vec::new();
    for entry in entries.iter().rev() {
        if entry.source == entry.target {
            continue;
        }
        if path_entry_exists(&entry.source) && path_entry_exists(&entry.target) {
            match same_path_entry(&entry.source, &entry.target) {
                Ok(true) => continue,
                Ok(false) => errors.push(
                    io::Error::new(io::ErrorKind::AlreadyExists, "source and target both exist")
                        .into(),
                ),
                Err(error) => errors.push(error),
            }
            continue;
        }
        if !path_entry_exists(&entry.target) {
            errors.push(io::Error::new(io::ErrorKind::NotFound, "target does not exist").into());
            continue;
        }
        if let Err(error) = rename_without_overwrite(&entry.target, &entry.source) {
            errors.push(error);
        }
    }
    errors
}

pub fn path_string(path: &Path) -> Result<String, AppError> {
    path.to_str()
        .map(str::to_string)
        .ok_or_else(|| AppError::Message(format!("Path is not valid UTF-8: {path:?}")))
}
