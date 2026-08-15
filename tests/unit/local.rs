use std::collections::HashMap;
use std::fs;
use std::sync::{Arc, atomic::AtomicBool};

use tempfile::tempdir;

use super::*;
use crate::atomic_rename::{
    directory_identity_from_path, file_identity_from_path, identity_descriptor_is_unlinked,
};

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn refuses_to_rename_through_a_replaced_normal_root() {
    let parent = tempdir().unwrap();
    let root = parent.path().join("uploads");
    fs::create_dir(&root).unwrap();
    let expected_root = directory_identity_from_path(&root).unwrap();

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

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn refuses_to_rename_a_replaced_source_file() {
    let parent = tempdir().unwrap();
    let root = parent.path().join("uploads");
    let bucket = root.join("bucket");
    let source = bucket.join("source.txt");
    let target = bucket.join("target.txt");
    fs::create_dir_all(&bucket).unwrap();
    fs::write(&source, "original").unwrap();
    let expected_root = directory_identity_from_path(&root).unwrap();
    let expected_directory = directory_identity_from_path(&bucket).unwrap();
    let expected_file = file_identity_from_path(&source).unwrap();
    fs::remove_file(&source).unwrap();
    let entries = vec![Entry {
        source: source.to_str().unwrap().to_string(),
        target: target.to_str().unwrap().to_string(),
    }];
    let files = HashMap::from([(source.clone(), expected_file)]);
    let directories = HashMap::from([(bucket.clone(), expected_directory)]);
    let interrupt = InterruptFlag::from_atomic(Arc::new(AtomicBool::new(false)));
    assert!(identity_descriptor_is_unlinked(files.get(&source).unwrap()).unwrap());
    fs::write(&source, "replacement").unwrap();

    let error = apply_normalization_with_path_identities(
        &root,
        &entries,
        Some(expected_root),
        Some(&files),
        Some(&directories),
        &interrupt,
    )
    .unwrap_err();

    assert!(error.to_string().contains("Input file was replaced"));
    assert_eq!(fs::read_to_string(&source).unwrap(), "replacement");
    assert!(!target.exists());
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn renames_a_file_with_captured_identities() {
    let parent = tempdir().unwrap();
    let root = parent.path().join("uploads");
    let bucket = root.join("bucket");
    let source = bucket.join("source.txt");
    let target = bucket.join("target.txt");
    fs::create_dir_all(&bucket).unwrap();
    fs::write(&source, "original").unwrap();
    let expected_root = directory_identity_from_path(&root).unwrap();
    let expected_directory = directory_identity_from_path(&bucket).unwrap();
    let expected_file = file_identity_from_path(&source).unwrap();
    let entries = vec![Entry {
        source: source.to_str().unwrap().to_string(),
        target: target.to_str().unwrap().to_string(),
    }];
    let files = HashMap::from([(source.clone(), expected_file)]);
    let directories = HashMap::from([(bucket.clone(), expected_directory)]);
    let interrupt = InterruptFlag::from_atomic(Arc::new(AtomicBool::new(false)));

    apply_normalization_with_path_identities(
        &root,
        &entries,
        Some(expected_root),
        Some(&files),
        Some(&directories),
        &interrupt,
    )
    .unwrap();

    assert_eq!(fs::read_to_string(&target).unwrap(), "original");
    assert!(!source.exists());
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn refuses_to_rename_through_a_replaced_bucket() {
    let parent = tempdir().unwrap();
    let root = parent.path().join("uploads");
    let bucket = root.join("bucket");
    let source = bucket.join("source.txt");
    let target = bucket.join("target.txt");
    fs::create_dir_all(&bucket).unwrap();
    fs::write(&source, "original").unwrap();
    let expected_root = directory_identity_from_path(&root).unwrap();
    let expected_directory = directory_identity_from_path(&bucket).unwrap();
    let expected_file = file_identity_from_path(&source).unwrap();
    fs::remove_file(&source).unwrap();
    fs::remove_dir(&bucket).unwrap();
    fs::create_dir(&bucket).unwrap();
    fs::write(&source, "replacement").unwrap();
    let entries = vec![Entry {
        source: source.to_str().unwrap().to_string(),
        target: target.to_str().unwrap().to_string(),
    }];
    let files = HashMap::from([(source.clone(), expected_file)]);
    let directories = HashMap::from([(bucket.clone(), expected_directory)]);
    let interrupt = InterruptFlag::from_atomic(Arc::new(AtomicBool::new(false)));

    let error = apply_normalization_with_path_identities(
        &root,
        &entries,
        Some(expected_root),
        Some(&files),
        Some(&directories),
        &interrupt,
    )
    .unwrap_err();

    assert!(
        error
            .to_string()
            .contains("Input source directory was replaced")
    );
    assert_eq!(fs::read_to_string(&source).unwrap(), "replacement");
    assert!(!target.exists());
}
