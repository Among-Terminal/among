use crate::process::{allowed_environment, OwnedProcess};
use crate::{storage, Result};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::fs::{self, File};
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

pub const PHASES: [&str; 10] = [
    "edit",
    "restore",
    "fault",
    "disable",
    "off",
    "failure",
    "restore",
    "invalid-shortcuts",
    "invalid-version",
    "invalid-plugins",
];

pub fn owned_display() -> Result<()> {
    let home = PathBuf::from(std::env::var_os("HOME").ok_or("display HOME is missing")?);
    owned_display_for(&home)
}

pub(crate) fn owned_display_for(home: &Path) -> Result<()> {
    let base = home.parent().ok_or("display HOME has no parent")?;
    if home.file_name() != Some(std::ffi::OsStr::new("home"))
        || !base
            .file_name()
            .is_some_and(|name| name.to_string_lossy().starts_with("among-rust-display-"))
    {
        return Err("run this check through a Rust owned display runner".into());
    }
    let display = std::env::var("DISPLAY")?;
    for marker in ["xvfb-display.txt", "xephyr-display.txt"] {
        let path = storage::safe_path(base, Path::new(marker))?;
        if path.exists() && fs::read_to_string(path)? == display {
            return Ok(());
        }
    }
    Err("current display differs from owned display evidence".into())
}

pub fn from_cargo(records: &Path, plugins: &Path, scratch: &Path) -> Result<PathBuf> {
    owned_display()?;
    let executables = crate::ignored_tests::executables(&fs::read_to_string(records)?)?;
    let listing_root = storage::private_directory(scratch, "among-preferences-list")?;
    let env = allowed_environment(&std::env::vars_os().collect(), &["PATH", "LANG"]);
    let mut selected = None;
    for (index, executable) in executables.into_iter().enumerate() {
        if fs::symlink_metadata(&executable)?.file_type().is_symlink() || !executable.is_file() {
            return Err("Cargo test executable must be a regular file".into());
        }
        let log_path = listing_root.join(format!("{index}.log"));
        let log = File::create(&log_path)?;
        let mut listing = OwnedProcess::spawn(
            &executable,
            &["--list", "--ignored"],
            &env,
            &std::env::current_dir()?,
            &log,
        )?;
        if !listing.wait(Duration::from_secs(30))?.success() {
            return Err("Preferences test enumeration failed".into());
        }
        if fs::read_to_string(log_path)?
            .lines()
            .any(|line| line == "app::tests::preferences_persist_across_fresh_processes: test")
            && selected.replace(executable).is_some()
        {
            return Err("multiple application Preferences test executables are ambiguous".into());
        }
    }
    run(
        &selected.ok_or("Cargo records contain no application Preferences test")?,
        plugins,
        scratch,
    )
}

pub fn snapshot(config: &Path) -> Result<BTreeMap<String, Vec<u8>>> {
    let mut files = BTreeMap::new();
    for entry in fs::read_dir(config)? {
        let entry = entry?;
        if entry.file_type()?.is_symlink() {
            return Err("preference fixture contains an unsafe symlink".into());
        }
        if entry.file_type()?.is_file() {
            files.insert(
                entry
                    .file_name()
                    .into_string()
                    .map_err(|_| "non-UTF-8 preference filename")?,
                fs::read(entry.path())?,
            );
        }
    }
    Ok(files)
}

fn defaults() -> Value {
    json!({"plugins":[{"name":"clock","enabled":false},{"name":"unavailable","enabled":true},{"name":"system-status","enabled":false},{"name":"pane-pwd","enabled":false}],"keep":42})
}

pub fn verify(
    phase: &str,
    before: &BTreeMap<String, Vec<u8>>,
    after: &BTreeMap<String, Vec<u8>>,
) -> Result<()> {
    if phase != "invalid-plugins" {
        let settings = storage::parse_json(
            after
                .get("plugins.json")
                .ok_or("plugin settings disappeared")?,
        )?;
        if settings["keep"] != 42
            || settings["plugins"][1] != json!({"name":"unavailable","enabled":true})
        {
            return Err("Preferences lost unrelated plugin settings".into());
        }
    }
    if matches!(phase, "restore" | "off" | "fault") && before != after {
        return Err("opening Preferences or process faults saved preferences unexpectedly".into());
    }
    Ok(())
}

