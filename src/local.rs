use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};

use crate::InterruptFlag;
use crate::atomic_rename::rename_without_overwrite;
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
    let mut renamed = Vec::new();
    let operation = (|| {
        for entry in entries {
            let source = PathBuf::from(&entry.source);
            let target = PathBuf::from(&entry.target);
            if source == target {
                continue;
            }
            rename_without_overwrite(root, &source, &target)?;
            renamed.push(RenameRecord { source, target });
            interrupt.check()?;
        }
        Ok::<(), AppError>(())
    })();

    if let Err(error) = operation {
        return Err(AppError::rollback(error, rollback(root, &renamed)));
    }

    Ok(entries
        .iter()
        .map(|entry| (PathBuf::from(&entry.source), PathBuf::from(&entry.target)))
        .collect())
}

pub fn rollback_normalization(root: &Path, entries: &[(PathBuf, PathBuf)]) -> Vec<AppError> {
    let records = entries
        .iter()
        .map(|(source, target)| RenameRecord {
            source: source.clone(),
            target: target.clone(),
        })
        .collect::<Vec<_>>();
    rollback(root, &records)
}

fn rollback(root: &Path, entries: &[RenameRecord]) -> Vec<AppError> {
    let mut errors = Vec::new();
    for entry in entries.iter().rev() {
        if entry.source == entry.target {
            continue;
        }
        if let Err(error) = rename_without_overwrite(root, &entry.target, &entry.source) {
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
