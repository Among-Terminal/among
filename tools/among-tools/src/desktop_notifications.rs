//! Runs the application's real-VTE/GIO notification contract against a Rust
//! notification server on the owned display's private session bus.

use crate::process::{allowed_environment, OwnedProcess};
use crate::{preferences, storage, Result};
use serde_json::json;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::Duration;

pub const TEST: &str = "app::desktop_notification_tests::real_bells_send_exact_desktop_payloads_and_clicks_focus_only_current_targets";

fn snapshot_executable(source: &Path, base: &Path) -> Result<(PathBuf, String)> {
    let mut original = OpenOptions::new()
        .read(true)
        .custom_flags(nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK | nix::libc::O_CLOEXEC)
        .open(source)?;
    let metadata = original.metadata()?;
    const LIMIT: u64 = 512 * 1024 * 1024;
    if !metadata.is_file() || metadata.len() > LIMIT || metadata.permissions().mode() & 0o111 == 0 {
        return Err("desktop test snapshot requires a bounded regular executable".into());
    }
    let mut bytes = Vec::new();
    (&mut original).take(LIMIT + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 != metadata.len() {
        return Err("desktop test executable changed while its snapshot was read".into());
    }
    let path = base.join("application-test");
    let mut copy = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)?;
    copy.write_all(&bytes)?;
    // Cargo may replace the input pathname while a check runs. Only this
    // private read-only image is enumerated, executed and named by its digest.
    fs::set_permissions(
        &path,
        fs::Permissions::from_mode((metadata.permissions().mode() & 0o555) | 0o500),
    )?;
    Ok((path, storage::sha256(bytes.as_slice())?))
}

