use crate::{
    process::{allowed_environment, OwnedProcess},
    storage, Result,
};
use std::collections::BTreeSet;
use std::fs::{self, File};
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub fn run(
    root: &Path,
    app: &Path,
    built: &Path,
    installer: &Path,
    scratch: &Path,
) -> Result<PathBuf> {
    crate::preferences::owned_display()?;
    let root = root.canonicalize()?;
    let app = app.canonicalize()?;
    let built = built.canonicalize()?;
    let installer = installer.canonicalize()?;
    let names = ["clock", "system-status", "pane-pwd"];
    let source_names = fs::read_dir(root.join("examples/plugins/src/bin"))?
        .map(|entry| {
            let path = entry?.path();
            Ok(path
                .extension()
                .is_some_and(|ext| ext == "rs")
                .then(|| {
                    path.file_stem()
                        .map(|stem| stem.to_string_lossy().to_string())
                })
                .flatten())
        })
        .collect::<Result<Vec<_>>>()?
        .into_iter()
        .flatten()
        .collect::<BTreeSet<_>>();
    if source_names
        != names
            .iter()
            .map(|name| name.to_string())
            .collect::<BTreeSet<_>>()
    {
        return Err("shipped plugin source inventory differs from approved three plugins".into());
    }
    fs::create_dir_all(scratch)?;
    let base = scratch.join(format!(
        "among-install-check-{}-{}",
        std::process::id(),
        SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos()
    ));
    fs::DirBuilder::new().mode(0o700).create(&base)?;
    for name in ["home", "config", "data", "cache", "state"] {
        fs::DirBuilder::new().mode(0o700).create(base.join(name))?;
    }
    let mut env = allowed_environment(
        &std::env::vars_os().collect(),
        &[
            "PATH",
            "LANG",
            "DISPLAY",
            "XDG_RUNTIME_DIR",
            "LIBGL_ALWAYS_SOFTWARE",
            "GSK_RENDERER",
        ],
    );
    for (key, dir) in [
        ("HOME", "home"),
        ("XDG_CONFIG_HOME", "config"),
        ("XDG_DATA_HOME", "data"),
        ("XDG_CACHE_HOME", "cache"),
        ("XDG_STATE_HOME", "state"),
    ] {
        env.insert(key.into(), base.join(dir).into_os_string());
    }
    let install_log = File::create(base.join("install.log"))?;
    let mut install =
        OwnedProcess::spawn(&installer, &["--enable-new"], &env, &root, &install_log)?;
    if !install.wait(Duration::from_secs(60))?.success() {
        return Err(format!("plugin installation failed; evidence: {}", base.display()).into());
    }
    let config = base.join("config/among/plugins.json");
    let original = fs::read(&config)?;
    let document = storage::parse_json(&original)?;
    let entries = document["plugins"]
        .as_array()
        .ok_or("plugin config has no entries")?;
    if entries.len() != names.len()
        || !entries
            .iter()
            .zip(names)
            .all(|(entry, name)| entry["name"] == name && entry["enabled"] == false)
    {
        return Err("fresh plugin installation must add exactly three disabled entries".into());
    }
    let installed = base.join("data/among/plugins");
    let installed_names = fs::read_dir(&installed)?
        .map(|entry| Ok(entry?.file_name().to_string_lossy().to_string()))
        .collect::<Result<BTreeSet<_>>>()?;
    if installed_names
        != names
            .iter()
            .map(|name| name.to_string())
            .collect::<BTreeSet<_>>()
    {
        return Err("installed plugin payload differs from approved inventory".into());
    }
    for name in names {
        let source = fs::read(storage::safe_path(&built, Path::new(name))?)?;
        let destination = fs::read(storage::safe_path(&installed, Path::new(name))?)?;
        if !destination.starts_with(b"\x7fELF")
            || storage::sha256(source.as_slice())? != storage::sha256(destination.as_slice())?
        {
            return Err("installed plugin bytes differ from checked native binary".into());
        }
    }
    for (key, value) in [
        ("AMONG_SELFCHECK", "1"),
        ("AMONG_SELFCHECK_TICKS", "1000"),
        ("GDK_BACKEND", "x11"),
        ("GTK_A11Y", "none"),
        ("G_DEBUG", "fatal-criticals"),
        ("GIO_USE_VFS", "local"),
        ("ADW_DISABLE_PORTAL", "1"),
        ("SHELL", "/bin/sh"),
        ("HISTFILE", "/dev/null"),
    ] {
        env.insert(key.into(), value.into());
    }
    for phase in ["disabled", "enabled"] {
        let session = base.join("data/among/session.json");
        match fs::remove_file(session) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        if phase == "enabled" {
            env.insert("AMONG_ENABLE".into(), names.join(",").into());
        }
        let log_path = base.join(format!("{phase}.log"));
        let log = File::create(&log_path)?;
        let mut app = OwnedProcess::spawn(
            Path::new("/usr/bin/dbus-run-session"),
            &["--", app.to_str().ok_or("app path is not UTF-8")?],
            &env,
            &base,
            &log,
        )?;
        let deadline = Instant::now() + Duration::from_secs(15);
        let state = if phase == "disabled" {
            "Disabled"
        } else {
            "Running"
        };
        let text = loop {
            let text = fs::read_to_string(&log_path)?;
            if names
                .iter()
                .all(|name| text.contains(&format!("{name}={state}")))
            {
                break text;
            }
            if !app.is_running()? || Instant::now() >= deadline {
                return Err(
                    format!("plugin discovery failed; evidence: {}", log_path.display()).into(),
                );
            }
            std::thread::sleep(Duration::from_millis(100));
        };
        if !text.contains("pane_numbers=[\"#1\"]") || !text.contains("sidebar_states=[0:0:false:1]")
        {
            return Err("fresh plugin test lost native pane counts".into());
        }
        if phase == "disabled"
            && (!text.contains("app_header=[]") || !text.contains("app_footer=[]"))
        {
            return Err("disabled plugins emitted fragments".into());
        }
        if fs::read(&config)? != original {
            return Err("runtime plugin switches persisted into configuration".into());
        }
        if text.contains("CRITICAL") || text.contains("panicked") {
            return Err("plugin test observed native critical/panic".into());
        }
        app.cleanup()?;
        println!("PASS documented fresh plugin install: {phase}, built-in counts and pane number");
    }
    println!("Evidence: {}", base.display());
    Ok(base)
}
