use std::process::Stdio;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant};

use super::{wait_for_child, wait_for_child_with_input};
use crate::{AppError, InterruptFlag};

#[test]
fn terminates_a_child_when_interrupted() {
    let interrupted = Arc::new(AtomicBool::new(false));
    let interrupt = InterruptFlag::from_atomic(Arc::clone(&interrupted));
    let child = std::process::Command::new("sleep")
        .arg("30")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    interrupted.store(true, Ordering::Relaxed);
    let started = Instant::now();

    let error = wait_for_child(
        child,
        Some(&interrupt),
        Duration::from_secs(5),
        "test command",
    )
    .unwrap_err();

    assert!(matches!(error, AppError::Interrupted));
    assert!(started.elapsed() < Duration::from_secs(2));
}

#[test]
fn terminates_a_child_when_the_command_times_out() {
    let child = std::process::Command::new("sleep")
        .arg("30")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();

    let error = wait_for_child(child, None, Duration::from_millis(50), "test command").unwrap_err();

    assert!(matches!(error, AppError::Message(message) if message.contains("timed out")));
}

#[test]
fn interrupts_a_child_while_stdin_is_blocked() {
    let interrupted = Arc::new(AtomicBool::new(true));
    let interrupt = InterruptFlag::from_atomic(interrupted);
    let child = std::process::Command::new("sleep")
        .arg("30")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let started = Instant::now();

    let error = wait_for_child_with_input(
        child,
        Some(&interrupt),
        Duration::from_secs(5),
        "test command",
        Some(vec![0; 8 * 1024 * 1024]),
    )
    .unwrap_err();

    assert!(matches!(error, AppError::Interrupted));
    assert!(started.elapsed() < Duration::from_secs(2));
}
