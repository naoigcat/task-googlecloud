use std::collections::HashMap;
#[cfg(any(target_os = "linux", target_os = "macos"))]
use std::ffi::CString;
use std::fs;
#[cfg(any(target_os = "linux", target_os = "macos"))]
use std::os::unix::ffi::OsStrExt;
#[cfg(any(target_os = "linux", target_os = "macos"))]
use std::os::unix::fs::OpenOptionsExt;
#[cfg(any(target_os = "linux", target_os = "macos"))]
use std::sync::mpsc;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
#[cfg(any(target_os = "linux", target_os = "macos"))]
use std::thread;
#[cfg(any(target_os = "linux", target_os = "macos"))]
use std::time::Duration;

use tempfile::tempdir;

use super::*;
use crate::atomic_rename::{
    directory_identity_from_path, file_identity_from_path, identity_descriptor_is_unlinked,
};

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn create_fifo(path: &std::path::Path) {
    let path = CString::new(path.as_os_str().as_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);
}

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
fn refuses_to_block_on_a_fifo_replacing_a_source_file() {
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
    create_fifo(&source);
    let entries = vec![Entry {
        source: source.to_str().unwrap().to_string(),
        target: target.to_str().unwrap().to_string(),
    }];
    let files = HashMap::from([(source.clone(), expected_file)]);
    let directories = HashMap::from([(bucket.clone(), expected_directory)]);
    let interrupted = Arc::new(AtomicBool::new(false));
    let interrupt = InterruptFlag::from_atomic(Arc::clone(&interrupted));
    let (started_sender, started_receiver) = mpsc::channel();
    let (result_sender, result_receiver) = mpsc::channel();
    let root_for_thread = root.clone();
    let entries_for_thread = entries.clone();
    let files_for_thread = files.clone();
    let directories_for_thread = directories.clone();
    let interrupt_for_thread = interrupt.clone();
    let worker = thread::spawn(move || {
        started_sender.send(()).unwrap();
        let result = apply_normalization_with_path_identities(
            &root_for_thread,
            &entries_for_thread,
            Some(expected_root),
            Some(&files_for_thread),
            Some(&directories_for_thread),
            &interrupt_for_thread,
        );
        result_sender.send(result).unwrap();
    });
    started_receiver
        .recv_timeout(Duration::from_secs(1))
        .unwrap();
    interrupted.store(true, Ordering::Relaxed);

    let first_result = result_receiver.recv_timeout(Duration::from_millis(250));
    let blocked = first_result.is_err();
    let writer = if blocked {
        Some(
            fs::OpenOptions::new()
                .write(true)
                .custom_flags(libc::O_NONBLOCK)
                .open(&source)
                .unwrap(),
        )
    } else {
        None
    };
    let result = if blocked {
        result_receiver
            .recv_timeout(Duration::from_secs(1))
            .unwrap()
    } else {
        first_result.unwrap()
    };
    drop(writer);
    worker.join().unwrap();

    assert!(!blocked, "source replacement blocked identity verification");
    assert!(interrupted.load(Ordering::Relaxed));
    assert!(result.is_err());
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
