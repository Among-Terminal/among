use crate::{
    process::{allowed_environment, OwnedProcess},
    storage, Result,
};
use nix::sys::signal::Signal;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs::{self, File};
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Bound on the helper output quoted in a probe failure, so a chatty helper
/// cannot flood the CI log while its last lines, where errors land, survive.
const PROBE_OUTPUT_LIMIT: usize = 2000;

/// Describe a helper that exited unsuccessfully: which program, with which
/// arguments, how it ended and the end of what it printed. CI keeps only the
/// job log, so without this a failed xdotool, import or clipboard call is
/// indistinguishable from any other. A helper that hangs never gets here:
/// `OwnedProcess::wait` reports the timeout itself.
fn probe_failure(
    program: &str,
    args: &[&str],
    status: std::process::ExitStatus,
    output: &[u8],
) -> String {
    let mut start = output.len().saturating_sub(PROBE_OUTPUT_LIMIT);
    // Skip UTF-8 continuation bytes so the cut does not split a character.
    while output.get(start).is_some_and(|byte| byte & 0xC0 == 0x80) {
        start += 1;
    }
    let text = String::from_utf8_lossy(&output[start..]);
    let omitted = if start > 0 {
        format!(" (first {start} bytes omitted)")
    } else {
        String::new()
    };
    format!(
        "session probe failed: {program} {args:?} ended with {status}; output{omitted}:\n{}",
        text.trim_end()
    )
}

/// An X error raised because a window vanished while `xdotool search` walked
/// the tree (`X_QueryTree`) or, for `--onlyvisible`, read the map state of a
/// window it had just listed (`X_GetWindowAttributes`).
fn transient_window_race(message: &str) -> bool {
    message.contains("BadWindow (invalid Window parameter)")
        && (message.contains("X_QueryTree") || message.contains("X_GetWindowAttributes"))
}

fn require(condition: bool, message: &str) -> Result<()> {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

fn key_arguments(key: &str) -> [&str; 5] {
    [
        "key",
        "--clearmodifiers",
        "--delay",
        "100",
        if key == "F11" { "95" } else { key },
    ]
}

fn find_application_pid(
    parent: u32,
    mut read: impl FnMut(u32) -> Result<(String, Vec<u32>)>,
) -> Result<u32> {
    let mut pending = vec![parent];
    let mut visited = std::collections::BTreeSet::new();
    let mut candidates = Vec::new();
    while let Some(pid) = pending.pop() {
        if !visited.insert(pid) {
            continue;
        }
        if visited.len() > 512 {
            return Err("owned application process tree exceeds test bound".into());
        }
        let (name, children) = read(pid)?;
        if pid != parent && name == "among" {
            candidates.push(pid);
        }
        pending.extend(children);
    }
    match candidates.as_slice() {
        [pid] => Ok(*pid),
        [] => Err("native application missing from owned launcher process tree".into()),
        _ => Err("ambiguous native applications in owned launcher process tree".into()),
    }
}

fn owned_application_pid(parent: u32) -> Result<u32> {
    // AppImage extraction and launcher interpreters add processes between
    // dbus-run-session and GTK. Never substitute a global `pgrep among`: only
    // descendants of the process we started are eligible for test input/signals.
    find_application_pid(parent, |pid| {
        let name = match fs::read_to_string(format!("/proc/{pid}/comm")) {
            Ok(name) => name,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok((String::new(), Vec::new()))
            }
            Err(error) => return Err(error.into()),
        };
        let children = match fs::read_to_string(format!("/proc/{pid}/task/{pid}/children")) {
            Ok(children) => children,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok((String::new(), Vec::new()))
            }
            Err(error) => return Err(error.into()),
        };
        Ok((
            name.trim().into(),
            children
                .split_whitespace()
                .map(str::parse)
                .collect::<std::result::Result<Vec<u32>, _>>()?,
        ))
    })
}

pub fn rectangles(text: &str, name: &str) -> Vec<[f64; 4]> {
    let needle = format!("{name}:");
    text.match_indices(&needle)
        .filter_map(|(index, _)| {
            if index > 0 && text.as_bytes()[index - 1].is_ascii_digit() {
                return None;
            }
            let value = &text[index + needle.len()..];
            let token = value
                .split(|character: char| {
                    !character.is_ascii_digit() && !matches!(character, '.' | '+' | 'x')
                })
                .next()?;
            let values = token
                .split(['+', 'x'])
                .map(str::parse::<f64>)
                .collect::<std::result::Result<Vec<_>, _>>()
                .ok()?;
            let rect: [f64; 4] = values.try_into().ok()?;
            rect.iter()
                .all(|value| value.is_finite() && *value >= 0.0)
                .then_some(rect)
        })
        .collect()
}

pub fn pane_rectangles(text: &str, name: &str) -> Vec<[f64; 4]> {
    let boxes = text
        .split_once("tree_boxes=[")
        .and_then(|(_, tail)| tail.split_once(']'))
        .map(|(boxes, _)| boxes)
        .unwrap_or("");
    rectangles(boxes, name)
}

