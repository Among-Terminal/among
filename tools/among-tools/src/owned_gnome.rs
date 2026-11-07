//! An owned GNOME session has its own bus, display, HOME and minimal filesystem.
//! Only copied fixtures and system runtime files enter its mount namespace.

use crate::{deb_installed::Runner, display, process::OwnedProcess, storage, Result};
use nix::fcntl::{fcntl, FcntlArg, OFlag};
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    ffi::OsString,
    fs::{self, File},
    io::Read,
    os::unix::fs::{DirBuilderExt, PermissionsExt},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

pub(crate) const ROOT: &str = "/owned";
pub(crate) const AUTOMATION: &str = r#"import GLib from 'gi://GLib';
export function init() { global.context.unsafe_mode = true; }
export async function run() {
    await new Promise(resolve => GLib.timeout_add(GLib.PRIORITY_DEFAULT, 180000,
        () => { resolve(); return GLib.SOURCE_REMOVE; }));
}
"#;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Mode {
    Wayland,
    X11,
}

impl Mode {
    pub(crate) fn parse(value: &str) -> Result<Self> {
        match value {
            "wayland" => Ok(Self::Wayland),
            "x11" => Ok(Self::X11),
            _ => Err("GNOME mode must be wayland or x11".into()),
        }
    }
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Wayland => "wayland",
            Self::X11 => "x11",
        }
    }
}

/// Existing AppImage checks can use the same guarded Shell API operations.
pub(crate) fn eval(runner: &mut Runner, code: &str) -> Result<String> {
    let text = runner.command(
        "/usr/bin/gdbus",
        &[
            "call",
            "--session",
            "--dest",
            "org.gnome.Shell",
            "--object-path",
            "/org/gnome/Shell",
            "--method",
            "org.gnome.Shell.Eval",
            code,
        ],
        15,
    )?;
    if !text.trim_start().starts_with("(true,") {
        return Err(format!("owned GNOME inspection failed: {text}").into());
    }
    Ok(text)
}

pub(crate) fn screenshot(runner: &mut Runner, path: &Path) -> Result<()> {
    runner.command(
        "/usr/bin/gdbus",
        &[
            "call",
            "--session",
            "--dest",
            "org.gnome.Shell.Screenshot",
            "--object-path",
            "/org/gnome/Shell/Screenshot",
            "--method",
            "org.gnome.Shell.Screenshot.Screenshot",
            "false",
            "false",
            path.to_str().ok_or("screenshot path is not UTF-8")?,
        ],
        15,
    )?;
    Ok(())
}

fn copy_regular(source: &Path, destination: &Path, mode: u32) -> Result<()> {
    if !fs::symlink_metadata(source)?.file_type().is_file() {
        return Err(format!("GNOME fixture must be a regular file: {}", source.display()).into());
    }
    fs::copy(source, destination)?;
    fs::set_permissions(destination, fs::Permissions::from_mode(mode))?;
    Ok(())
}

