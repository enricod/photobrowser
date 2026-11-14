//! Runs the real `photobrowser` binary in LibRaw helper mode.

use std::path::Path;
use std::process::Command;
use std::time::Duration;

use photobrowser::decode::DecodeError;
use photobrowser::decode::helper::{HELPER_FLAG, full_args, preview_args, run_helper};

const BIN: &str = env!("CARGO_BIN_EXE_photobrowser");

fn not_a_raw() -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("fake.CR3");
    std::fs::write(&path, b"definitely not a raw file").unwrap();
    (dir, path)
}

#[test]
fn helper_reports_a_bad_file_as_an_error() {
    let (_dir, path) = not_a_raw();
    for args in [preview_args(&path, 256), full_args(&path)] {
        match run_helper(Path::new(BIN), &args, Duration::from_secs(30)) {
            Err(DecodeError::Raw(message)) => {
                assert!(!message.contains("crashed"), "{message}");
                assert!(!message.is_empty());
            }
            other => panic!(
                "expected a RAW error, got {:?}",
                other.map(|(i, _)| i.width)
            ),
        }
    }
}

#[test]
fn helper_exits_cleanly_without_gtk() {
    let (_dir, path) = not_a_raw();
    let out = Command::new(BIN)
        .args([HELPER_FLAG, "full"])
        .arg(&path)
        .env_remove("DISPLAY")
        .env_remove("WAYLAND_DISPLAY")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1), "{out:?}");
    assert!(out.stdout.is_empty());
}

#[test]
fn helper_rejects_bad_arguments() {
    let out = Command::new(BIN)
        .args([HELPER_FLAG, "bogus"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
}
