use std::path::{Path, PathBuf};

pub fn compile_fixture(root: &Path) -> PathBuf {
    let executable = root.join("notification-command-fixture");
    let output = std::process::Command::new("rustc")
        .args(["--edition=2021", "-O"])
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/notification-command.rs"))
        .arg("-o")
        .arg(&executable)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "fixture compilation: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    // the ignored native check runs under an environment without the compiler.
    // cache only this synthetic executable beside the Cargo test binaries.
    let cache = std::env::current_exe()
        .unwrap()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("notification-command-fixture");
    std::fs::copy(&executable, cache).unwrap();
    executable
}
