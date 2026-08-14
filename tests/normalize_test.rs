use std::path::Path;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use task_googlecloud::{AppError, InterruptFlag, ObjectPath, StorageClient, process_moves};

struct FakeStorage {
    moves: std::cell::RefCell<Vec<(String, String)>>,
    rollbacks: std::cell::RefCell<Vec<(String, String)>>,
    fail_on_move: Option<usize>,
    interrupt_after_move: Option<Arc<AtomicBool>>,
}

impl StorageClient for FakeStorage {
    fn list_objects(&self, _bucket: &str) -> Result<Vec<String>, AppError> {
        Ok(Vec::new())
    }

    fn upload_file(&self, _source: &Path, _target: &ObjectPath) -> Result<String, AppError> {
        Ok("1".to_string())
    }

    fn move_object(
        &self,
        source: &ObjectPath,
        target: &ObjectPath,
        _expected_source_generation: Option<&str>,
    ) -> Result<String, AppError> {
        let mut moves = self.moves.borrow_mut();
        moves.push((source.uri(), target.uri()));
        if self.fail_on_move == Some(moves.len()) {
            return Err(AppError::Message("move failed".to_string()));
        }
        if let Some(interrupted) = &self.interrupt_after_move {
            interrupted.store(true, Ordering::Relaxed);
        }
        Ok(moves.len().to_string())
    }

    fn rollback_object(
        &self,
        source: &ObjectPath,
        target: &ObjectPath,
        _generation: &str,
    ) -> Result<String, AppError> {
        self.rollbacks
            .borrow_mut()
            .push((source.uri(), target.uri()));
        Ok("rollback".to_string())
    }

    fn cleanup_object(
        &self,
        _target: &ObjectPath,
        _target_generation: &str,
    ) -> Result<(), AppError> {
        Ok(())
    }

    fn confirm_move_after_failure(
        &self,
        _source: &ObjectPath,
        _target: &ObjectPath,
        _operation: &AppError,
    ) -> Result<(), AppError> {
        Ok(())
    }

    fn confirm_write_after_failure(
        &self,
        _target: &ObjectPath,
        _operation: &AppError,
    ) -> Result<(), AppError> {
        Ok(())
    }
}

struct ReplacingStorage {
    move_generations: std::cell::RefCell<Vec<Option<String>>>,
    rollbacks: std::cell::RefCell<Vec<String>>,
}

impl StorageClient for ReplacingStorage {
    fn list_objects(&self, _bucket: &str) -> Result<Vec<String>, AppError> {
        Ok(Vec::new())
    }

    fn upload_file(&self, _source: &Path, _target: &ObjectPath) -> Result<String, AppError> {
        Ok("upload".to_string())
    }

    fn move_object(
        &self,
        _source: &ObjectPath,
        _target: &ObjectPath,
        expected_source_generation: Option<&str>,
    ) -> Result<String, AppError> {
        let call = self.move_generations.borrow().len();
        self.move_generations
            .borrow_mut()
            .push(expected_source_generation.map(str::to_string));

        match call {
            0 => {
                // A different writer replaces the temporary object before finalization.
                Ok("staged".to_string())
            }
            1 if expected_source_generation.is_none() => Ok("finalized".to_string()),
            1 => Err(AppError::Storage {
                status: 412,
                message: "staged object generation changed".to_string(),
            }),
            _ => unreachable!("the test should only perform staging and finalization"),
        }
    }

    fn rollback_object(
        &self,
        _source: &ObjectPath,
        _target: &ObjectPath,
        _target_generation: &str,
    ) -> Result<String, AppError> {
        self.rollbacks.borrow_mut().push("staged move".to_string());
        Ok("rollback".to_string())
    }

    fn cleanup_object(
        &self,
        _target: &ObjectPath,
        _target_generation: &str,
    ) -> Result<(), AppError> {
        Ok(())
    }

    fn confirm_move_after_failure(
        &self,
        _source: &ObjectPath,
        _target: &ObjectPath,
        _operation: &AppError,
    ) -> Result<(), AppError> {
        Ok(())
    }

    fn confirm_write_after_failure(
        &self,
        _target: &ObjectPath,
        _operation: &AppError,
    ) -> Result<(), AppError> {
        Ok(())
    }
}

#[test]
fn rolls_back_staged_moves_when_finalization_fails() {
    let storage = FakeStorage {
        moves: std::cell::RefCell::new(Vec::new()),
        rollbacks: std::cell::RefCell::new(Vec::new()),
        fail_on_move: Some(2),
        interrupt_after_move: None,
    };
    let interrupt = InterruptFlag::from_atomic(Arc::new(AtomicBool::new(false)));
    let source = ObjectPath::parse("gs://bucket/e\u{301}.txt").unwrap();
    let target = ObjectPath::parse("gs://bucket/é.txt").unwrap();
    let temporary = ObjectPath::parse("gs://bucket/e\u{301}.txt.task-googlecloud-token").unwrap();

    let result = process_moves(&storage, &interrupt, vec![(source, target, temporary)]);

    assert!(result.is_err());
    assert_eq!(storage.rollbacks.borrow().len(), 1);
    assert_eq!(storage.rollbacks.borrow()[0].0, "gs://bucket/e\u{301}.txt");
}

