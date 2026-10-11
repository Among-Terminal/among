use crate::{process::OwnedProcess, sessions::fresh, Result};
use std::fs::{self, File};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

pub fn session(runner_home: &Path, entry: &Path) -> Result<()> {
    crate::preferences::owned_display_for(runner_home)?;
    if !Path::new("/usr/bin/gio").is_file() {
        return Err("GIO desktop launcher unavailable; install libglib2.0-bin in the isolated acceptance environment".into());
    }
    let status = std::process::Command::new("/usr/bin/gio")
        .arg("launch")
        .arg(entry)
        .status()?;
    if !status.success() {
        return Err("GIO desktop launch failed".into());
    }
    // the outer owned D-Bus process group contains GApplication after gio exits.
    std::thread::sleep(Duration::from_secs(60));
    Ok(())
}

pub fn run(root: &Path, binary: &Path, scratch: &Path) -> Result<PathBuf> {
    let root = root.canonicalize()?;
    let binary = binary.canonicalize()?;
    let mut h = fresh(&binary, scratch, "among-desktop")?;
    let tools = std::env::current_exe()?;
    let runner_home = std::env::var("HOME")?;
    h.env
        .insert("AMONG_TOOLS".into(), tools.as_os_str().to_owned());
    let desktop = root.join("data/com.github.among-terminal.desktop");
    let metadata = root.join("data/com.github.among-terminal.metainfo.xml");
    h.probe(
        "/usr/bin/desktop-file-validate",
        &[desktop.to_str().ok_or("desktop path not UTF-8")?],
    )?;
    h.probe(
        "/usr/bin/appstreamcli",
        &[
            "validate",
            "--no-net",
            "--strict",
            metadata.to_str().ok_or("metadata path not UTF-8")?,
        ],
    )?;
    let installer = root.join("scripts/install.sh");
    let log = File::create(h.base.join("install.log"))?;
    let mut install = OwnedProcess::spawn(
        Path::new("/bin/sh"),
        &[
            installer.to_str().ok_or("installer path not UTF-8")?,
            "--binary",
            binary.to_str().ok_or("binary path not UTF-8")?,
        ],
        &h.env,
        &root,
        &log,
    )?;
    if !install.wait(Duration::from_secs(60))?.success() {
        return Err(format!("desktop fixture install failed: {}", h.base.display()).into());
    }
    let entry = h
        .base
        .join("data/applications/com.github.among-terminal.desktop");
    let icon = h
        .base
        .join("data/icons/hicolor/scalable/apps/com.github.among-terminal.svg");
    let result = (|| -> Result<()> {
        if !icon.is_file() {
            return Err("installed application icon missing".into());
        }
        h.probe(
            "/usr/bin/desktop-file-validate",
            &[entry.to_str().ok_or("entry path not UTF-8")?],
        )?;
        h.log = h.base.join("launch.log");
        let log = File::create(&h.log)?;
        h.process = Some(OwnedProcess::spawn(
            Path::new("/usr/bin/dbus-run-session"),
            &[
                "--",
                tools.to_str().ok_or("tool path not UTF-8")?,
                "desktop-session",
                &runner_home,
                entry.to_str().ok_or("entry path not UTF-8")?,
            ],
            &h.env,
            &h.base,
            &log,
        )?);
        h.wait(15, |this| {
            let text = this.text()?;
            Ok(text.contains("session_ready=true") && text.contains("has_focus=true"))
        })?;
        let windows = h.probe(
            "/usr/bin/xdotool",
            &["search", "--onlyvisible", "--name", "^Among Terminal$"],
        )?;
        h.window = windows
            .split_whitespace()
            .last()
            .ok_or("desktop window missing")?
            .to_string();
        if h.probe("/usr/bin/xdotool", &["getwindowname", &h.window])?
            .trim()
            != "Among Terminal"
        {
            return Err("desktop launch has wrong title".into());
        }
        let class = fs::read_to_string(&entry)?
            .lines()
            .find_map(|line| line.strip_prefix("StartupWMClass=").map(str::to_string))
            .ok_or("desktop WM class missing")?;
        if !h
            .probe("/usr/bin/xprop", &["-id", &h.window, "WM_CLASS"])?
            .contains(&format!("\"{class}\""))
        {
            return Err("desktop WM class mismatch".into());
        }
        h.wait(15, |this| {
            let text = this.text()?;
            Ok(text.contains("application_icon=true")
                && text.contains(icon.to_str().ok_or("icon path not UTF-8")?))
        })?;
        h.clean_log(&h.log)?;
        h.screenshot("desktop.png")?;
        println!("PASS installed desktop GIO launch, title/class and native GTK icon lookup");
        Ok(())
    })();
    h.stop(false)?;
    let uninstaller = h.base.join("home/.local/bin/among-uninstall");
    h.probe(
        uninstaller.to_str().ok_or("uninstaller path not UTF-8")?,
        &[],
    )?;
    if entry.exists() || icon.exists() {
        return Err("desktop removal left installed entry/icon".into());
    }
    result?;
    println!("Evidence: {}", h.base.display());
    Ok(h.base.clone())
}