pub(crate) fn pair(text: &str, name: &str, separator: char) -> Result<[i64; 2]> {
    let token = text
        .split_once(&format!("{name}="))
        .ok_or("missing geometry pair")?
        .1
        .split(|character: char| !character.is_ascii_digit() && character != separator)
        .next()
        .ok_or("missing geometry token")?;
    let values = token
        .split(separator)
        .map(str::parse::<i64>)
        .collect::<std::result::Result<Vec<_>, _>>()?;
    values
        .try_into()
        .map_err(|_| "invalid geometry pair".into())
}

/// One owned application run with private XDG directories. Public beyond the
/// crate so that tools built on this library drive the same harness instead of
/// keeping a second copy of it.
pub struct Harness {
    pub base: PathBuf,
    data: PathBuf,
    session: PathBuf,
    launched: PathBuf,
    pub(crate) app: PathBuf,
    pub env: BTreeMap<OsString, OsString>,
    pub(crate) process: Option<OwnedProcess>,
    pub log: PathBuf,
    pub(crate) window: String,
}

impl Drop for Harness {
    fn drop(&mut self) {
        let _ = fs::set_permissions(&self.data, fs::Permissions::from_mode(0o700));
        let denied = self.base.join("inaccessible");
        if denied.exists() {
            let _ = fs::set_permissions(denied, fs::Permissions::from_mode(0o700));
        }
    }
}

