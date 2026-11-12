//! Writing executable test fixtures without leaving a write descriptor behind.
//!
//! A test process that writes an executable and then starts it can be refused
//! with `ETXTBSY`: another test thread may fork while the write descriptor is
//! still open, and its child keeps the inherited descriptor until its own exec.
//! Opening the fixture through a short-lived child keeps the descriptor out of
//! the test process, so a concurrent fork cannot carry it into another program
//! start. The child is waited for before the path is handed back; once it exits,
//! the kernel no longer sees a writer for the file.

// Each test crate uses only the writers it needs, and warnings are denied over
// every target.
#![allow(dead_code)]

use std::io::{self, Write};
use std::path::Path;
use std::process::{Command, Stdio};

const WRITER: &str = "set -eu\ncat > \"$1\"\n";
const WRITER_WITH_MODE: &str = "set -eu\ncat > \"$1\"\nchmod \"$2\" \"$1\"\n";
const COPY: &str = "set -eu\ncp \"$1\" \"$2\"\nchmod \"$3\" \"$2\"\n";

/// Writes `bytes` to `path` without changing the file mode.
///
/// A fixture that is executable only after a later explicit mode change still
/// has to be written here: the write descriptor, not the mode, is what a
/// concurrent fork can leave behind.
pub fn write_file(path: &Path, bytes: &[u8]) -> io::Result<()> {
    write_via(path, bytes, None)
}

/// Writes `bytes` to `path` and makes it executable.
pub fn write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    write_mode(path, bytes, 0o755)
}

/// Writes `bytes` to `path` with `mode` (including the executable bits).
pub fn write_mode(path: &Path, bytes: &[u8], mode: u32) -> io::Result<()> {
    write_via(path, bytes, Some(mode))
}

fn write_via(path: &Path, bytes: &[u8], mode: Option<u32>) -> io::Result<()> {
    let script = if mode.is_some() {
        WRITER_WITH_MODE
    } else {
        WRITER
    };
    let mut command = Command::new("/bin/sh");
    command
        .args(["-c", script, "among-test-executable"])
        .arg(path);
    if let Some(mode) = mode {
        command.arg(format!("{mode:o}"));
    }
    let mut child = command.stdin(Stdio::piped()).spawn()?;
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| io::Error::other("executable fixture writer has no stdin"))?;
    let written = stdin.write_all(bytes);
    drop(stdin);
    let status = child.wait()?;
    written?;
    if !status.success() {
        return Err(io::Error::other(format!(
            "writing executable fixture {} failed with {status}",
            path.display()
        )));
    }
    Ok(())
}

/// Copies `source` to `destination` and makes the copy executable.
pub fn copy(source: &Path, destination: &Path) -> io::Result<()> {
    copy_mode(source, destination, 0o755)
}

/// Copies `source` to `destination` with `mode` (including the executable bits).
pub fn copy_mode(source: &Path, destination: &Path, mode: u32) -> io::Result<()> {
    let status = Command::new("/bin/sh")
        .args(["-c", COPY, "among-test-executable"])
        .arg(source)
        .arg(destination)
        .arg(format!("{mode:o}"))
        .status()?;
    if !status.success() {
        return Err(io::Error::other(format!(
            "copying executable fixture {} to {} failed with {status}",
            source.display(),
            destination.display()
        )));
    }
    Ok(())
}
