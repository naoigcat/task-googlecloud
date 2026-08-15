use std::sync::{Arc, atomic::AtomicBool};

use super::{RemoteChange, RemoteTransaction};
use crate::InterruptFlag;
use crate::error::AppError;
use crate::object_path::ObjectPath;

fn change(generation: &str) -> RemoteChange {
    RemoteChange {
        source: ObjectPath::from_parts("bucket", "source"),
        target: ObjectPath::from_parts("bucket", "target"),
        generation: generation.to_string(),
    }
}

fn interrupt() -> InterruptFlag {
    InterruptFlag::from_atomic(Arc::new(AtomicBool::new(false)))
}

#[test]
fn records_staged_and_finalized_changes_separately() {
    let mut transaction = RemoteTransaction::new();
    transaction.stage(change("staged"));

    transaction
        .finalize(&interrupt(), |_, staged| {
            assert_eq!(staged.generation, "staged");
            Ok(change("finalized"))
        })
        .unwrap();

    assert_eq!(transaction.staged()[0].generation, "staged");
    assert_eq!(transaction.finalized()[0].generation, "finalized");
}

#[test]
fn preserves_a_restored_generation_for_rollback() {
    let mut transaction = RemoteTransaction::new();
    transaction.stage(change("copied"));

    let error = transaction
        .finalize(&interrupt(), |_, _| {
            Err(AppError::interrupted_after_move_rollback(
                "restored".to_string(),
            ))
        })
        .unwrap_err();

    assert!(error.is_interrupted());
    assert_eq!(transaction.staged()[0].generation, "restored");
    assert!(transaction.finalized().is_empty());
}
