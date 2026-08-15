use std::fs;
use std::sync::{Arc, atomic::AtomicBool};

use tempfile::tempdir;

use super::*;
use crate::atomic_rename::directory_identity_from_metadata;

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn refuses_to_rename_through_a_replaced_normal_root() {
    let parent = tempdir().unwrap();
    let root = parent.path().join("uploads");
    fs::create_dir(&root).unwrap();
    let expected_root = directory_identity_from_metadata(&fs::symlink_metadata(&root).unwrap());

    let replacement = parent.path().join("replacement");
    fs::create_dir(&replacement).unwrap();
    fs::remove_dir(&root).unwrap();
    fs::rename(&replacement, &root).unwrap();

    let source = root.join("source.txt");
    let target = root.join("target.txt");
    fs::write(&source, "replacement").unwrap();
    let entries = vec![Entry {
        source: source.to_str().unwrap().to_string(),
        target: target.to_str().unwrap().to_string(),
    }];
    let interrupt = InterruptFlag::from_atomic(Arc::new(AtomicBool::new(false)));

    let error = apply_normalization_with_identity(&root, &entries, Some(expected_root), &interrupt)
        .unwrap_err();

    assert!(error.to_string().contains("Input root was replaced"));
    assert_eq!(fs::read_to_string(&source).unwrap(), "replacement");
    assert!(!target.exists());
}
