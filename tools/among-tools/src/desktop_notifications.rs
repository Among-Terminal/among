//! Runs the application's real-VTE/GIO notification contract against a Rust
//! notification server on the owned display's private session bus.

use crate::process::{allowed_environment, OwnedProcess};
use crate::{preferences, storage, Result};
use serde_json::json;
use std::fs::{self, File};
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

pub const TEST: &str = "app::desktop_notification_tests::real_bells_send_exact_desktop_payloads_and_clicks_focus_only_current_targets";

pub fn run(executable: &Path, scratch: &Path) -> Result<PathBuf> {
    preferences::owned_display()?;
    if !executable.is_absolute() || !fs::symlink_metadata(executable)?.file_type().is_file() {
        return Err(
            "desktop notification check requires an absolute regular application test executable"
                .into(),
        );
    }
    let base = storage::private_directory(scratch, "among-desktop-notifications")?;
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
    let mut listing = OwnedProcess::spawn(executable, &["--list", "--ignored"], &env, &base, &log)?;
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
        executable,
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
        "executable_sha256": storage::sha256(File::open(executable)?)?,
        "boundary": "real VTE bell and GIO calls on a private session bus; shell display is not asserted",
        "result": "passed"
    });
    fs::write(
        base.join("result.json"),
        serde_json::to_vec_pretty(&report)?,
    )?;
    Ok(base)
}
