//! native acceptance delegates installation to the actual host UI, never a
//! standalone copier or a hand-written installed.json receipt.

use crate::{
    process::{allowed_environment, OwnedProcess},
    storage, Result,
};
use std::fs::{self, File};
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

pub mod release;

pub const NATIVE_TEST: &str = "app::plugin_tests::installation::real_published_official_packages_install_review_enable_disable_uninstall";

pub fn run(executable: &Path, inputs: &Path, scratch: &Path) -> Result<PathBuf> {
    crate::preferences::owned_display()?;
    let executable = executable.canonicalize()?;
    let inputs = inputs.canonicalize()?;
    release::verify(&inputs)?;
    fs::create_dir_all(scratch)?;
    let base = storage::private_directory(scratch, "among-release-install-check")?;
    for name in ["home", "config", "data", "cache", "state", "run"] {
        fs::DirBuilder::new().mode(0o700).create(base.join(name))?;
    }
    let mut environment = allowed_environment(
        &std::env::vars_os().collect(),
        &[
            "PATH",
            "LANG",
            "DISPLAY",
            "LIBGL_ALWAYS_SOFTWARE",
            "GSK_RENDERER",
        ],
    );
    for (key, directory) in [
        ("HOME", "home"),
        ("XDG_CONFIG_HOME", "config"),
        ("XDG_DATA_HOME", "data"),
        ("XDG_CACHE_HOME", "cache"),
        ("XDG_STATE_HOME", "state"),
        ("XDG_RUNTIME_DIR", "run"),
    ] {
        environment.insert(key.into(), base.join(directory).into_os_string());
    }
    for (key, value) in [
        ("GDK_BACKEND", "x11"),
        ("GTK_A11Y", "test"),
        ("G_DEBUG", "fatal-criticals"),
        ("GIO_USE_VFS", "local"),
        ("ADW_DISABLE_PORTAL", "1"),
        ("SHELL", "/bin/sh"),
        ("HISTFILE", "/dev/null"),
    ] {
        environment.insert(key.into(), value.into());
    }
    environment.insert("AMONG_CHECK_ROOT".into(), base.clone().into_os_string());
    environment.insert(
        "AMONG_OFFICIAL_PLUGIN_INPUTS".into(),
        inputs.clone().into_os_string(),
    );
    let list_path = base.join("test-names.txt");
    let list = File::create(&list_path)?;
    let mut listing = OwnedProcess::spawn(&executable, &["--list"], &environment, &base, &list)?;
    if !listing.wait(Duration::from_secs(10))?.success()
        || fs::read_to_string(list_path)?
            .lines()
            .filter(|line| *line == format!("{NATIVE_TEST}: test"))
            .count()
            != 1
    {
        return Err(
            "current host does not provide the exact published-package native acceptance test"
                .into(),
        );
    }
    let log_path = base.join("native-install.log");
    let log = File::create(&log_path)?;
    let mut child = OwnedProcess::spawn(
        Path::new("/usr/bin/dbus-run-session"),
        &[
            "--",
            executable.to_str().ok_or("host test path is not UTF-8")?,
            NATIVE_TEST,
            "--exact",
            "--ignored",
            "--test-threads=1",
            "--nocapture",
            "--color=never",
        ],
        &environment,
        &base,
        &log,
    )?;
    let status = child.wait(Duration::from_secs(240))?;
    let output = fs::read_to_string(&log_path)?;
    if !status.success() || !output.contains("test result: ok. 1 passed; 0 failed; 0 ignored;")
        || !output.contains("PASS published_release_ui install_review_enable_disable_uninstall=true terminal_input=true")
        || output.contains("CRITICAL") || output.contains("panicked")
    { return Err(format!("published-package native installation not accepted; evidence: {}", base.display()).into()); }
    release::verify(&inputs)?;
    println!(
        "PASS actual published-package native installation; evidence: {}",
        base.display()
    );
    Ok(base)
}
