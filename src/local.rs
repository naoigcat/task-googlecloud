use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};

use crate::InterruptFlag;
use crate::atomic_rename::{
    DirectoryIdentity, FileIdentity, RenameIdentity, rename_without_overwrite_with_identity,
};
use crate::error::AppError;
use crate::normalization_plan::Entry;

#[derive(Clone, Debug)]
struct RenameRecord {
    source: PathBuf,
    target: PathBuf,
}

struct RenameContext<'a> {
    root: &'a Path,
    expected_root: Option<DirectoryIdentity>,
    expected_files: Option<&'a HashMap<PathBuf, FileIdentity>>,
    expected_directories: Option<&'a HashMap<PathBuf, DirectoryIdentity>>,
}

impl<'a> RenameContext<'a> {
    fn new(
        root: &'a Path,
        expected_root: Option<DirectoryIdentity>,
        expected_files: Option<&'a HashMap<PathBuf, FileIdentity>>,
        expected_directories: Option<&'a HashMap<PathBuf, DirectoryIdentity>>,
    ) -> Self {
        Self {
            root,
            expected_root,
            expected_files,
            expected_directories,
        }
    }

    /// Applies the same identity checks to forward and rollback renames so a
    /// replacement cannot be treated differently in either transaction phase.
    fn rename(&self, source: &Path, target: &Path, renamed: &mut bool) -> Result<(), AppError> {
        self.rename_with_expected_file(source, target, source, renamed)
    }

    fn rename_with_expected_file(
        &self,
        source: &Path,
        target: &Path,
        expected_file: &Path,
        renamed: &mut bool,
    ) -> Result<(), AppError> {
        match self.expected_root.clone() {
            Some(expected_root) => rename_without_overwrite_with_identity(
                self.root,
                RenameIdentity {
                    root: Some(expected_root),
                    file: self
                        .expected_files
                        .and_then(|files| files.get(expected_file).cloned()),
                    source_parent: self.parent_identity(source),
                    target_parent: self.parent_identity(target),
                },
                source,
                target,
                renamed,
            ),
            None => rename_without_overwrite_with_identity(
                self.root,
                RenameIdentity::without_identity_checks(),
                source,
                target,
                renamed,
            ),
        }
    }

    fn parent_identity(&self, path: &Path) -> Option<DirectoryIdentity> {
        self.expected_directories.and_then(|directories| {
            path.parent()
                .and_then(|parent| directories.get(parent).cloned())
        })
    }
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
    let context = RenameContext::new(root, expected_root, expected_files, expected_directories);
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
            let mut rename_completed = false;
            let result = context.rename(&source, &target, &mut rename_completed);
            if rename_completed {
                renamed.push(RenameRecord {
                    source: source.clone(),
                    target: target.clone(),
                });
            }
            result?;
            interrupt.check()?;
        }
        Ok::<(), AppError>(())
    })();

    if let Err(error) = operation {
        return Err(AppError::rollback(error, rollback(&context, &renamed)));
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
    let context = RenameContext::new(root, expected_root, expected_files, expected_directories);
    let records = entries
        .iter()
        .map(|(source, target)| RenameRecord {
            source: source.clone(),
            target: target.clone(),
        })
        .collect::<Vec<_>>();
    rollback(&context, &records)
}

fn rollback(context: &RenameContext<'_>, entries: &[RenameRecord]) -> Vec<AppError> {
    let mut errors = Vec::new();
    // Reverse order restores chained renames without making an earlier source
    // name collide with a later rename that has not been undone yet.
    for entry in entries.iter().rev() {
        if entry.source == entry.target {
            continue;
        }
        let mut rename_completed = false;
        if let Err(error) = context.rename_with_expected_file(
            &entry.target,
            &entry.source,
            &entry.source,
            &mut rename_completed,
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

#[cfg(test)]
#[path = "../tests/unit/local.rs"]
mod tests;
