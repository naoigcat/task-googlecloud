use std::fs;
use std::sync::{Arc, atomic::AtomicBool};

use task_googlecloud::{Entry, InterruptFlag, apply_normalization, rollback_normalization};
use tempfile::tempdir;

fn no_interrupt() -> InterruptFlag {
    InterruptFlag::from_atomic(Arc::new(AtomicBool::new(false)))
}

#[test]
fn renames_without_changing_contents() {
    let directory = tempdir().unwrap();
    let source = directory.path().join("source.txt");
    let target = directory.path().join("target.txt");
    fs::write(&source, "content").unwrap();

    let entries = vec![Entry {
        source: source.to_str().unwrap().to_string(),
        target: target.to_str().unwrap().to_string(),
    }];
    let normalized = apply_normalization(&entries, &no_interrupt()).unwrap();

    assert_eq!(normalized.get(&source), Some(&target));
    assert_eq!(fs::read_to_string(target).unwrap(), "content");
    assert!(!source.exists());
}

#[test]
fn does_not_overwrite_a_target_that_appears_after_planning() {
    let directory = tempdir().unwrap();
    let source = directory.path().join("source.txt");
    let target = directory.path().join("target.txt");
    fs::write(&source, "original").unwrap();
    fs::write(&target, "competitor").unwrap();

    let entries = vec![Entry {
        source: source.to_str().unwrap().to_string(),
        target: target.to_str().unwrap().to_string(),
    }];
    let error = apply_normalization(&entries, &no_interrupt()).unwrap_err();

    assert!(
        error.to_string().contains("File exists") || error.to_string().contains("already exists")
    );
    assert_eq!(fs::read_to_string(source).unwrap(), "original");
    assert_eq!(fs::read_to_string(target).unwrap(), "competitor");
}

#[test]
fn rollback_does_not_overwrite_a_new_source() {
    let directory = tempdir().unwrap();
    let source = directory.path().join("source.txt");
    let target = directory.path().join("target.txt");
    fs::write(&source, "source").unwrap();
    fs::write(&target, "target").unwrap();

    let errors = rollback_normalization(&[(source.clone(), target.clone())]);

    assert_eq!(errors.len(), 1);
    assert_eq!(fs::read_to_string(source).unwrap(), "source");
    assert_eq!(fs::read_to_string(target).unwrap(), "target");
}

#[cfg(target_os = "macos")]
#[test]
fn rollback_ignores_nfc_and_nfd_aliases() {
    let directory = tempdir().unwrap();
    let source = directory.path().join("cafe\u{301}.txt");
    let target = directory.path().join("caf\u{e9}.txt");
    fs::write(&source, "content").unwrap();
    assert!(target.exists());
    let entries = vec![Entry {
        source: source.to_str().unwrap().to_string(),
        target: target.to_str().unwrap().to_string(),
    }];
    apply_normalization(&entries, &no_interrupt()).unwrap();
    assert_eq!(fs::read_to_string(&source).unwrap(), "content");
    assert_eq!(fs::read_to_string(&target).unwrap(), "content");

    let errors = rollback_normalization(&[(source.clone(), target.clone())]);

    assert!(errors.is_empty());
    assert_eq!(fs::read_to_string(&source).unwrap(), "content");
    assert_eq!(fs::read_to_string(&target).unwrap(), "content");
}

#[cfg(unix)]
#[test]
fn rollback_treats_dangling_symlinks_as_existing_entries() {
    let directory = tempdir().unwrap();
    let source = directory.path().join("source.txt");
    let target = directory.path().join("target.txt");
    fs::write(&source, "source").unwrap();
    std::os::unix::fs::symlink(directory.path().join("missing.txt"), &target).unwrap();

    let errors = rollback_normalization(&[(source, target)]);

    assert_eq!(errors.len(), 1);
    assert!(
        errors[0]
            .to_string()
            .contains("source and target both exist")
    );
}