#[test]
fn rejects_a_staged_object_replaced_before_finalization() {
    let storage = ReplacingStorage {
        move_generations: std::cell::RefCell::new(Vec::new()),
        rollbacks: std::cell::RefCell::new(Vec::new()),
    };
    let interrupt = InterruptFlag::from_atomic(Arc::new(AtomicBool::new(false)));
    let source = ObjectPath::parse("gs://bucket/source").unwrap();
    let target = ObjectPath::parse("gs://bucket/target").unwrap();
    let temporary = ObjectPath::parse("gs://bucket/source.task-googlecloud-token").unwrap();

    let result = process_moves(&storage, &interrupt, vec![(source, target, temporary)]);

    assert!(matches!(result, Err(AppError::Storage { status: 412, .. })));
    assert_eq!(
        *storage.move_generations.borrow(),
        vec![None, Some("staged".to_string())]
    );
    assert_eq!(storage.rollbacks.borrow().len(), 1);
}

#[test]
fn rolls_back_finalized_moves_before_unfinalized_staged_moves() {
    let storage = FakeStorage {
        moves: std::cell::RefCell::new(Vec::new()),
        rollbacks: std::cell::RefCell::new(Vec::new()),
        fail_on_move: Some(6),
        interrupt_after_move: None,
    };
    let interrupt = InterruptFlag::from_atomic(Arc::new(AtomicBool::new(false)));
    let first_source = ObjectPath::parse("gs://bucket/first-source").unwrap();
    let first_target = ObjectPath::parse("gs://bucket/first-target").unwrap();
    let first_temporary = ObjectPath::parse("gs://bucket/first-temporary").unwrap();
    let second_source = ObjectPath::parse("gs://bucket/second-source").unwrap();
    let second_target = ObjectPath::parse("gs://bucket/second-target").unwrap();
    let second_temporary = ObjectPath::parse("gs://bucket/second-temporary").unwrap();
    let third_source = ObjectPath::parse("gs://bucket/third-source").unwrap();
    let third_target = ObjectPath::parse("gs://bucket/third-target").unwrap();
    let third_temporary = ObjectPath::parse("gs://bucket/third-temporary").unwrap();

    let result = process_moves(
        &storage,
        &interrupt,
        vec![
            (first_source, first_target, first_temporary),
            (second_source, second_target, second_temporary),
            (third_source, third_target, third_temporary),
        ],
    );

    assert!(result.is_err());
    assert_eq!(
        *storage.rollbacks.borrow(),
        vec![
            (
                "gs://bucket/second-source".to_string(),
                "gs://bucket/second-target".to_string(),
            ),
            (
                "gs://bucket/first-source".to_string(),
                "gs://bucket/first-target".to_string(),
            ),
            (
                "gs://bucket/third-source".to_string(),
                "gs://bucket/third-temporary".to_string(),
            ),
        ]
    );
}

#[test]
fn rolls_back_staged_moves_in_reverse_order_when_staging_fails() {
    let storage = FakeStorage {
        moves: std::cell::RefCell::new(Vec::new()),
        rollbacks: std::cell::RefCell::new(Vec::new()),
        fail_on_move: Some(3),
        interrupt_after_move: None,
    };
    let interrupt = InterruptFlag::from_atomic(Arc::new(AtomicBool::new(false)));
    let first_source = ObjectPath::parse("gs://bucket/first-source").unwrap();
    let first_target = ObjectPath::parse("gs://bucket/first-target").unwrap();
    let first_temporary = ObjectPath::parse("gs://bucket/first-temporary").unwrap();
    let second_source = ObjectPath::parse("gs://bucket/second-source").unwrap();
    let second_target = ObjectPath::parse("gs://bucket/second-target").unwrap();
    let second_temporary = ObjectPath::parse("gs://bucket/second-temporary").unwrap();
    let third_source = ObjectPath::parse("gs://bucket/third-source").unwrap();
    let third_target = ObjectPath::parse("gs://bucket/third-target").unwrap();
    let third_temporary = ObjectPath::parse("gs://bucket/third-temporary").unwrap();

    let result = process_moves(
        &storage,
        &interrupt,
        vec![
            (first_source, first_target, first_temporary),
            (second_source, second_target, second_temporary),
            (third_source, third_target, third_temporary),
        ],
    );

    assert!(result.is_err());
    assert_eq!(
        *storage.rollbacks.borrow(),
        vec![
            (
                "gs://bucket/second-source".to_string(),
                "gs://bucket/second-temporary".to_string(),
            ),
            (
                "gs://bucket/first-source".to_string(),
                "gs://bucket/first-temporary".to_string(),
            ),
        ]
    );
}

#[test]
fn records_a_remote_side_effect_before_handling_an_interrupt() {
    let interrupted = Arc::new(AtomicBool::new(false));
    let storage = FakeStorage {
        moves: std::cell::RefCell::new(Vec::new()),
        rollbacks: std::cell::RefCell::new(Vec::new()),
        fail_on_move: None,
        interrupt_after_move: Some(Arc::clone(&interrupted)),
    };
    let interrupt = InterruptFlag::from_atomic(Arc::clone(&interrupted));
    let source = ObjectPath::parse("gs://bucket/source.txt").unwrap();
    let target = ObjectPath::parse("gs://bucket/target.txt").unwrap();
    let temporary = ObjectPath::parse("gs://bucket/source.txt.task-googlecloud-token").unwrap();

    let result = process_moves(&storage, &interrupt, vec![(source, target, temporary)]);

    assert!(matches!(result, Err(AppError::Interrupted)));
    assert_eq!(storage.rollbacks.borrow().len(), 1);
    assert!(!interrupted.load(Ordering::Relaxed));
}
