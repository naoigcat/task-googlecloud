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
    let normalized = apply_normalization(directory.path(), &entries, &no_interrupt()).unwrap();

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
    let error = apply_normalization(directory.path(), &entries, &no_interrupt()).unwrap_err();

    assert!(
        error.to_string().contains("File exists") || error.to_string().contains("already exists")
    );
    assert_eq!(fs::read_to_string(source).unwrap(), "original");
    assert_eq!(fs::read_to_string(target).unwrap(), "competitor");
}

#[cfg(target_os = "linux")]
#[test]
fn does_not_treat_a_hard_link_as_a_unicode_alias() {
    let directory = tempdir().unwrap();
    let source = directory.path().join("cafe\u{301}.txt");
    let target = directory.path().join("caf\u{e9}.txt");
    fs::write(&source, "original").unwrap();
    fs::hard_link(&source, &target).unwrap();

    let entries = vec![Entry {
        source: source.to_str().unwrap().to_string(),
        target: target.to_str().unwrap().to_string(),
    }];
    let error = apply_normalization(directory.path(), &entries, &no_interrupt()).unwrap_err();

    assert!(
        error.to_string().contains("File exists") || error.to_string().contains("already exists")
    );
    assert_eq!(fs::read_to_string(source).unwrap(), "original");
    assert_eq!(fs::read_to_string(target).unwrap(), "original");
}

#[cfg(unix)]
#[test]
fn refuses_to_rename_through_a_replaced_root() {
    use std::os::unix::fs::symlink;

    let parent = tempdir().unwrap();
    let root = parent.path().join("uploads");
    let outside = tempdir().unwrap();
    let outside_bucket = outside.path().join("bucket");
    fs::create_dir(&root).unwrap();
    fs::create_dir(&outside_bucket).unwrap();
    let outside_source = outside_bucket.join("source.txt");
    let outside_target = outside_bucket.join("target.txt");
    fs::write(&outside_source, "outside").unwrap();

    fs::remove_dir(&root).unwrap();
    symlink(outside.path(), &root).unwrap();
    let source = root.join("bucket/source.txt");
    let target = root.join("bucket/target.txt");
    let entries = vec![Entry {
        source: source.to_str().unwrap().to_string(),
        target: target.to_str().unwrap().to_string(),
    }];

    assert!(apply_normalization(&root, &entries, &no_interrupt()).is_err());
    assert_eq!(fs::read_to_string(outside_source).unwrap(), "outside");
    assert!(!outside_target.exists());
}

#[test]
fn rollback_does_not_overwrite_a_new_source() {
    let directory = tempdir().unwrap();
    let source = directory.path().join("source.txt");
    let target = directory.path().join("target.txt");
    fs::write(&source, "source").unwrap();
    fs::write(&target, "target").unwrap();

    let errors = rollback_normalization(directory.path(), &[(source.clone(), target.clone())]);

    assert_eq!(errors.len(), 1);
    assert_eq!(fs::read_to_string(source).unwrap(), "source");
    assert_eq!(fs::read_to_string(target).unwrap(), "target");
}

#[cfg(unix)]
#[test]
fn refuses_to_rollback_through_a_replaced_bucket() {
    use std::os::unix::fs::symlink;

    let parent = tempdir().unwrap();
    let root = parent.path().join("uploads");
    let bucket = root.join("bucket");
    let outside = tempdir().unwrap();
    fs::create_dir(&root).unwrap();
    fs::write(outside.path().join("target.txt"), "outside").unwrap();

    fs::create_dir(&bucket).unwrap();
    fs::remove_dir(&bucket).unwrap();
    symlink(outside.path(), &bucket).unwrap();
    let source = bucket.join("source.txt");
    let target = bucket.join("target.txt");

    let errors = rollback_normalization(&root, &[(source.clone(), target.clone())]);

    assert_eq!(errors.len(), 1);
    assert_eq!(
        fs::read_to_string(outside.path().join("target.txt")).unwrap(),
        "outside"
    );
    assert!(!outside.path().join("source.txt").exists());
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn rollback_ignores_nfc_and_nfd_aliases() {
    // Use the project mount so Docker Desktop exposes the host filesystem's
    // normalization behavior; a native Linux filesystem may keep both names.
    let directory = tempfile::tempdir_in(".").unwrap();
    let source = directory.path().join("cafe\u{301}.txt");
    let target = directory.path().join("caf\u{e9}.txt");
    fs::write(&source, "content").unwrap();
    if !target.exists() {
        return;
    }
    let entries = vec![Entry {
        source: source.to_str().unwrap().to_string(),
        target: target.to_str().unwrap().to_string(),
    }];
    apply_normalization(directory.path(), &entries, &no_interrupt()).unwrap();
    assert_eq!(fs::read_to_string(&source).unwrap(), "content");
    assert_eq!(fs::read_to_string(&target).unwrap(), "content");

    let errors = rollback_normalization(directory.path(), &[(source.clone(), target.clone())]);

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

    let errors = rollback_normalization(directory.path(), &[(source, target)]);

    assert_eq!(errors.len(), 1);
    assert!(
        errors[0]
            .to_string()
            .contains("source and target both exist")
    );
}