impl Harness {
    pub fn text(&self) -> Result<String> {
        Ok(fs::read_to_string(&self.log)?)
    }
    /// Remove the session file this harness owns, if there is one.
    ///
    /// A launch that finds one asks the user whether to restore it, and the
    /// application stays behind that question: no keyboard focus, and none of the
    /// window a pointer test needs. A later launch therefore starts clean.
    pub(crate) fn forget_session(&self) -> Result<()> {
        match fs::remove_file(&self.session) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error.into()),
        }
    }
    fn snapshot(&self) -> Result<Value> {
        storage::parse_json(&fs::read(&self.session)?)
    }
    /// The snapshot as written so far, or `None` while none has been written.
    ///
    /// A caller waiting for a checkpoint has to treat "nothing written yet" as a
    /// state to keep waiting through, which the fallible reader above cannot
    /// express.
    pub(crate) fn snapshot_if_written(&self) -> Option<Value> {
        storage::parse_json(&fs::read(&self.session).ok()?).ok()
    }
    fn write_snapshot(&self, value: &Value) -> Result<()> {
        storage::atomic_write(
            &self.data,
            Path::new("session.json"),
            &serde_json::to_vec(value)?,
            0o600,
        )
    }
    fn count_shells(&self) -> Result<usize> {
        Ok(fs::read_to_string(&self.launched)?.lines().count())
    }
    pub fn probe(&self, program: &str, args: &[&str]) -> Result<String> {
        let path = self.base.join("probe.log");
        let file = File::create(&path)?;
        let mut process =
            OwnedProcess::spawn(Path::new(program), args, &self.env, &self.base, &file)?;
        let status = process.wait(Duration::from_secs(5))?;
        if !status.success() {
            let output = fs::read(&path)?;
            return Err(probe_failure(program, args, status, &output).into());
        }
        Ok(fs::read_to_string(path)?)
    }
    pub fn wait(
        &mut self,
        timeout: u64,
        mut predicate: impl FnMut(&mut Self) -> Result<bool>,
    ) -> Result<()> {
        let deadline = Instant::now() + Duration::from_secs(timeout);
        loop {
            if predicate(self)? {
                return Ok(());
            }
            if Instant::now() >= deadline {
                let mut details = BTreeMap::new();
                for (name, program, args) in [
                    (
                        "active",
                        "/usr/bin/xprop",
                        vec!["-root", "_NET_ACTIVE_WINDOW"],
                    ),
                    ("focus", "/usr/bin/xdotool", vec!["getwindowfocus"]),
                    (
                        "window",
                        "/usr/bin/xprop",
                        vec!["-id", self.window.as_str(), "_NET_WM_STATE", "WM_PROTOCOLS"],
                    ),
                ] {
                    details.insert(
                        name,
                        self.probe(program, &args)
                            .unwrap_or_else(|error| error.to_string()),
                    );
                }
                let _ = fs::write(
                    self.base.join("timeout-state.json"),
                    serde_json::to_vec_pretty(
                        &json!({"probes":details,"expected_window":self.window,"evidence_log":self.log}),
                    )?,
                );
                if !self.window.is_empty() {
                    let _ = self.screenshot("timeout-window.png");
                }
                return Err(
                    format!("session wait timed out; evidence: {}", self.base.display()).into(),
                );
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }
    pub fn xdo(&self, args: &[&str]) -> Result<()> {
        self.probe("/usr/bin/xdotool", args)?;
        Ok(())
    }
    pub fn key(&self, key: &str) -> Result<()> {
        // Fedora's XKB CTRL+ALT function-key type repeats F11 at several
        // levels. xdotool's symbolic lookup selects Alt+F11 even with
        // --clearmodifiers. Numeric keycodes bypass that lookup with modmask=0.
        // These harnesses use owned Xorg virtual PC displays; their FK11 is 95.
        // The caller checks actual GTK/WM fullscreen state; this is not a
        // general-purpose keymap-independent keyboard API.
        self.xdo(&key_arguments(key))?;
        std::thread::sleep(Duration::from_millis(200));
        Ok(())
    }
    pub(crate) fn move_mouse(&self, x: i64, y: i64) -> Result<()> {
        self.xdo(&[
            "mousemove",
            "--window",
            &self.window,
            &x.to_string(),
            &y.to_string(),
        ])
    }
    pub(crate) fn button(&mut self, name: &str) -> Result<()> {
        self.wait(15, |this| {
            let samples = rectangles(&this.text()?, name);
            Ok(samples.len() >= 2 && samples[samples.len() - 1] == samples[samples.len() - 2])
        })?;
        let bounds = rectangles(&self.text()?, name)
            .last()
            .copied()
            .ok_or("consent button bounds missing")?;
        self.move_mouse(
            (bounds[0] + bounds[2] / 2.0) as i64,
            (bounds[1] + bounds[3] / 2.0) as i64,
        )?;
        self.xdo(&["click", "1"])
    }
    pub fn stop(&mut self, crash: bool) -> Result<()> {
        if let Some(mut process) = self.process.take() {
            process.signal(if crash {
                Signal::SIGKILL
            } else {
                Signal::SIGTERM
            })?;
            process.wait(Duration::from_secs(10))?;
        }
        Ok(())
    }
    fn exited(&mut self) -> Result<bool> {
        Ok(!self
            .process
            .as_mut()
            .ok_or("app process missing")?
            .is_running()?)
    }
    pub fn start(&mut self, phase: &str) -> Result<u32> {
        self.stop(false)?;
        self.window.clear();
        self.log = self.base.join(format!("{phase}.log"));
        let log = File::create(&self.log)?;
        self.process = Some(OwnedProcess::spawn(
            Path::new("/usr/bin/dbus-run-session"),
            &["--", self.app.to_str().ok_or("app path is not UTF-8")?],
            &self.env,
            &self.base,
            &log,
        )?);
        self.wait(15, |this| Ok(this.text()?.contains("among[tick1]")))?;
        let parent = self.process.as_ref().ok_or("app process missing")?.id();
        let pid = owned_application_pid(parent)?;
        // `xdotool search` walks the whole window tree. A window destroyed
        // between listing and querying it (GTK creates and drops helper
        // windows while starting) makes the walk fail with BadWindow; that
        // says nothing about our window, so the search is repeated.
        let pid_text = pid.to_string();
        let mut windows = String::new();
        self.wait(15, |this| {
            match this.probe(
                "/usr/bin/xdotool",
                &["search", "--onlyvisible", "--pid", &pid_text],
            ) {
                Ok(found) => {
                    windows = found;
                    Ok(true)
                }
                Err(error) if transient_window_race(&error.to_string()) => Ok(false),
                Err(error) => Err(error),
            }
        })?;
        self.window = windows
            .split_whitespace()
            .last()
            .ok_or("native app window missing")?
            .to_string();
        self.xdo(&["windowactivate", "--sync", &self.window])?;
        self.xdo(&["windowfocus", "--sync", &self.window])?;
        // WM_PROTOCOLS belongs to the client; it can precede WM management.
        // Wait for the managed state, active window and focus before the first key;
        // never retry close or replace it with killing the owned application.
        self.wait(15, |this| {
            let protocols = this.probe("/usr/bin/xprop", &["-id", &this.window, "WM_PROTOCOLS"])?;
            let focus = this.probe("/usr/bin/xdotool", &["getwindowfocus"])?;
            let active = this.probe("/usr/bin/xdotool", &["getactivewindow"])?;
            let managed = this.probe("/usr/bin/xprop", &["-id", &this.window, "WM_STATE"])?;
            Ok(protocols.contains("WM_DELETE_WINDOW")
                && managed.contains("Normal")
                && active.trim() == this.window
                && focus.trim() == this.window)
        })?;
        Ok(pid)
    }
    pub(crate) fn ready(&mut self) -> Result<()> {
        self.wait(15, |this| {
            let text = this.text()?;
            Ok(text.contains("session_ready=true") && text.contains("has_focus=true"))
        })?;
        self.clean_log(&self.log)
    }
    pub fn clean_log(&self, log: &Path) -> Result<()> {
        let text = fs::read_to_string(log)?;
        for line in text.lines() {
            if let Some((_, value)) = line.split_once("toast_widgets=") {
                require(
                    value.split_whitespace().next() == Some("0"),
                    "the live window contains a toast widget",
                )?;
            }
        }
        require(
            ![
                "CRITICAL",
                "panicked",
                "stylesheet error",
                "Gdk:ERROR",
                "Bail out",
            ]
            .iter()
            .any(|error| text.contains(error)),
            "session log contains native critical/panic/style error",
        )
    }
    pub(crate) fn geometry(&self) -> Result<String> {
        self.text()?
            .lines()
            .rfind(|line| line.contains("tree_boxes="))
            .map(str::to_string)
            .ok_or_else(|| "missing tree geometry".into())
    }
    fn pane(&mut self, id: u64) -> Result<()> {
        self.wait(15, |this| {
            Ok(this
                .geometry()
                .is_ok_and(|geometry| !pane_rectangles(&geometry, &id.to_string()).is_empty()))
        })?;
        let geometry = self.geometry()?;
        let origin = pair(&geometry, "origin", ',')?;
        let bounds = pane_rectangles(&geometry, &id.to_string())
            .last()
            .copied()
            .ok_or("missing pane rectangle")?;
        self.move_mouse(
            origin[0] + (bounds[0] + bounds[2] / 2.0) as i64,
            origin[1] + (bounds[1] + bounds[3] / 2.0) as i64,
        )?;
        self.xdo(&["click", "1"])?;
        std::thread::sleep(Duration::from_millis(200));
        Ok(())
    }
    pub(crate) fn command(&self, command: &str) -> Result<()> {
        self.xdo(&["type", "--clearmodifiers", "--delay", "2", command])?;
        self.key("Return")
    }
    fn pwd(&mut self, id: u64, expected: &Path, phase: &str) -> Result<()> {
        self.pane(id)?;
        let output = self.base.join(format!("{phase}-{id}.pwd"));
        self.command(&format!("pwd > '{}'", output.display()))?;
        self.wait(15, |_| Ok(output.exists()))?;
        require(
            fs::read_to_string(output)?.trim()
                == expected.to_str().ok_or("directory is not UTF-8")?,
            "actual restored shell cwd differs",
        )
    }
    pub(crate) fn screenshot(&self, name: &str) -> Result<()> {
        self.probe(
            "/usr/bin/import",
            &[
                "-window",
                &self.window,
                self.base
                    .join(name)
                    .to_str()
                    .ok_or("screenshot path is not UTF-8")?,
            ],
        )?;
        Ok(())
    }
}

pub fn fresh(binary: &Path, scratch: &Path, prefix: &str) -> Result<Harness> {
    crate::preferences::owned_display()?;
    if nix::unistd::geteuid().is_root() {
        return Err("permission scenarios require non-root execution".into());
    }
    fs::create_dir_all(scratch)?;
    let base = crate::storage::private_directory(scratch, prefix)?;
    for name in [
        "home",
        "data",
        "data/among",
        "config",
        "config/among",
        "cache",
        "state",
    ] {
        fs::DirBuilder::new().mode(0o700).create(base.join(name))?;
    }
    fs::write(base.join("config/among/plugins.json"), b"{\"plugins\":[]}")?;
    let launched = base.join("shell-launches");
    let shell = base.join("shell");
    storage::atomic_write(
        &base,
        Path::new("shell"),
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$$\" >> '{}'\nexec /bin/bash --noprofile --norc\n",
            launched.display()
        )
        .as_bytes(),
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
    for (key, dir) in [
        ("HOME", "home"),
        ("XDG_DATA_HOME", "data"),
        ("XDG_CONFIG_HOME", "config"),
        ("XDG_CACHE_HOME", "cache"),
        ("XDG_STATE_HOME", "state"),
    ] {
        env.insert(key.into(), base.join(dir).into_os_string());
    }
    for (key, value) in [
        ("HISTFILE", "/dev/null"),
        ("G_DEBUG", "fatal-criticals"),
        ("GTK_A11Y", "none"),
        ("AMONG_SELFCHECK", "1"),
        ("AMONG_SELFCHECK_TICKS", "1000"),
        ("GDK_BACKEND", "x11"),
        ("GIO_USE_VFS", "local"),
        ("ADW_DISABLE_PORTAL", "1"),
    ] {
        env.insert(key.into(), value.into());
    }
    env.insert("SHELL".into(), shell.into_os_string());
    let data = base.join("data/among");
    Ok(Harness {
        base: base.clone(),
        session: data.join("session.json"),
        data,
        launched,
        app: launch_path(binary)?,
        env,
        process: None,
        log: base.join("initial.log"),
        window: String::new(),
    })
}

pub fn launch_path(binary: &Path) -> Result<PathBuf> {
    let resolved = binary.canonicalize()?;
    let metadata = resolved.metadata()?;
    if !metadata.is_file() || metadata.permissions().mode() & 0o111 == 0 {
        return Err("selected application must resolve to an executable regular file".into());
    }
    // snap's launcher dispatches by argv[0]; canonicalizing /snap/bin/among
    // into /usr/bin/snap invokes the package manager, not the application.
    Ok(if binary.is_absolute() {
        binary.to_path_buf()
    } else {
        std::env::current_dir()?.join(binary)
    })
}

pub fn run(binary: &Path, scratch: &Path) -> Result<PathBuf> {
    let mut h = fresh(binary, scratch, "among-sessions")?;
    let base = h.base.clone();
    let dirs = [
        base.join("one"),
        base.join("目录 with spaces"),
        base.join("three"),
        base.join("four"),
    ];
    for dir in &dirs {
        fs::create_dir(dir)?;
    }
    let large = 1_000_000_000_000u64;
    let mut fixture = json!({"version":1,"active":1,"workspaces":[
        {"name":"项目 alpha","colour":3,"focused":large,"layout":{"type":"split","orientation":"vertical","ratio":0.32,"first":{"type":"pane","id":large},"second":{"type":"split","orientation":"horizontal","ratio":0.67,"first":{"type":"pane","id":2},"second":{"type":"pane","id":3}}},"directories":[{"pane":large,"cwd":dirs[0]},{"pane":2,"cwd":dirs[1]},{"pane":3,"cwd":dirs[2]}]},
        {"name":"beta","colour":6,"focused":4,"layout":{"type":"pane","id":4},"directories":[{"pane":4,"cwd":dirs[3]}]}
    ]});
    h.write_snapshot(&fixture)?;
    let original = fs::read(&h.session)?;
    h.start("pending-close")?;
    h.wait(15, |this| {
        let text = this.text()?;
        Ok(text.contains("session_ready=false") && text.contains("Restore:"))
    })?;
    let before_close = h.probe(
        "/usr/bin/xprop",
        &[
            "-id",
            &h.window,
            "WM_STATE",
            "WM_PROTOCOLS",
            "_NET_WM_STATE",
        ],
    )?;
    fs::write(base.join("pending-close-before-key.log"), before_close)?;
    h.key("alt+F4")?;
    h.wait(15, Harness::exited)?;
    require(
        fs::read(&h.session)? == original && !h.launched.exists(),
        "pending close changed snapshot or launched shells",
    )?;
    let pid = h.start("restore")?;
    require(
        h.probe("/usr/bin/xprop", &["-id", &h.window, "_NET_WM_STATE"])?
            .contains("_NET_WM_STATE_MAXIMIZED_VERT"),
        "restore window is not maximized",
    )?;
    h.wait(15, |this| {
        let text = this.text()?;
        Ok(text.contains("session_ready=false") && text.contains("Restore:"))
    })?;
    std::thread::sleep(Duration::from_secs(3));
    require(
        fs::read(&h.session)? == original && !h.launched.exists(),
        "pending consent saved snapshot or started shells",
    )?;
    h.screenshot("prompt.png")?;
    let bus = fs::read(format!("/proc/{pid}/environ"))?
        .split(|byte| *byte == 0)
        .find_map(|row| {
            row.strip_prefix(b"DBUS_SESSION_BUS_ADDRESS=")
                .map(|value| value.to_vec())
        })
        .ok_or("private app bus not found")?;
    let mut singleton_env = h.env.clone();
    singleton_env.insert(
        "DBUS_SESSION_BUS_ADDRESS".into(),
        String::from_utf8(bus)?.into(),
    );
    let file = File::create(base.join("singleton.log"))?;
    let mut singleton = OwnedProcess::spawn(&h.app, &[], &singleton_env, &base, &file)?;
    require(
        singleton.wait(Duration::from_secs(10))?.success(),
        "singleton invocation failed",
    )?;
    require(
        !h.launched.exists() && fs::read(&h.session)? == original,
        "singleton invocation bypassed consent",
    )?;
    h.button("Restore")?;
    h.ready()?;
    h.wait(15, |this| {
        Ok(this.launched.exists() && this.count_shells()? == 4)
    })?;
    h.wait(15, |this| {
        Ok(this.snapshot()?["workspaces"][0]["layout"] == fixture["workspaces"][0]["layout"])
    })?;
    let focused = base.join("initial-focus.pwd");
    h.command(&format!("pwd > '{}'", focused.display()))?;
    h.wait(15, |_| Ok(focused.exists()))?;
    require(
        fs::read_to_string(focused)?.trim() == dirs[3].to_str().ok_or("cwd is not UTF-8")?,
        "initial active workspace focus differs",
    )?;
    h.pwd(4, &dirs[3], "restored")?;
    let background = base.join("background-alive");
    h.command(&format!(
        "sleep 1; printf background > '{}'",
        background.display()
    ))?;
    h.key("ctrl+Page_Up")?;
    h.wait(15, |_| Ok(background.exists()))?;
    h.wait(15, |this| {
        Ok(this.text()?.contains("focused=PaneId(1000000000000)"))
    })?;
    for (id, dir) in [large, 2, 3].into_iter().zip(&dirs) {
        h.pwd(id, dir, "restored")?;
    }
    h.wait(15, |this| {
        let s = this.snapshot()?;
        Ok(s["active"] == 0 && s["workspaces"][0]["focused"] == 3)
    })?;
    require(h.count_shells()? == 4, "restore restarted extra shells")?;
    let geometry = h.geometry()?;
    let area = pair(&geometry, "area", 'x')?;
    let first = pane_rectangles(&geometry, &large.to_string())
        .last()
        .copied()
        .ok_or("large pane geometry missing")?;
    let second = pane_rectangles(&geometry, "2")
        .last()
        .copied()
        .ok_or("second pane geometry missing")?;
    require(
        (first[2] / area[0] as f64 - 0.32).abs() < 0.01
            && (second[3] / area[1] as f64 - 0.67).abs() < 0.01,
        "restored native divider ratios differ",
    )?;
    h.screenshot("restored.png")?;
    let origin = pair(&geometry, "origin", ',')?;
    let offset = h.text()?.len();
    let sx = origin[0] + (second[0] + second[2] / 2.0) as i64;
    let sy = origin[1] + second[1] as i64 + 12;
    h.move_mouse(sx, sy)?;
    h.xdo(&["mousedown", "1"])?;
    std::thread::sleep(Duration::from_millis(200));
    for (x, y) in [
        (sx - 30, sy),
        (
            origin[0] + (first[0] + first[2] / 2.0) as i64,
            origin[1] + (first[1] + first[3] / 2.0) as i64,
        ),
        (
            origin[0] + first[0] as i64 + 8,
            origin[1] + (first[1] + first[3] / 2.0) as i64,
        ),
    ] {
        h.move_mouse(x, y)?;
        std::thread::sleep(Duration::from_millis(300));
    }
    h.xdo(&["mouseup", "1"])?;
    h.wait(15, |this| {
        Ok(this
            .text()?
            .get(offset..)
            .is_some_and(|text| text.contains("tree_boxes=[2:0+")))
    })?;
    require(h.count_shells()? == 4, "pane drag restarted shells")?;
    h.pwd(2, &dirs[1], "moved")?;
    h.pwd(large, &dirs[0], "moved")?;
    h.key("ctrl+shift+n")?;
    h.wait(15, |this| {
        Ok(this.snapshot()?["workspaces"][0]["directories"]
            .as_array()
            .is_some_and(|entries| entries.len() == 4))
    })?;
    let s = h.snapshot()?;
    require(
        s["workspaces"][0]["directories"]
            .as_array()
            .ok_or("directories missing")?
            .iter()
            .filter_map(|entry| entry["pane"].as_u64())
            .max()
            == Some(large + 1),
        "new pane did not allocate above sparse IDs",
    )?;
    h.wait(15, |this| {
        let g = this.geometry()?;
        Ok(rectangles(
            g.split_once("name_rects=[")
                .map(|(_, tail)| tail)
                .unwrap_or(""),
            "0",
        )
        .first()
        .is_some_and(|bounds| bounds[2] > 0.0 && bounds[3] > 0.0))
    })?;
    let geometry = h.geometry()?;
    let sidebar = pair(&geometry, "sidebar_origin", ',')?;
    let name = rectangles(
        geometry
            .split_once("name_rects=[")
            .ok_or("name rectangles missing")?
            .1,
        "0",
    )
    .first()
    .copied()
    .ok_or("workspace name rect missing")?;
    h.move_mouse(
        sidebar[0] + (name[0] + name[2] / 2.0) as i64,
        sidebar[1] + (name[1] + name[3] / 2.0) as i64,
    )?;
    h.xdo(&["click", "--repeat", "2", "--delay", "80", "1"])?;
    std::thread::sleep(Duration::from_millis(150));
    h.key("ctrl+a")?;
    h.xdo(&["type", "--clearmodifiers", "saved alpha"])?;
    let changed = Instant::now();
    h.key("Return")?;
    h.wait(3, |this| {
        Ok(this.snapshot()?["workspaces"][0]["name"] == "saved alpha")
    })?;
    require(
        changed.elapsed() < Duration::from_secs(3),
        "workspace name checkpoint exceeded bound",
    )?;
    h.wait(15, |this| {
        Ok(this.text()?.contains("names=[saved alpha|beta]"))
    })?;
    let geometry = h.geometry()?;
    let origin = pair(&geometry, "origin", ',')?;
    let area = pair(&geometry, "area", 'x')?;
    let third = pane_rectangles(&geometry, "3")
        .last()
        .copied()
        .ok_or("third pane geometry missing")?;
    let divider = third[0] as i64 - 1;
    let before = h.snapshot()?["workspaces"][0]["layout"]["ratio"]
        .as_f64()
        .ok_or("divider ratio missing")?;
    h.move_mouse(origin[0] + divider + 1, origin[1] + area[1] / 2)?;
    h.xdo(&["mousedown", "1"])?;
    h.move_mouse(origin[0] + divider + 81, origin[1] + area[1] / 2)?;
    h.xdo(&["mouseup", "1"])?;
    let changed = Instant::now();
    h.wait(3, |this| {
        Ok((this.snapshot()?["workspaces"][0]["layout"]["ratio"]
            .as_f64()
            .ok_or("ratio missing")?
            - before)
            .abs()
            > 0.02)
    })?;
    require(
        changed.elapsed() < Duration::from_secs(3),
        "divider checkpoint exceeded bound",
    )?;
    h.key("ctrl+Page_Down")?;
    h.pane(4)?;
    h.command("export PROMPT_COMMAND='printf \"\\033]7;file://localhost%s\\007\" \"$PWD\"'")?;
    h.command(&format!("sleep 1; cd '{}'", dirs[0].display()))?;
    let changed = Instant::now();
    h.key("ctrl+Page_Up")?;
    h.wait(4, |this| {
        Ok(this.snapshot()?["workspaces"][1]["directories"][0]["cwd"] == json!(dirs[0]))
    })?;
    require(
        changed.elapsed() < Duration::from_secs(4),
        "background cwd checkpoint exceeded bound",
    )?;
    require(
        h.snapshot()?["workspaces"][0]["directories"][0]["cwd"] == json!(dirs[1]),
        "pane move lost actual directory",
    )?;
    h.key("alt+F4")?;
    h.button("Cancel")?;
    h.ready()?;
    let alive = base.join("cancel-alive");
    h.command(&format!("printf alive > '{}'", alive.display()))?;
    h.wait(15, |_| Ok(alive.exists()))?;
    h.key("ctrl+shift+t")?;
    h.key("alt+F4")?;
    h.button("Exit")?;
    h.wait(15, Harness::exited)?;
    require(
        h.snapshot()?["workspaces"]
            .as_array()
            .is_some_and(|entries| entries.len() == 3),
        "final save lost immediately created workspace",
    )?;
    println!("PASS restoration: consent, singleton, names/layout/cwd, sparse IDs, cancel and normal exit");
    h.start("crash-restored")?;
    h.button("Restore")?;
    h.ready()?;
    h.wait(15, |this| Ok(this.text()?.contains("session_saved=true")))?;
    let stable = fs::metadata(&h.session)?.modified()?;
    std::thread::sleep(Duration::from_secs(3));
    require(
        fs::metadata(&h.session)?.modified()? == stable,
        "unchanged session rewrote checkpoint",
    )?;
    let before_crash = h.snapshot()?;
    h.command("exec sleep 120")?;
    h.stop(true)?;
    h.start("after-crash")?;
    h.button("Restore")?;
    h.ready()?;
    h.wait(15, |this| Ok(this.text()?.contains("session_saved=true")))?;
    require(
        h.snapshot()? == before_crash,
        "fresh crash restoration changed checkpoint",
    )?;
    let alive = base.join("fresh-shell-after-crash");
    h.command(&format!("printf fresh > '{}'", alive.display()))?;
    h.wait(15, |_| Ok(alive.exists()))?;
    h.stop(false)?;
    println!("PASS fresh-process crash checkpoint restoration");
    h.start("decline")?;
    h.key("Escape")?;
    h.ready()?;
    h.wait(15, |this| {
        let s = this.snapshot()?;
        Ok(s["workspaces"]
            .as_array()
            .is_some_and(|rows| rows.len() == 1)
            && s["workspaces"][0]["name"] == "project")
    })?;
    require(
        h.snapshot()?["workspaces"][0]["directories"]
            .as_array()
            .is_some_and(|rows| rows.len() == 1),
        "decline did not create one fresh pane",
    )?;
    h.key("ctrl+shift+w")?;
    h.button("Exit")?;
    h.wait(15, Harness::exited)?;
    let s = h.snapshot()?;
    require(
        s["workspaces"]
            .as_array()
            .is_some_and(|rows| rows.len() == 1)
            && s["workspaces"][0]["directories"]
                .as_array()
                .is_some_and(|rows| rows.len() == 1),
        "decline exit changed fresh session",
    )?;
    println!("PASS Escape/Start new replaces old session");
    for (phase, name) in [
        ("missing-directory", "deleted"),
        ("inaccessible-directory", "inaccessible"),
    ] {
        let unavailable = base.join(name);
        if name == "inaccessible" {
            fs::create_dir(&unavailable)?;
            fs::set_permissions(&unavailable, fs::Permissions::from_mode(0o0))?;
        }
        fixture["workspaces"][0]["directories"][0]["cwd"] = json!(unavailable);
        h.write_snapshot(&fixture)?;
        h.start(phase)?;
        h.button("Restore")?;
        h.ready()?;
        h.key("ctrl+Page_Up")?;
        h.pwd(large, &base.join("home"), phase)?;
        require(
            h.text()?.contains("the requested directory is unavailable"),
            "directory fallback warning missing",
        )?;
        h.stop(false)?;
        if name == "inaccessible" {
            fs::set_permissions(unavailable, fs::Permissions::from_mode(0o700))?;
        }
        println!("PASS {phase}: actual shell cwd fallback");
    }
    fs::remove_file(&h.session)?;
    h.start("storage-failure")?;
    h.ready()?;
    h.wait(15, |this| {
        Ok(this.session.exists() && this.text()?.contains("session_saved=true"))
    })?;
    let previous = fs::read(&h.session)?;
    fs::set_permissions(&h.data, fs::Permissions::from_mode(0o500))?;
    h.key("ctrl+shift+n")?;
    h.wait(15, |this| Ok(this.text()?.contains("Permission denied")))?;
    std::thread::sleep(Duration::from_millis(100));
    h.screenshot("storage-warning.png")?;
    let alive = base.join("storage-failure-input");
    h.command(&format!("printf alive > '{}'", alive.display()))?;
    h.wait(15, |_| Ok(alive.exists()))?;
    require(
        fs::read(&h.session)? == previous,
        "failed checkpoint corrupted snapshot",
    )?;
    h.key("alt+F4")?;
    h.button("Exit")?;
    h.button("Keep open")?;
    let alive = base.join("input-after-failed-exit");
    h.command(&format!("printf alive > '{}'", alive.display()))?;
    h.wait(15, |_| Ok(alive.exists()))?;
    fs::set_permissions(&h.data, fs::Permissions::from_mode(0o700))?;
    h.wait(15, |this| {
        Ok(this.snapshot()?["workspaces"][0]["directories"]
            .as_array()
            .is_some_and(|rows| rows.len() == 2))
    })?;
    h.stop(false)?;
    println!(
        "PASS failed checkpoint preserves snapshot/input and retries after permission recovery"
    );
    for (phase, content) in [("malformed", "broken"), ("future", "{\"version\":99}")] {
        fs::write(&h.session, content)?;
        h.start(phase)?;
        h.ready()?;
        std::thread::sleep(Duration::from_secs(3));
        require(
            fs::read_to_string(&h.session)? == content,
            "invalid session was overwritten",
        )?;
        require(
            !h.text()?.contains("session_problem=\"\""),
            "invalid session warning missing",
        )?;
        fs::remove_file(&h.session)?;
        h.wait(15, |this| {
            Ok(this.session.exists() && this.snapshot()?["version"] == 1)
        })?;
        h.stop(false)?;
        println!("PASS {phase}: preservation and recovery after removal");
    }
    for entry in fs::read_dir(&base)? {
        let path = entry?.path();
        if path.extension().is_some_and(|ext| ext == "log")
            && path.file_name() != Some(std::ffi::OsStr::new("probe.log"))
        {
            h.clean_log(&path)?;
        }
    }
    println!("Evidence: {}", base.display());
    Ok(base)
}

#[cfg(test)]
mod launcher_tests {
    use super::*;

    /// The message is the one a failed Snap run printed while the app was
    /// starting; only that race is retried, a search that finds nothing is not.
    #[test]
    fn only_a_window_vanishing_during_the_search_is_retried() {
        let race = "session probe failed: /usr/bin/xdotool [\"search\", \"--onlyvisible\", \"--pid\", \"18464\"] ended with exit status: 1; output:\nX Error of failed request:  BadWindow (invalid Window parameter)\n  Major opcode of failed request:  15 (X_QueryTree)";
        assert!(transient_window_race(race));
        // Observed on the native arm64 Checks runner: the window vanished
        // between listing and the visibility query.
        let visibility = "session probe failed: /usr/bin/xdotool [\"search\", \"--onlyvisible\", \"--pid\", \"21834\"] ended with exit status: 1; output:\nX Error of failed request:  BadWindow (invalid Window parameter)\n  Major opcode of failed request:  3 (X_GetWindowAttributes)";
        assert!(transient_window_race(visibility));
        assert!(!transient_window_race(
            "session probe failed: /usr/bin/xdotool [\"search\", \"--onlyvisible\", \"--pid\", \"18464\"] ended with exit status: 1; output:"
        ));
        assert!(!transient_window_race(
            "X Error of failed request:  BadWindow (invalid Window parameter)\n  Major opcode of failed request:  20 (X_GetProperty)"
        ));
    }

    #[test]
    fn only_unmodified_f11_bypasses_symbolic_modifier_lookup() {
        assert_eq!(
            key_arguments("F11"),
            ["key", "--clearmodifiers", "--delay", "100", "95"]
        );
        for key in ["Return", "Escape", "alt+F4", "ctrl+F11"] {
            assert_eq!(
                key_arguments(key),
                ["key", "--clearmodifiers", "--delay", "100", key]
            );
        }
    }

    #[test]
    fn a_failed_probe_names_the_helper_its_exit_and_the_end_of_its_output() {
        use std::os::unix::process::ExitStatusExt;
        // The CI log is the only record of a failed helper; the message must
        // say which call failed and keep the last lines, where errors are.
        let mut output = "early line\n".repeat(400).into_bytes();
        output.extend_from_slice("ошибка: cannot open display\n".as_bytes());
        let message = probe_failure(
            "/usr/bin/xdotool",
            &["key", "shift+F10"],
            std::process::ExitStatus::from_raw(1 << 8),
            &output,
        );
        assert!(message.starts_with("session probe failed: /usr/bin/xdotool [\"key\", \"shift+F10\"] ended with exit status: 1;"), "{message}");
        assert!(
            message.ends_with("ошибка: cannot open display"),
            "{message}"
        );
        assert!(message.contains("bytes omitted"), "{message}");
        assert!(
            message.len() < PROBE_OUTPUT_LIMIT + 200,
            "{}",
            message.len()
        );
    }

    #[test]
    fn a_probe_cut_inside_a_character_keeps_the_text_valid() {
        use std::os::unix::process::ExitStatusExt;
        // Three-byte characters and a limit that is not a multiple of three
        // put the cut inside a character.
        assert_ne!(PROBE_OUTPUT_LIMIT % 3, 0);
        let output = "€".repeat(PROBE_OUTPUT_LIMIT);
        let message = probe_failure(
            "/usr/bin/import",
            &[],
            std::process::ExitStatus::from_raw(1 << 8),
            output.as_bytes(),
        );
        assert!(!message.contains('\u{FFFD}'), "{message}");
        assert!(message.ends_with('€'));
    }

    #[test]
    fn a_short_probe_output_is_quoted_whole() {
        use std::os::unix::process::ExitStatusExt;
        let message = probe_failure(
            "/usr/bin/import",
            &["-window", "root"],
            std::process::ExitStatus::from_raw(9),
            b"import: unable to grab\n",
        );
        assert!(!message.contains("omitted"), "{message}");
        assert!(message.contains("signal: 9"), "{message}");
        assert!(message.ends_with("import: unable to grab"), "{message}");
    }

    #[test]
    fn process_discovery_handles_nested_launchers_and_rejects_ambiguous_apps() -> Result<()> {
        for nested in [false, true] {
            let tree = BTreeMap::from([
                (1, ("dbus-run-session".into(), vec![2])),
                (
                    2,
                    (
                        if nested { "AppImage" } else { "among" }.into(),
                        if nested { vec![3] } else { vec![] },
                    ),
                ),
                (3, ("among".into(), vec![])),
            ]);
            assert_eq!(
                find_application_pid(1, |pid| tree
                    .get(&pid)
                    .cloned()
                    .ok_or("unknown fixture PID".into()))?,
                if nested { 3 } else { 2 }
            );
        }
        assert!(find_application_pid(1, |_| Ok(("dbus-run-session".into(), vec![]))).is_err());
        assert!(find_application_pid(1, |pid| Ok((
            "among".into(),
            if pid == 1 { vec![2, 3] } else { vec![] }
        )))
        .is_err());
        Ok(())
    }
}