pub fn installed(scratch: &Path) -> Result<PathBuf> {
    if std::env::var("GITHUB_ACTIONS").as_deref() != Ok("true") {
        return Err("installed desktop checks run only in release CI".into());
    }
    let binary = Path::new("/usr/bin/among");
    let entry = Path::new("/usr/share/applications/com.github.among-terminal.desktop");
    let icon = Path::new("/usr/share/icons/hicolor/scalable/apps/com.github.among-terminal.svg");
    if !fs::read_to_string(entry)?
        .lines()
        .any(|line| line == "Exec=/usr/bin/among")
    {
        return Err("installed desktop launcher is not bound to system executable".into());
    }
    let mut h = fresh(binary, scratch, "among-deb-desktop")?;
    h.probe(
        "/usr/bin/desktop-file-validate",
        &[entry.to_str().ok_or("entry path is not UTF-8")?],
    )?;
    h.probe(
        "/usr/bin/appstreamcli",
        &[
            "validate",
            "--no-net",
            "--strict",
            "/usr/share/metainfo/com.github.among-terminal.metainfo.xml",
        ],
    )?;
    let tools = std::env::current_exe()?;
    let runner_home = std::env::var("HOME")?;
    h.log = h.base.join("launch.log");
    let log = File::create(&h.log)?;
    h.process = Some(OwnedProcess::spawn(
        Path::new("/usr/bin/dbus-run-session"),
        &[
            "--",
            tools.to_str().ok_or("tool path is not UTF-8")?,
            "desktop-session",
            &runner_home,
            entry.to_str().ok_or("entry path is not UTF-8")?,
        ],
        &h.env,
        &h.base,
        &log,
    )?);
    let result = (|| -> Result<()> {
        h.wait(30, |this| {
            if !this
                .process
                .as_mut()
                .ok_or("owned desktop launcher absent")?
                .is_running()?
            {
                return Err(format!(
                    "owned desktop launcher exited before readiness: {}",
                    this.text()?
                )
                .into());
            }
            // Session loading completes before GTK maps the window. Do not
            // issue a one-shot visible-window probe against that earlier state.
            let text = this.text()?;
            Ok(text.contains("session_ready=true") && text.contains("has_focus=true"))
        })?;
        let windows = h.probe(
            "/usr/bin/xdotool",
            &["search", "--onlyvisible", "--name", "^Among Terminal$"],
        )?;
        h.window = windows
            .split_whitespace()
            .last()
            .ok_or("installed desktop window missing")?
            .to_string();
        let pid = h
            .probe("/usr/bin/xdotool", &["getwindowpid", &h.window])?
            .trim()
            .parse::<u32>()?;
        let process = PathBuf::from(format!("/proc/{pid}"));
        if fs::read_link(process.join("exe"))? != binary
            || fs::metadata(&process)?.uid() != nix::unistd::geteuid().as_raw()
        {
            return Err(
                "desktop window does not belong to the current synthetic user's system executable"
                    .into(),
            );
        }
        let identity = h.probe("/usr/bin/xprop", &["-id", &h.window, "_GTK_APPLICATION_ID"])?;
        if !identity.contains("\"com.github.among-terminal\"") {
            return Err("installed desktop application identity differs".into());
        }
        h.wait(15, |this| {
            let text = this.text()?;
            Ok(text.contains("application_icon=true")
                && text.contains(icon.to_str().ok_or("icon path is not UTF-8")?))
        })?;
        h.xdo(&["windowactivate", "--sync", &h.window])?;
        let marker = h.base.join("terminal-input");
        h.xdo(&[
            "type",
            "--clearmodifiers",
            &format!("printf 'installed-input' > '{}'", marker.display()),
        ])?;
        h.key("Return")?;
        h.wait(15, |_| {
            Ok(fs::read_to_string(&marker).is_ok_and(|text| text == "installed-input"))
        })?;
        h.clean_log(&h.log)?;
        h.screenshot("installed-desktop.png")?;
        Ok(())
    })();
    h.stop(false)?;
    result?;
    println!("PASS installed system GIO launch, application identity, native SVG lookup and host-shell input; evidence: {}", h.base.display());
    Ok(h.base.clone())
}
