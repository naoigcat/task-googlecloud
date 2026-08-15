use std::collections::HashSet;

use crate::InterruptFlag;
use crate::error::AppError;
use crate::storage::{ObjectPath, StorageClient};

#[derive(Clone, Debug)]
/// One remote change and the generation that this run owns at its target.
pub struct RemoteChange {
    pub source: ObjectPath,
    pub target: ObjectPath,
    pub generation: String,
}

#[derive(Default)]
pub(crate) struct RemoteTransaction {
    staged: Vec<RemoteChange>,
    finalized: Vec<RemoteChange>,
}

impl RemoteTransaction {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn stage(&mut self, change: RemoteChange) {
        self.staged.push(change);
    }

    pub(crate) fn staged(&self) -> &[RemoteChange] {
        &self.staged
    }

    pub(crate) fn finalized(&self) -> &[RemoteChange] {
        &self.finalized
    }

    /// Finalizes each staged change while retaining enough state to roll it
    /// back if the finalization or its interrupt boundary fails.
    pub(crate) fn finalize<F>(
        &mut self,
        interrupt: &InterruptFlag,
        mut operation: F,
    ) -> Result<(), AppError>
    where
        F: FnMut(usize, &RemoteChange) -> Result<RemoteChange, AppError>,
    {
        for index in 0..self.staged.len() {
            let staged = self.staged[index].clone();
            let finalized = match operation(index, &staged) {
                Ok(finalized) => finalized,
                Err(error) => {
                    if let Some(restored_generation) = error.restored_move_generation() {
                        self.staged[index].generation = restored_generation.to_string();
                    }
                    return Err(error);
                }
            };
            self.finalized.push(finalized);
            interrupt.check()?;
        }
        Ok(())
    }
}

pub(crate) fn rollback_moves<S: StorageClient>(
    storage: &S,
    staged: &[RemoteChange],
    finalized: &[RemoteChange],
) -> Vec<AppError> {
    rollback_changes(
        staged,
        finalized,
        |change| {
            storage
                .rollback_object(&change.source, &change.target, &change.generation)
                .map(|_| ())
        },
        |change| {
            storage
                .rollback_object(&change.source, &change.target, &change.generation)
                .map(|_| ())
        },
    )
}

/// Rolls back finalized changes before still-staged changes while avoiding a
/// second rollback for a staged source already consumed by finalization.
pub(crate) fn rollback_changes<Finalized, Staged>(
    staged: &[RemoteChange],
    finalized: &[RemoteChange],
    mut rollback_finalized: Finalized,
    mut rollback_staged: Staged,
) -> Vec<AppError>
where
    Finalized: FnMut(&RemoteChange) -> Result<(), AppError>,
    Staged: FnMut(&RemoteChange) -> Result<(), AppError>,
{
    let mut errors = Vec::new();
    let finalized_sources = finalized
        .iter()
        .map(|change| &change.source)
        .collect::<HashSet<_>>();

    // Undo finalized moves first, then the still-staged moves. Reverse order
    // preserves the same dependency ordering used while applying the plan.
    for change in finalized.iter().rev() {
        if let Err(error) = rollback_finalized(change) {
            errors.push(error);
        }
    }

    for change in staged.iter().rev() {
        if finalized_sources.contains(&change.source) {
            continue;
        }
        if let Err(error) = rollback_staged(change) {
            errors.push(error);
        }
    }
    errors
}

#[cfg(test)]
#[path = "../tests/unit/transaction.rs"]
mod tests;
