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

    fn move_object(&self, source: &ObjectPath, target: &ObjectPath) -> Result<String, AppError> {
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
fn records_a_remote_side_effect_before_handling_an_interrupt() {
    let interrupted = Arc::new(AtomicBool::new(false));
    let storage = FakeStorage {
        moves: std::cell::RefCell::new(Vec::new()),
        rollbacks: std::cell::RefCell::new(Vec::new()),
        fail_on_move: None,
        interrupt_after_move: Some(Arc::clone(&interrupted)),
    };
    let interrupt = InterruptFlag::from_atomic(interrupted);
    let source = ObjectPath::parse("gs://bucket/source.txt").unwrap();
    let target = ObjectPath::parse("gs://bucket/target.txt").unwrap();
    let temporary = ObjectPath::parse("gs://bucket/source.txt.task-googlecloud-token").unwrap();

    let result = process_moves(&storage, &interrupt, vec![(source, target, temporary)]);

    assert!(matches!(result, Err(AppError::Interrupted)));
    assert_eq!(storage.rollbacks.borrow().len(), 1);
}