pub fn run(executable: &Path, scratch: &Path) -> Result<PathBuf> {
    preferences::owned_display()?;
    if !executable.is_absolute() || !fs::symlink_metadata(executable)?.file_type().is_file() {
        return Err(
            "desktop notification check requires an absolute regular application test executable"
                .into(),
        );
    }
    let base = storage::private_directory(scratch, "among-desktop-notifications")?;
    let (snapshot, snapshot_sha256) = snapshot_executable(executable, &base)?;
    let mut env = allowed_environment(
        &std::env::vars_os().collect(),
        &[
            "PATH",
            "LANG",
            "DISPLAY",
            "DBUS_SESSION_BUS_ADDRESS",
            "XDG_RUNTIME_DIR",
            "GSK_RENDERER",
            "LIBGL_ALWAYS_SOFTWARE",
        ],
    );
    for (key, directory) in [
        ("HOME", "home"),
        ("XDG_CONFIG_HOME", "config"),
        ("XDG_DATA_HOME", "data"),
        ("XDG_CACHE_HOME", "cache"),
        ("XDG_STATE_HOME", "state"),
    ] {
        let path = base.join(directory);
        fs::DirBuilder::new().mode(0o700).create(&path)?;
        env.insert(key.into(), path.into_os_string());
    }
    for (key, value) in [
        ("SHELL", "/bin/sh"),
        ("GDK_BACKEND", "x11"),
        ("GTK_A11Y", "none"),
        ("GIO_USE_VFS", "local"),
        ("GSETTINGS_BACKEND", "memory"),
        ("ADW_DISABLE_PORTAL", "1"),
    ] {
        env.insert(key.into(), value.into());
    }
    env.insert(
        crate::scratch::VARIABLE.into(),
        scratch.canonicalize()?.into_os_string(),
    );
    let log = File::create(base.join("listing.log"))?;
    let mut listing = OwnedProcess::spawn(&snapshot, &["--list", "--ignored"], &env, &base, &log)?;
    if !listing.wait(Duration::from_secs(30))?.success() {
        return Err("desktop notification test enumeration failed".into());
    }
    if fs::read_to_string(base.join("listing.log"))?
        .lines()
        .filter(|line| *line == format!("{TEST}: test"))
        .count()
        != 1
    {
        return Err(
            "application executable does not declare exactly one desktop notification check".into(),
        );
    }
    let log = File::create(base.join("test.log"))?;
    let mut test = OwnedProcess::spawn(
        &snapshot,
        &[
            TEST,
            "--exact",
            "--ignored",
            "--test-threads=1",
            "--nocapture",
        ],
        &env,
        &base,
        &log,
    )?;
    let status = test.wait(Duration::from_secs(120))?;
    let output = fs::read_to_string(base.join("test.log"))?;
    println!("{output}");
    if !status.success() || !output.contains("test result: ok. 1 passed; 0 failed;") {
        return Err(format!(
            "desktop notification check failed; evidence: {}",
            base.display()
        )
        .into());
    }
    let report = json!({
        "schema": 1, "test": TEST, "executable": executable,
        "executed_snapshot": snapshot,
        "executable_sha256": snapshot_sha256,
        "boundary": "real VTE bell and GIO calls on a private session bus; shell display is not asserted",
        "result": "passed"
    });
    fs::write(
        base.join("result.json"),
        serde_json::to_vec_pretty(&report)?,
    )?;
    Ok(base)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[test]
    fn replacing_the_original_path_cannot_change_the_enumerated_and_executed_snapshot() -> Result<()>
    {
        let base = storage::private_directory(&crate::scratch::root()?, "desktop-snapshot-unit")?;
        let original = base.join("original");
        let replacement = base.join("replacement");
        for (path, replace) in [(&original, false), (&replacement, true)] {
            let mut compiler = std::process::Command::new("rustc");
            compiler
                .args(["--edition=2021", "--cfg", "snapshot_image"])
                .arg(
                    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/desktop-listing.rs"),
                )
                .arg("-o")
                .arg(path);
            if replace {
                compiler.args(["--cfg", "replacement_image"]);
            }
            let output = compiler.output()?;
            if !output.status.success() {
                return Err(String::from_utf8_lossy(&output.stderr).into_owned().into());
            }
        }
        let execute_permissions = original.metadata()?.permissions().mode() & 0o111;
        let (snapshot, reported_hash) = snapshot_executable(&original, &base)?;
        let log = File::create(base.join("listing.log"))?;
        let env = BTreeMap::from([("PATH".into(), "/usr/bin:/bin".into())]);
        let mut listing =
            OwnedProcess::spawn(&snapshot, &["--list", "--ignored"], &env, &base, &log)?;
        assert!(listing.wait(Duration::from_secs(5))?.success());
        fs::rename(&replacement, &original)?;
        let actual_log = File::create(base.join("actual.log"))?;
        let mut actual = OwnedProcess::spawn(&snapshot, &[], &env, &base, &actual_log)?;
        assert!(actual.wait(Duration::from_secs(5))?.success());
        let control_log = File::create(base.join("control.log"))?;
        let mut control = OwnedProcess::spawn(&original, &[], &env, &base, &control_log)?;
        assert!(control.wait(Duration::from_secs(5))?.success());
        assert_eq!(
            fs::read_to_string(base.join("listing.log"))?.trim(),
            "original:listing"
        );
        assert_eq!(
            fs::read_to_string(base.join("actual.log"))?.trim(),
            "original:execution"
        );
        assert_eq!(
            fs::read_to_string(base.join("control.log"))?.trim(),
            "replacement:execution"
        );
        assert_eq!(reported_hash, storage::sha256(File::open(&snapshot)?)?);
        assert_ne!(reported_hash, storage::sha256(File::open(&original)?)?);
        assert_eq!(
            snapshot.metadata()?.permissions().mode() & 0o111,
            execute_permissions
        );
        fs::remove_dir_all(base)?;
        Ok(())
    }

    #[test]
    fn snapshot_refuses_links_and_non_regular_or_non_executable_inputs() -> Result<()> {
        let base =
            storage::private_directory(&crate::scratch::root()?, "desktop-snapshot-refusal")?;
        let plain = base.join("plain");
        fs::write(&plain, b"not executable")?;
        assert!(snapshot_executable(&plain, &base).is_err());
        let link = base.join("link");
        std::os::unix::fs::symlink("/usr/bin/true", &link)?;
        assert!(snapshot_executable(&link, &base).is_err());
        assert!(snapshot_executable(&base, &base).is_err());
        let fifo = base.join("fifo");
        nix::unistd::mkfifo(&fifo, nix::sys::stat::Mode::S_IRUSR)?;
        assert!(snapshot_executable(&fifo, &base).is_err());
        fs::remove_dir_all(base)?;
        Ok(())
    }
}
