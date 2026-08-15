use std::cell::RefCell;
use std::sync::{Arc, atomic::AtomicBool};

use super::{RemoteChange, RemoteTransaction, rollback_changes};
use crate::InterruptFlag;
use crate::error::AppError;
use crate::object_path::ObjectPath;

fn named_change(source: &str, target: &str, generation: &str) -> RemoteChange {
    RemoteChange {
        source: ObjectPath::from_parts("bucket", source),
        target: ObjectPath::from_parts("bucket", target),
        generation: generation.to_string(),
    }
}

fn change(generation: &str) -> RemoteChange {
    named_change("source", "target", generation)
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

#[test]
fn rolls_back_finalized_changes_before_unfinalized_staged_changes() {
    let mut transaction = RemoteTransaction::new();
    transaction.stage(RemoteChange {
        source: ObjectPath::from_parts("bucket", "first-source"),
        target: ObjectPath::from_parts("bucket", "first-temporary"),
        generation: "first".to_string(),
    });
    transaction.stage(RemoteChange {
        source: ObjectPath::from_parts("bucket", "second-source"),
        target: ObjectPath::from_parts("bucket", "second-temporary"),
        generation: "second".to_string(),
    });
    transaction.stage(RemoteChange {
        source: ObjectPath::from_parts("bucket", "third-source"),
        target: ObjectPath::from_parts("bucket", "third-temporary"),
        generation: "third".to_string(),
    });

    transaction
        .finalize(&interrupt(), |index, change| {
            if index == 2 {
                return Err(AppError::Message("stop finalization".to_string()));
            }
            Ok(RemoteChange {
                source: change.source.clone(),
                target: ObjectPath::from_parts("bucket", &format!("final-{index}")),
                generation: format!("final-{index}"),
            })
        })
        .unwrap_err();

    let order = RefCell::new(Vec::new());
    let errors = rollback_changes(
        transaction.staged(),
        transaction.finalized(),
        |change| {
            order
                .borrow_mut()
                .push(format!("finalized:{}", change.source.object));
            Ok(())
        },
        |change| {
            order
                .borrow_mut()
                .push(format!("staged:{}", change.source.object));
            Ok(())
        },
    );

    assert!(errors.is_empty());
    assert_eq!(
        *order.borrow(),
        vec![
            "finalized:second-source",
            "finalized:first-source",
            "staged:third-source",
        ]
    );
}

#[test]
fn collects_rollback_errors_and_continues_in_reverse_order() {
    let staged = vec![
        named_change("staged-first", "staged-target-first", "staged-1"),
        named_change("staged-second", "staged-target-second", "staged-2"),
    ];
    let finalized = vec![
        named_change("finalized-first", "finalized-target-first", "finalized-1"),
        named_change("finalized-second", "finalized-target-second", "finalized-2"),
    ];
    let calls = RefCell::new(Vec::new());

    let errors = rollback_changes(
        &staged,
        &finalized,
        |change| {
            calls
                .borrow_mut()
                .push(format!("finalized:{}", change.source.object));
            if change.source.object == "finalized-second" {
                return Err(AppError::Message("finalized rollback failed".to_string()));
            }
            Ok(())
        },
        |change| {
            calls
                .borrow_mut()
                .push(format!("staged:{}", change.source.object));
            if change.source.object == "staged-second" {
                return Err(AppError::Message("staged rollback failed".to_string()));
            }
            Ok(())
        },
    );

    assert_eq!(
        *calls.borrow(),
        vec![
            "finalized:finalized-second",
            "finalized:finalized-first",
            "staged:staged-second",
            "staged:staged-first",
        ]
    );
    assert_eq!(
        errors.iter().map(ToString::to_string).collect::<Vec<_>>(),
        vec!["finalized rollback failed", "staged rollback failed"]
    );
}

#[test]
fn executes_staging_and_rolls_back_when_staging_fails() {
    let calls = RefCell::new(Vec::new());
    let error = RemoteTransaction::execute(
        &interrupt(),
        |transaction| {
            transaction.stage(change("first"));
            transaction.stage(change("second"));
            Err(AppError::Message("staging failed".to_string()))
        },
        |_index, _change| panic!("finalization must not run after staging fails"),
        |staged, finalized| {
            calls.borrow_mut().push((staged.len(), finalized.len()));
            Vec::new()
        },
    )
    .unwrap_err();

    assert_eq!(error.to_string(), "staging failed");
    assert_eq!(*calls.borrow(), vec![(2, 0)]);
}

#[test]
fn executes_rollback_after_a_partial_finalization() {
    let calls = RefCell::new(Vec::new());
    let error = RemoteTransaction::execute(
        &interrupt(),
        |transaction| {
            transaction.stage(change("first"));
            transaction.stage(change("second"));
            Ok(())
        },
        |index, change| {
            if index == 1 {
                return Err(AppError::Message("finalization failed".to_string()));
            }
            Ok(named_change(
                &change.source.object,
                "finalized-target",
                "finalized",
            ))
        },
        |staged, finalized| {
            calls.borrow_mut().push((staged.len(), finalized.len()));
            Vec::new()
        },
    )
    .unwrap_err();

    assert_eq!(error.to_string(), "finalization failed");
    assert_eq!(*calls.borrow(), vec![(2, 1)]);
}