pub(crate) fn launch(
    source: &Path,
    binary: &Path,
    mode: Mode,
    output: &Path,
    xvfb: Option<&Path>,
) -> Result<PathBuf> {
    for name in ["bwrap", "gnome-shell", "gdbus", "dbus-daemon"] {
        if !Path::new("/usr/bin").join(name).is_file() {
            return Err(format!("owned GNOME needs {name}").into());
        }
    }
    let xvfb = xvfb.unwrap_or(Path::new("/usr/bin/Xvfb"));
    if mode == Mode::X11 && !xvfb.is_file() {
        return Err(
            "owned GNOME X11 requires Xvfb; no personal X server is used as a fallback".into(),
        );
    }
    if output.exists() {
        return Err("GNOME evidence directory must be new".into());
    }
    fs::DirBuilder::new().mode(0o700).create(output)?;
    let base = output.canonicalize()?;
    for directory in [
        "bin",
        "home",
        "config",
        "data",
        "cache",
        "state",
        "tmp",
        "data/applications",
        "data/icons",
        "data/icons/hicolor",
        "data/icons/hicolor/scalable",
        "data/icons/hicolor/scalable/apps",
    ] {
        fs::DirBuilder::new()
            .mode(0o700)
            .create(base.join(directory))?;
    }
    copy_regular(binary, &base.join("bin/among"), 0o755)?;
    copy_regular(
        &std::env::current_exe()?,
        &base.join("bin/among-tools"),
        0o755,
    )?;
    if mode == Mode::X11 {
        copy_regular(xvfb, &base.join("bin/Xvfb"), 0o755)?;
    }
    copy_regular(
        &source.join("data/com.github.among-terminal.svg"),
        &base.join("data/icons/hicolor/scalable/apps/com.github.among-terminal.svg"),
        0o644,
    )?;
    let desktop = fs::read_to_string(source.join("data/com.github.among-terminal.desktop"))?;
    let mut executions = 0;
    let desktop = desktop
        .lines()
        .map(|line| {
            if line.starts_with("Exec=") {
                executions += 1;
                "Exec=/owned/bin/among"
            } else {
                line
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    if executions != 1 {
        return Err("GNOME fixture desktop entry must have exactly one Exec line".into());
    }
    fs::write(
        base.join("data/applications/com.github.among-terminal.desktop"),
        format!("{desktop}\n"),
    )?;
    fs::write(base.join("automation.js"), AUTOMATION)?;
    let uid = nix::unistd::getuid().as_raw();
    let gid = nix::unistd::getgid().as_raw();
    fs::write(
        base.join("passwd"),
        format!("tester:x:{uid}:{gid}:Owned GNOME:/owned/home:/bin/sh\n"),
    )?;
    fs::write(base.join("group"), format!("tester:x:{gid}:\n"))?;
    fs::write(base.join("hosts"), b"127.0.0.1 localhost\n::1 localhost\n")?;
    let mut random = [0_u8; 16];
    File::open("/dev/urandom")?.read_exact(&mut random)?;
    fs::write(
        base.join("machine-id"),
        format!(
            "{}\n",
            random
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>()
        ),
    )?;
    let manifest = json!({
        "schema": 1, "backend": mode.name(),
        "among_sha256": storage::sha256(File::open(base.join("bin/among"))?)?,
        "checker_sha256": storage::sha256(File::open(base.join("bin/among-tools"))?)?,
        "xvfb_sha256": if mode == Mode::X11 { Some(storage::sha256(File::open(base.join("bin/Xvfb"))?)?) } else { None },
        "desktop_id": "com.github.among-terminal.desktop",
        "mount_policy": "system runtime read-only; no host home, display, session bus or device mounts"
    });
    fs::write(
        base.join("inputs.json"),
        serde_json::to_vec_pretty(&manifest)?,
    )?;
    let mut arguments: Vec<OsString> = [
        "--unshare-user",
        "--unshare-pid",
        "--unshare-ipc",
        "--unshare-net",
        "--new-session",
        "--die-with-parent",
        "--ro-bind",
        "/usr",
        "/usr",
        "--symlink",
        "usr/bin",
        "/bin",
        "--symlink",
        "usr/sbin",
        "/sbin",
        "--symlink",
        "usr/lib",
        "/lib",
        "--dir",
        "/etc",
        "--dir",
        "/home",
        "--dir",
        "/root",
        "--dir",
        "/var",
        "--dev",
        "/dev",
        "--proc",
        "/proc",
        "--tmpfs",
        "/run",
        "--dir",
        "/run/among",
        "--dir",
        "/work",
    ]
    .into_iter()
    .map(OsString::from)
    .collect();
    if Path::new("/usr/lib64").exists() {
        arguments.extend(["--symlink", "usr/lib64", "/lib64"].map(OsString::from));
    }
    for path in [
        "/etc/ld.so.cache",
        "/etc/fonts",
        "/etc/dbus-1",
        "/etc/os-release",
    ] {
        if Path::new(path).exists() {
            arguments.extend([OsString::from("--ro-bind"), path.into(), path.into()]);
        }
    }
    for (file, destination) in [
        ("passwd", "/etc/passwd"),
        ("group", "/etc/group"),
        ("hosts", "/etc/hosts"),
        ("machine-id", "/etc/machine-id"),
    ] {
        arguments.extend([
            OsString::from("--ro-bind"),
            base.join(file).into_os_string(),
            destination.into(),
        ]);
    }
    arguments.extend([
        OsString::from("--bind"),
        base.clone().into_os_string(),
        ROOT.into(),
    ]);
    arguments.extend([
        OsString::from("--ro-bind"),
        base.join("bin").into_os_string(),
        "/owned/bin".into(),
    ]);
    arguments.extend([
        OsString::from("--bind"),
        base.join("tmp").into_os_string(),
        "/tmp".into(),
    ]);
    arguments.extend(
        [
            "--chdir",
            ROOT,
            "--",
            "/owned/bin/among-tools",
            "owned-gnome-notification-session",
            mode.name(),
        ]
        .map(OsString::from),
    );
    let arguments: Vec<_> = arguments
        .iter()
        .map(|argument| argument.to_str().ok_or("namespace argument is not UTF-8"))
        .collect::<std::result::Result<_, _>>()?;
    let environment = BTreeMap::from([
        (OsString::from("PATH"), OsString::from("/usr/bin:/bin")),
        ("LANG".into(), "C.UTF-8".into()),
    ]);
    let log = File::create(base.join("namespace.log"))?;
    let mut child = OwnedProcess::spawn(
        Path::new("/usr/bin/bwrap"),
        &arguments,
        &environment,
        &base,
        &log,
    )?;
    let status = child.wait(Duration::from_secs(170))?;
    if !status.success() {
        return Err(format!("owned GNOME proof failed; evidence: {}", base.display()).into());
    }
    let result = storage::parse_json(&fs::read(base.join("result.json"))?)?;
    if result["passed"] != true {
        return Err("owned GNOME proof produced no accepted result".into());
    }
    Ok(base)
}

pub(crate) struct Session {
    pub(crate) runner: Runner,
    pub(crate) mode: Mode,
    shell: OwnedProcess,
    _display: Option<OwnedProcess>,
    _session_bus: OwnedProcess,
    _system_bus: OwnedProcess,
}

impl Session {
    pub(crate) fn start(mode: Mode) -> Result<Self> {
        if std::env::current_dir()? != Path::new(ROOT)
            || std::env::current_exe()? != Path::new("/owned/bin/among-tools")
        {
            return Err("internal GNOME session must run in its owned mount namespace".into());
        }
        let base = PathBuf::from(ROOT);
        fs::set_permissions("/run/among", fs::Permissions::from_mode(0o700))?;
        let mut environment = BTreeMap::from([
            ("PATH".into(), "/usr/bin:/bin".into()),
            ("LANG".into(), "C.UTF-8".into()),
            ("HOME".into(), "/owned/home".into()),
            ("XDG_CONFIG_HOME".into(), "/owned/config".into()),
            ("XDG_DATA_HOME".into(), "/owned/data".into()),
            ("XDG_CACHE_HOME".into(), "/owned/cache".into()),
            ("XDG_STATE_HOME".into(), "/owned/state".into()),
            ("XDG_RUNTIME_DIR".into(), "/run/among".into()),
            (
                "DBUS_SESSION_BUS_ADDRESS".into(),
                "unix:path=/run/among/session-bus".into(),
            ),
            (
                "DBUS_SYSTEM_BUS_ADDRESS".into(),
                "unix:path=/run/among/system-bus".into(),
            ),
            ("XDG_SESSION_TYPE".into(), mode.name().into()),
            ("XDG_CURRENT_DESKTOP".into(), "GNOME".into()),
            ("LIBGL_ALWAYS_SOFTWARE".into(), "1".into()),
            ("LP_NUM_THREADS".into(), "2".into()),
            ("NO_AT_BRIDGE".into(), "1".into()),
            ("SHELL".into(), "/bin/sh".into()),
            ("AMONG_SCRIPT".into(), "/owned/input.script".into()),
            ("AMONG_SELFCHECK".into(), "1".into()),
            ("AMONG_SELFCHECK_TICKS".into(), "180".into()),
        ]);
        let start_bus = |name: &str| -> Result<OwnedProcess> {
            let log = File::create(base.join(format!("{name}-bus.log")))?;
            OwnedProcess::spawn(
                Path::new("/usr/bin/dbus-daemon"),
                &[
                    "--session",
                    "--nofork",
                    &format!("--address=unix:path=/run/among/{name}-bus"),
                ],
                &environment,
                &base,
                &log,
            )
        };
        let system_bus = start_bus("system")?;
        let session_bus = start_bus("session")?;
        let deadline = Instant::now() + Duration::from_secs(10);
        while !Path::new("/run/among/session-bus").exists()
            || !Path::new("/run/among/system-bus").exists()
        {
            if Instant::now() >= deadline {
                return Err("owned bus did not create its socket".into());
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let mut display_process = None;
        if mode == Mode::X11 {
            let log = File::create(base.join("xvfb.log"))?;
            let mut server = OwnedProcess::spawn_capture(
                Path::new("/owned/bin/Xvfb"),
                &[
                    "-displayfd",
                    "1",
                    "-screen",
                    "0",
                    "1280x900x24",
                    "-nolisten",
                    "tcp",
                    "-ac",
                ],
                &environment,
                &base,
                &log,
            )?;
            let mut pipe = server.take_stdout()?;
            let flags = OFlag::from_bits_truncate(fcntl(&pipe, FcntlArg::F_GETFL)?);
            fcntl(&pipe, FcntlArg::F_SETFL(flags | OFlag::O_NONBLOCK))?;
            environment.insert(
                "DISPLAY".into(),
                display::read_display(&mut pipe, Duration::from_secs(30))?.into(),
            );
            environment.insert("GDK_BACKEND".into(), "x11".into());
            display_process = Some(server);
        } else {
            environment.insert("WAYLAND_DISPLAY".into(), "among-wl-0".into());
            environment.insert("GDK_BACKEND".into(), "wayland".into());
        }
        let mut arguments = vec![
            "--sm-disable",
            "--automation-script",
            "/owned/automation.js",
        ];
        if mode == Mode::Wayland {
            arguments.extend([
                "--wayland",
                "--headless",
                "--virtual-monitor=1280x900",
                "--no-x11",
                "--wayland-display=among-wl-0",
            ]);
        } else {
            arguments.push("--x11");
        }
        let log = File::create(base.join("gnome-shell.log"))?;
        let shell = OwnedProcess::spawn(
            Path::new("/usr/bin/gnome-shell"),
            &arguments,
            &environment,
            &base,
            &log,
        )?;
        let mut session = Self {
            runner: Runner::new(environment, base),
            mode,
            shell,
            _display: display_process,
            _session_bus: session_bus,
            _system_bus: system_bus,
        };
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            if !session.shell.is_running()? || Instant::now() >= deadline {
                return Err("owned GNOME failed readiness; inspect gnome-shell.log".into());
            }
            let (success, output) = session.runner.execute("/usr/bin/gdbus", &["call", "--session", "--dest", "org.gnome.Shell", "--object-path", "/org/gnome/Shell", "--method", "org.gnome.Shell.Eval", "(async () => { const Main = await import('resource:///org/gnome/shell/ui/main.js'); return !Main.layoutManager._startingUp; })()"], 2)?;
            if success && output.trim() == "(true, 'true')" {
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        // Wayland advertises keyboard capability only when a seat has a device.
        // A pointer-only headless seat can focus MetaWindow while GTK receives
        // no keyboard enter and consequently reports the window as inactive.
        eval(&mut session.runner, "(async () => { const {default: Clutter} = await import('gi://Clutter'); const seat = global.stage.context.get_backend().get_default_seat(); global.ownedPointer = seat.create_virtual_device(Clutter.InputDeviceType.POINTER_DEVICE); global.ownedKeyboard = seat.create_virtual_device(Clutter.InputDeviceType.KEYBOARD_DEVICE); })()")?;
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let seat = session.json(
                "seat-ready",
                r#"(async () => {
                const {default: Clutter} = await import('gi://Clutter');
                const seat = global.stage.context.get_backend().get_default_seat();
                const devices = seat.list_devices().map(device => ({name:device.get_device_name(),
                    type:device.get_device_type(), capabilities:device.get_capabilities()}));
                return {devices, keyboard_present:devices.some(device =>
                    (device.capabilities & Clutter.InputCapabilities.KEYBOARD) !== 0)};
            })()"#,
            )?;
            if seat["keyboard_present"] == true {
                break;
            }
            if Instant::now() >= deadline {
                return Err(
                    "owned GNOME seat has no keyboard capability; inspect seat-ready.json".into(),
                );
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        Ok(session)
    }

    pub(crate) fn json(&mut self, name: &str, expression: &str) -> Result<Value> {
        storage::relative_path(Path::new(name))?;
        let path = self.runner.base.join(format!("{name}.json"));
        let encoded = serde_json::to_string(&path)?;
        eval(&mut self.runner, &format!("(async () => {{ const {{default: Gio}} = await import('gi://Gio'); const result = await ({expression}); Gio.File.new_for_path({encoded}).replace_contents(JSON.stringify(result),null,false,Gio.FileCreateFlags.REPLACE_DESTINATION,null); return true; }})()"))?;
        storage::parse_json(&fs::read(path)?)
    }

    pub(crate) fn screen(&mut self, name: &str) -> Result<()> {
        storage::relative_path(Path::new(name))?;
        let path = self.runner.base.join(format!("{name}.png"));
        screenshot(&mut self.runner, &path)?;
        let mut signature = [0_u8; 8];
        File::open(path)?.read_exact(&mut signature)?;
        if signature != *b"\x89PNG\r\n\x1a\n" {
            return Err("GNOME screenshot is not a PNG".into());
        }
        Ok(())
    }

    pub(crate) fn alive(&mut self) -> Result<bool> {
        self.shell.is_running()
    }
}