pub fn run(executable: &Path, plugins_source: &Path, scratch: &Path) -> Result<PathBuf> {
    owned_display()?;
    let executable = executable.canonicalize()?;
    let plugins_source = plugins_source.canonicalize()?;
    fs::create_dir_all(scratch)?;
    let base = storage::private_directory(scratch, "among-preferences")?;
    for directory in [
        "home",
        "config",
        "config/among",
        "data",
        "data/among",
        "data/among/plugins",
        "cache",
        "state",
    ] {
        fs::DirBuilder::new()
            .mode(0o700)
            .create(base.join(directory))?;
    }
    let config = base.join("config/among");
    let plugins = base.join("data/among/plugins");
    for name in ["clock", "system-status", "pane-pwd"] {
        let source = storage::safe_path(&plugins_source, Path::new(name))?;
        if !source.is_file() {
            return Err(format!("missing prebuilt plugin: {name}").into());
        }
        storage::atomic_write(&plugins, Path::new(name), &fs::read(source)?, 0o755)?;
    }
    storage::atomic_write(
        &config,
        Path::new("plugins.json"),
        &serde_json::to_vec(&defaults())?,
        0o600,
    )?;
    storage::atomic_write(
        &base,
        Path::new("shell"),
        b"#!/bin/sh\nexec /bin/bash --noprofile --norc\n",
        0o755,
    )?;
    let mut env = allowed_environment(
        &std::env::vars_os().collect(),
        &[
            "PATH",
            "LANG",
            "DISPLAY",
            "XDG_RUNTIME_DIR",
            "GSK_RENDERER",
            "LIBGL_ALWAYS_SOFTWARE",
        ],
    );
    for (key, value) in [
        ("HOME", "home"),
        ("XDG_CONFIG_HOME", "config"),
        ("XDG_DATA_HOME", "data"),
        ("XDG_CACHE_HOME", "cache"),
        ("XDG_STATE_HOME", "state"),
    ] {
        env.insert(key.into(), base.join(value).into_os_string());
    }
    for (key, value) in [
        ("GDK_BACKEND", "x11"),
        ("GTK_A11Y", "none"),
        ("G_DEBUG", "fatal-criticals"),
        ("GIO_USE_VFS", "local"),
        ("GSETTINGS_BACKEND", "memory"),
        ("ADW_DISABLE_PORTAL", "1"),
        ("HISTFILE", "/dev/null"),
    ] {
        env.insert(key.into(), value.into());
    }
    env.insert("SHELL".into(), base.join("shell").into_os_string());
    // The application test refuses to touch configuration outside this root.
    env.insert(
        crate::scratch::VARIABLE.into(),
        scratch.canonicalize()?.into_os_string(),
    );
    for (index, phase) in PHASES.iter().enumerate() {
        let session = base.join("data/among/session.json");
        match fs::remove_file(&session) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        if phase.starts_with("invalid-") {
            storage::atomic_write(
                &config,
                Path::new("plugins.json"),
                &serde_json::to_vec(&defaults())?,
                0o600,
            )?;
            storage::atomic_write(
                &config,
                Path::new("shortcuts.json"),
                br#"{"version":1,"overrides":{"split-pane":"F11"}}"#,
                0o600,
            )?;
            let (file, bytes) = match *phase {
                "invalid-plugins" => ("plugins.json", b"malformed plugin settings".as_slice()),
                "invalid-shortcuts" => ("shortcuts.json", b"malformed shortcuts".as_slice()),
                _ => (
                    "shortcuts.json",
                    br#"{"version":2,"overrides":{}}"#.as_slice(),
                ),
            };
            storage::atomic_write(&config, Path::new(file), bytes, 0o600)?;
        }
        let before = snapshot(&config)?;
        env.insert("AMONG_PREFERENCES_TEST_PHASE".into(), (*phase).into());
        env.insert(
            "AMONG_PREFERENCES_SCREENSHOT".into(),
            base.join(format!("{index}-{phase}.png")).into_os_string(),
        );
        let log = File::create(base.join(format!("{index}-{phase}.log")))?;
        let mut child = OwnedProcess::spawn(
            Path::new("/usr/bin/dbus-run-session"),
            &[
                "--",
                executable
                    .to_str()
                    .ok_or("test executable path is not UTF-8")?,
                "app::tests::preferences_persist_across_fresh_processes",
                "--exact",
                "--ignored",
                "--test-threads=1",
                "--nocapture",
            ],
            &env,
            &std::env::current_dir()?,
            &log,
        )?;
        if !child.wait(Duration::from_secs(90))?.success() {
            return Err(format!(
                "Preferences phase {phase} failed; evidence: {}",
                base.display()
            )
            .into());
        }
        verify(phase, &before, &snapshot(&config)?)?;
        println!("PASS {phase}: actual GTK preferences and fresh-process state");
    }
    println!("Evidence: {}", base.display());
    Ok(base)
}
