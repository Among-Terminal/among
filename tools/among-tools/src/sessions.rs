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
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

fn require(condition: bool, message: &str) -> Result<()> {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
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

pub(crate) struct Harness {
    pub(crate) base: PathBuf,
    data: PathBuf,
    session: PathBuf,
    launched: PathBuf,
    pub(crate) app: PathBuf,
    pub(crate) env: BTreeMap<OsString, OsString>,
    pub(crate) process: Option<OwnedProcess>,
    pub(crate) log: PathBuf,
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
    pub(crate) fn text(&self) -> Result<String> {
        Ok(fs::read_to_string(&self.log)?)
    }
    fn snapshot(&self) -> Result<Value> {
        storage::parse_json(&fs::read(&self.session)?)
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
    pub(crate) fn probe(&self, program: &str, args: &[&str]) -> Result<String> {
        let path = self.base.join("probe.log");
        let file = File::create(&path)?;
        let mut process =
            OwnedProcess::spawn(Path::new(program), args, &self.env, &self.base, &file)?;
        require(
            process.wait(Duration::from_secs(5))?.success(),
            "session probe failed",
        )?;
        Ok(fs::read_to_string(path)?)
    }
    pub(crate) fn wait(
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
                fs::write(
                    self.base.join("timeout-state.json"),
                    serde_json::to_vec_pretty(
                        &json!({"probes":details,"expected_window":self.window,"evidence_log":self.log}),
                    )?,
                )?;
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
    pub(crate) fn xdo(&self, args: &[&str]) -> Result<()> {
        self.probe("/usr/bin/xdotool", args)?;
        Ok(())
    }
    pub(crate) fn key(&self, key: &str) -> Result<()> {
        self.xdo(&["key", "--clearmodifiers", key])?;
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
    fn button(&mut self, name: &str) -> Result<()> {
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
    pub(crate) fn stop(&mut self, crash: bool) -> Result<()> {
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
    pub(crate) fn start(&mut self, phase: &str) -> Result<u32> {
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
        let parent = self
            .process
            .as_ref()
            .ok_or("app process missing")?
            .id()
            .to_string();
        let output = self.probe("/usr/bin/pgrep", &["-P", &parent, "-x", "among"])?;
        let pid = output
            .split_whitespace()
            .next()
            .ok_or("native app PID missing")?
            .parse::<u32>()?;
        let windows = self.probe(
            "/usr/bin/xdotool",
            &["search", "--onlyvisible", "--pid", &pid.to_string()],
        )?;
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
    fn ready(&mut self) -> Result<()> {
        self.wait(15, |this| {
            let text = this.text()?;
            Ok(text.contains("session_ready=true") && text.contains("has_focus=true"))
        })?;
        self.clean_log(&self.log)
    }
    pub(crate) fn clean_log(&self, log: &Path) -> Result<()> {
        let text = fs::read_to_string(log)?;
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

pub(crate) fn fresh(binary: &Path, scratch: &Path, prefix: &str) -> Result<Harness> {
    crate::preferences::owned_display()?;
    if nix::unistd::geteuid().is_root() {
        return Err("permission scenarios require non-root execution".into());
    }
    fs::create_dir_all(scratch)?;
    let base = scratch.join(format!(
        "{prefix}-{}-{}",
        std::process::id(),
        SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos()
    ));
    fs::DirBuilder::new().mode(0o700).create(&base)?;
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
            h.text()?.contains("saved directory is unavailable"),
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
