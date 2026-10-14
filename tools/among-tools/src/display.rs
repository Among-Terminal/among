use crate::process::{allowed_environment, OwnedProcess};
use crate::Result;
use nix::fcntl::{fcntl, FcntlArg, OFlag};
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

pub enum Mode {
    Ci,
    Video,
    VideoGnome,
    Guest,
    NestedGnome,
}

pub fn launch_environment(caller: &BTreeMap<OsString, OsString>) -> BTreeMap<OsString, OsString> {
    allowed_environment(caller, &["PATH", "GITHUB_ACTIONS", "RUNNER_ENVIRONMENT"])
}

pub fn read_display(reader: &mut impl Read, timeout: Duration) -> Result<String> {
    let number = read_line(reader, timeout, 10)?;
    if number.is_empty() || !number.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err("invalid owned display number".into());
    }
    Ok(format!(":{number}"))
}

fn read_line(reader: &mut impl Read, timeout: Duration, maximum: usize) -> Result<String> {
    let deadline = Instant::now() + timeout;
    let mut number = Vec::new();
    loop {
        let mut byte = [0u8];
        match reader.read(&mut byte) {
            Ok(0) => return Err("owned display closed before a complete number line".into()),
            Ok(_) if byte[0] == b'\n' => {
                return Ok(String::from_utf8(number)?);
            }
            Ok(_) => {
                number.push(byte[0]);
                if number.len() > maximum {
                    return Err("owned display number line is too long".into());
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(error) => return Err(error.into()),
        }
        if Instant::now() >= deadline {
            return Err("owned display number readiness timed out".into());
        }
        std::thread::sleep(Duration::from_millis(2));
    }
}

pub fn openbox_configuration(original: &str) -> Result<String> {
    let document = roxmltree::Document::parse(original)?;
    let keyboard = document
        .descendants()
        .find(|node| node.has_tag_name(("http://openbox.org/3.4/rc", "keyboard")))
        .ok_or("Openbox defaults have no keyboard section")?;
    let mut section = original[keyboard.range()].to_string();
    let start = keyboard.range().start;
    let mut bindings = keyboard
        .children()
        .filter(|node| node.is_element() && node.attribute("key") == Some("A-F10"))
        .map(|node| node.range())
        .collect::<Vec<_>>();
    bindings.sort_by_key(|range| range.start);
    for range in bindings.into_iter().rev() {
        section.replace_range(range.start - start..range.end - start, "");
    }
    let closing = section
        .rfind("</")
        .ok_or("Openbox keyboard closing tag is missing")?;
    section.insert_str(
        closing,
        "<keybind key=\"A-F10\"><action name=\"ToggleMaximize\"/></keybind>",
    );
    let mut output = original.to_string();
    output.replace_range(keyboard.range(), &section);
    Ok(output)
}

fn private_directory(parent: &Path) -> Result<PathBuf> {
    fs::create_dir_all(parent)?;
    let path = crate::storage::private_directory(parent, "among-rust-display")?;
    for name in ["home", "runtime", "config", "data", "cache"] {
        fs::DirBuilder::new().mode(0o700).create(path.join(name))?;
    }
    Ok(path)
}

fn environment(scratch: &Path, base: &Path, cairo: bool) -> Result<BTreeMap<OsString, OsString>> {
    let root = scratch.canonicalize()?;
    let caller = std::env::vars_os().collect();
    let mut env = launch_environment(&caller);
    if !env.contains_key(&OsString::from("PATH")) {
        env.insert(
            "PATH".into(),
            "/usr/local/bin:/usr/bin:/bin:/snap/bin".into(),
        );
    }
    for (key, value) in [
        ("LANG", "C.UTF-8"),
        ("XDG_SESSION_TYPE", "x11"),
        ("GDK_BACKEND", "x11"),
        ("GSETTINGS_BACKEND", "memory"),
        ("GIO_USE_VFS", "local"),
        ("GTK_A11Y", "none"),
        ("ADW_DISABLE_PORTAL", "1"),
        ("LIBGL_ALWAYS_SOFTWARE", "1"),
    ] {
        env.insert(key.into(), value.into());
    }
    for (key, directory) in [
        ("HOME", "home"),
        ("XDG_RUNTIME_DIR", "runtime"),
        ("XDG_CONFIG_HOME", "config"),
        ("XDG_DATA_HOME", "data"),
        ("XDG_CACHE_HOME", "cache"),
    ] {
        env.insert(key.into(), base.join(directory).into_os_string());
    }
    // The commands under the display (checks, ignored application tests)
    // write below the same root; they cannot derive it themselves because
    // HOME and XDG_CACHE_HOME above already point into the private base.
    env.insert(
        crate::scratch::VARIABLE.into(),
        root.clone().into_os_string(),
    );
    if cairo {
        env.insert("GSK_RENDERER".into(), "cairo".into());
    }
    if let Some(home) = std::env::var_os("CARGO_HOME") {
        let home = PathBuf::from(home).canonicalize()?;
        if home.starts_with(&root) {
            env.insert("CARGO_HOME".into(), home.into_os_string());
        }
    }
    Ok(env)
}

fn start_server(
    program: &Path,
    nested: bool,
    env: &BTreeMap<OsString, OsString>,
    base: &Path,
    label: &str,
) -> Result<(OwnedProcess, String)> {
    start_server_profile(program, nested, env, base, label, false)
}

fn start_server_profile(
    program: &Path,
    nested: bool,
    env: &BTreeMap<OsString, OsString>,
    base: &Path,
    label: &str,
    video: bool,
) -> Result<(OwnedProcess, String)> {
    let log = File::create(base.join(format!("{label}.log")))?;
    let args = if nested {
        vec![
            "-displayfd",
            "1",
            "-screen",
            if video { "1600x1200" } else { "1920x1200" },
            "-ac",
            "-nolisten",
            "tcp",
            "-no-host-grab",
        ]
    } else {
        vec![
            "-displayfd",
            "1",
            "-screen",
            "0",
            if video {
                "1600x1200x24"
            } else {
                "1920x1200x24"
            },
            "-nolisten",
            "tcp",
            "-ac",
        ]
    };
    let mut server = OwnedProcess::spawn_capture(program, &args, env, base, &log)?;
    let mut pipe = server.take_stdout()?;
    let flags = OFlag::from_bits_truncate(fcntl(&pipe, FcntlArg::F_GETFL)?);
    fcntl(&pipe, FcntlArg::F_SETFL(flags | OFlag::O_NONBLOCK))?;
    // The first X server on a cold, busy CI runner has exceeded 10 seconds
    // before announcing its display. The bound only matters when the server
    // is stuck, so it is generous; the server's own log is quoted then, since
    // the evidence directory is not always retained.
    let display = read_display(&mut pipe, Duration::from_secs(60)).map_err(|error| {
        let log = fs::read_to_string(base.join(format!("{label}.log"))).unwrap_or_default();
        let tail = log.lines().rev().take(20).collect::<Vec<_>>();
        format!(
            "{error}; {label} server log ends with:\n{}",
            tail.into_iter().rev().collect::<Vec<_>>().join("\n")
        )
    })?;
    if !server.is_running()? {
        return Err("owned X server exited during readiness".into());
    }
    fs::write(base.join(format!("{label}-display.txt")), &display)?;
    Ok((server, display))
}

fn probe(
    program: &Path,
    args: &[&str],
    env: &BTreeMap<OsString, OsString>,
    base: &Path,
) -> Result<(bool, String)> {
    let path = base.join("probe.log");
    let log = File::create(&path)?;
    let mut process = OwnedProcess::spawn(program, args, env, base, &log)?;
    let success = process.wait(Duration::from_secs(3))?.success();
    Ok((success, fs::read_to_string(path)?))
}

fn manager_ready(
    server: &mut OwnedProcess,
    manager: &mut OwnedProcess,
    env: &BTreeMap<OsString, OsString>,
    base: &Path,
    gnome: bool,
    timeout: Duration,
) -> Result<()> {
    let deadline = Instant::now() + timeout;
    loop {
        if !server.is_running()? || !manager.is_running()? {
            return Err("owned display/manager exited during readiness".into());
        }
        let (connected, state) = probe(
            Path::new("/usr/bin/xprop"),
            &["-root", "_NET_SUPPORTING_WM_CHECK"],
            env,
            base,
        )?;
        let gnome_ready =
            !gnome || fs::read_to_string(base.join("manager.log"))?.contains("GNOME Shell started");
        if connected && state.contains("window id") && gnome_ready {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err("owned window manager readiness timed out".into());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

pub fn run(
    mode: Mode,
    args: &[String],
    scratch: &Path,
    timeout: Duration,
) -> Result<std::process::ExitStatus> {
    if args.is_empty() {
        return Err("display runner requires a command".into());
    }
    if matches!(mode, Mode::Guest) {
        let output = std::process::Command::new("systemd-detect-virt")
            .env_clear()
            .output()?;
        if !matches!(String::from_utf8(output.stdout)?.trim(), "kvm" | "qemu") {
            return Err("vm-x11 requires a disposable KVM/QEMU guest".into());
        }
    }
    let base = private_directory(scratch)?;
    eprintln!("Owned display evidence: {}", base.display());
    let mut env = environment(scratch, &base, matches!(mode, Mode::Ci | Mode::Video))?;
    let nested = matches!(mode, Mode::NestedGnome | Mode::VideoGnome);
    let mut parent = None;
    let (mut server, display) = if nested {
        // create a second owned server as Xephyr's parent, not the personal screen.
        let (owned_parent, outer_display) =
            start_server(Path::new("/usr/bin/Xvfb"), false, &env, &base, "parent")?;
        env.insert("DISPLAY".into(), outer_display.into());
        parent = Some(owned_parent);
        start_server_profile(
            Path::new("/usr/bin/Xephyr"),
            true,
            &env,
            &base,
            "xephyr",
            matches!(mode, Mode::VideoGnome),
        )?
    } else {
        start_server_profile(
            Path::new("/usr/bin/Xvfb"),
            false,
            &env,
            &base,
            "xvfb",
            matches!(mode, Mode::Video),
        )?
    };
    env.insert("DISPLAY".into(), display.into());
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if !server.is_running()? {
            return Err("owned X server died before client readiness".into());
        }
        if probe(
            Path::new("/usr/bin/xprop"),
            &["-root", "_NET_SUPPORTING_WM_CHECK"],
            &env,
            &base,
        )?
        .0
        {
            break;
        }
        if Instant::now() >= deadline {
            return Err("owned X client readiness timed out".into());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let manager_log_path = base.join("manager.log");
    let manager_log = File::create(&manager_log_path)?;
    let mut system_bus = None;
    if nested {
        let bus_log = File::create(base.join("system-bus.log"))?;
        let address = format!("unix:path={}", base.join("system-bus").display());
        let mut bus = OwnedProcess::spawn_capture(
            Path::new("/usr/bin/dbus-daemon"),
            &[
                "--session",
                "--nofork",
                "--print-address=1",
                "--address",
                &address,
            ],
            &env,
            &base,
            &bus_log,
        )?;
        let mut pipe = bus.take_stdout()?;
        let flags = OFlag::from_bits_truncate(fcntl(&pipe, FcntlArg::F_GETFL)?);
        fcntl(&pipe, FcntlArg::F_SETFL(flags | OFlag::O_NONBLOCK))?;
        let ready = read_line(&mut pipe, Duration::from_secs(10), 4096)?;
        if !ready.starts_with(&address) || !bus.is_running()? {
            return Err("owned system bus failed readiness".into());
        }
        env.insert("DBUS_SYSTEM_BUS_ADDRESS".into(), ready.into());
        system_bus = Some(bus);
    }
    let mut manager = if nested {
        OwnedProcess::spawn(
            Path::new("/usr/bin/dbus-run-session"),
            &["--", "gnome-shell", "--x11", "--sm-disable"],
            &env,
            &base,
            &manager_log,
        )?
    } else {
        let configuration = base.join("openbox.xml");
        fs::write(
            &configuration,
            openbox_configuration(&fs::read_to_string("/etc/xdg/openbox/rc.xml")?)?,
        )?;
        OwnedProcess::spawn(
            Path::new("/usr/bin/openbox"),
            &[
                "--sm-disable",
                "--config-file",
                configuration
                    .to_str()
                    .ok_or("configuration path is not UTF-8")?,
            ],
            &env,
            &base,
            &manager_log,
        )?
    };
    manager_ready(
        &mut server,
        &mut manager,
        &env,
        &base,
        nested,
        Duration::from_secs(30),
    )?;
    if nested
        && !probe(
            Path::new("/usr/bin/xdotool"),
            &["key", "--clearmodifiers", "Escape"],
            &env,
            &base,
        )?
        .0
    {
        return Err("GNOME overview dismissal failed".into());
    }
    let command_log = File::create(base.join("command.log"))?;
    let mut argv = vec!["--"];
    argv.extend(args.iter().map(String::as_str));
    let mut command = OwnedProcess::spawn(
        Path::new("/usr/bin/dbus-run-session"),
        &argv,
        &env,
        &std::env::current_dir()?,
        &command_log,
    )?;
    let status = command.wait(timeout)?;
    std::io::stdout().write_all(&fs::read(base.join("command.log"))?)?;
    manager.cleanup()?;
    if let Some(mut bus) = system_bus {
        bus.cleanup()?;
    }
    server.cleanup()?;
    if let Some(mut parent) = parent {
        parent.cleanup()?;
    }
    Ok(status)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Unrelated(std::process::Child);
    impl Drop for Unrelated {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    #[test]
    #[ignore = "requires disposable X11 container"]
    fn readiness_failures_stop_owned_children_only() -> Result<()> {
        let mut unrelated = Unrelated(std::process::Command::new("/bin/sleep").arg("30").spawn()?);
        let root = crate::scratch::root()?;
        let base = private_directory(&root)?;
        let mut env = environment(&root, &base, true)?;
        assert!(start_server(Path::new("/bin/false"), false, &env, &base, "failed").is_err());
        assert!(start_server(
            Path::new("/missing-x-server"),
            false,
            &env,
            &base,
            "missing"
        )
        .is_err());
        for manager_program in ["/bin/false", "/bin/sleep"] {
            let (mut server, display) =
                start_server(Path::new("/usr/bin/Xvfb"), false, &env, &base, "xvfb")?;
            let server_pid = server.id();
            env.insert("DISPLAY".into(), display.into());
            let log = File::create(base.join("manager.log"))?;
            let arguments = if manager_program == "/bin/sleep" {
                vec!["30"]
            } else {
                vec![]
            };
            let mut manager =
                OwnedProcess::spawn(Path::new(manager_program), &arguments, &env, &base, &log)?;
            let manager_pid = manager.id();
            let error = manager_ready(
                &mut server,
                &mut manager,
                &env,
                &base,
                false,
                Duration::from_millis(30),
            )
            .err()
            .ok_or("invalid manager was accepted")?;
            assert!(error.to_string().contains("readiness"));
            drop(manager);
            drop(server);
            assert!(!Path::new(&format!("/proc/{server_pid}")).exists());
            assert!(!Path::new(&format!("/proc/{manager_pid}")).exists());
            assert!(unrelated.0.try_wait()?.is_none());
        }
        Ok(())
    }

    #[test]
    #[ignore = "requires disposable X11 container"]
    fn command_timeout_preserves_unrelated_child_and_retains_failure_logs() -> Result<()> {
        let mut unrelated = Unrelated(std::process::Command::new("/bin/sleep").arg("30").spawn()?);
        let scratch = private_directory(&crate::scratch::root()?)?;
        let error = run(
            Mode::Ci,
            &["/bin/sleep".into(), "30".into()],
            &scratch,
            Duration::from_millis(20),
        )
        .err()
        .ok_or("timeout was accepted")?;
        assert!(error.to_string().contains("timed out"));
        assert!(unrelated.0.try_wait()?.is_none());
        for entry in fs::read_dir(&scratch)? {
            let base = entry?.path();
            if !base.join("command.log").exists() {
                continue;
            }
            let display = fs::read_to_string(base.join("xvfb-display.txt"))?;
            let output = std::process::Command::new("/usr/bin/xprop")
                .env_clear()
                .env("DISPLAY", display)
                .args(["-root", "_NET_SUPPORTING_WM_CHECK"])
                .output()?;
            assert!(
                !output.status.success(),
                "timed-out display still accepts clients"
            );
            assert!(base.join("manager.log").exists());
        }
        Ok(())
    }
}
