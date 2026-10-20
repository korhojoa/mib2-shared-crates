// SPDX-License-Identifier: MIT OR Apache-2.0
//! Check the actual command, file handling and sum-line interoperability.

use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT: AtomicU64 = AtomicU64::new(0);

struct Files(PathBuf);

impl Files {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "file-checksum-test-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }

    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_file-checksum"));
        command.current_dir(&self.0);
        command
    }
}

impl Drop for Files {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

#[test]
fn actual_command_reads_files_and_preserves_contents() {
    let files = Files::new();
    fs::write(files.0.join("a file"), b"abc").unwrap();
    fs::write(files.0.join("empty"), b"").unwrap();
    let output = files.command().args(["a file", "empty"]).output().unwrap();
    assert!(output.status.success());
    assert_eq!(output.stderr, b"");
    assert_eq!(
        output.stdout,
        concat!(
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad  a file\n",
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855  empty\n"
        )
        .as_bytes()
    );
    assert_eq!(fs::read(files.0.join("a file")).unwrap(), b"abc");
    assert_eq!(fs::read(files.0.join("empty")).unwrap(), b"");
}

#[test]
fn missing_file_fails_without_a_fabricated_digest_and_continues() {
    let files = Files::new();
    fs::write(files.0.join("good"), b"abc").unwrap();
    let output = files.command().args(["missing", "good"]).output().unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8(output.stderr)
        .unwrap()
        .contains("missing"));
    assert_eq!(
        output.stdout,
        b"ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad  good\n"
    );
}

#[test]
fn directory_read_fails_without_digest() {
    let files = Files::new();
    let output = files.command().arg(".").output().unwrap();
    assert!(!output.status.success());
    assert_eq!(output.stdout, b"");
    assert_ne!(output.stderr, b"");
}

#[test]
fn empty_arguments_fail_and_help_succeeds() {
    let files = Files::new();
    for args in [vec![], vec!["--"]] {
        let output = files.command().args(args).output().unwrap();
        assert!(!output.status.success());
        assert_eq!(output.stdout, b"");
    }
    assert!(files
        .command()
        .arg("--help")
        .output()
        .unwrap()
        .status
        .success());
}

#[test]
fn explicit_separator_allows_a_help_named_file() {
    let files = Files::new();
    fs::write(files.0.join("--help"), b"abc").unwrap();
    let output = files.command().args(["--", "--help"]).output().unwrap();
    assert!(output.status.success());
    assert_eq!(
        output.stdout,
        b"ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad  --help\n"
    );
}

#[cfg(unix)]
#[test]
fn unix_escaped_names_match_host_sha256sum() {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;

    let files = Files::new();
    let name = OsStr::from_bytes(b"name\\with\nline\rbreak");
    fs::write(files.0.join(name), b"abc").unwrap();
    let output = files.command().arg(name).output().unwrap();
    let reference = Command::new("sha256sum")
        .current_dir(&files.0)
        .arg(name)
        .output()
        .unwrap();
    assert!(reference.status.success());
    assert!(output.status.success());
    assert_eq!(output.stdout, reference.stdout);
}
