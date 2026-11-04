//! the hostile fixture, run twice per action on the real kernel: once without
//! confinement (the positive control, which must succeed in this very
//! environment) and once through the same launch path every managed plugin
//! uses. a denial counts as evidence only when its control succeeded.

use super::data_dir;
use super::launch::{Applied, Confinement, Spec};
use super::level;
use crate::plugin::process::confined_program;
use std::collections::BTreeSet;
use std::fs::File;
use std::io::{BufRead, BufReader, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

pub(super) struct Scratch {
    pub root: PathBuf,
    pub generation: PathBuf,
    pub data_home: PathBuf,
    pub home: PathBuf,
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn private(path: &Path) {
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
}

pub(super) fn fixture_executable() -> PathBuf {
    let path = std::env::current_exe()
        .unwrap()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("examples/sandbox-hostile");
    assert!(
        path.is_file(),
        "build the sandbox-hostile example first: cargo build --locked --examples"
    );
    path
}

impl Scratch {
    pub fn new(name: &str) -> Self {
        let base = std::env::var_os("AMONG_CHECK_ROOT")
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir);
        let root = base.join(format!("among-hostile-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        private(&root);
        let scratch = Self {
            generation: root.join("generation"),
            data_home: root.join("data-home"),
            home: root.join("home"),
            root,
        };
        for directory in [&scratch.generation, &scratch.data_home, &scratch.home] {
            std::fs::create_dir(directory).unwrap();
            private(directory);
        }
        std::fs::copy(fixture_executable(), scratch.generation.join("plugin")).unwrap();
        private(&scratch.generation.join("plugin"));
        // a user file the plugin can read with confinement off and must not
        // reach with it on.
        std::fs::write(scratch.home.join("secret.txt"), b"token").unwrap();
        // a readable but not writable file of the installed package, for
        // truncation (only the Landlock ABI that mediates it protects it).
        std::fs::write(scratch.generation.join("readme"), b"package file").unwrap();
        scratch
    }

    pub fn data_path(&self) -> PathBuf {
        self.data_home.join("among/plugin-data/12/hostile")
    }
}

/// What the harness learned from one run of one action.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Outcome {
    /// The fixture answered with this text (after the action name).
    Answered(String),
    /// The process died: the signal number, or none for a plain exit.
    Died(Option<i32>),
}

impl Outcome {
    pub fn text(&self) -> String {
        match self {
            Self::Answered(text) => text.clone(),
            Self::Died(Some(signal)) => format!("died:signal {signal}"),
            Self::Died(None) => "died:exit".into(),
        }
    }
    pub fn succeeded(&self) -> bool {
        matches!(self, Self::Answered(text) if text.starts_with("ok"))
    }
    pub fn failed_with(&self, errno: &str) -> bool {
        matches!(self, Self::Answered(text) if *text == format!("err:{errno}"))
    }
}

pub(super) struct Session {
    pub child: Child,
    stdin: Option<ChildStdin>,
    stdout: BufReader<ChildStdout>,
    pub applied: Option<Applied>,
}

impl Session {
    pub fn ask(&mut self, line: &str) -> Outcome {
        let written = self
            .stdin
            .as_mut()
            .map(|stdin| writeln!(stdin, "{line}").and_then(|()| stdin.flush()));
        if written.is_none_or(|result| result.is_err()) {
            return self.death();
        }
        let mut ready = libc::pollfd {
            fd: self.stdout.get_ref().as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        // a fixture that neither answers nor dies is killed, not waited for.
        if unsafe { libc::poll(&mut ready, 1, 20_000) } == 0 {
            let _ = self.child.kill();
            return self.death();
        }
        let mut answer = String::new();
        match self.stdout.read_line(&mut answer) {
            Ok(0) | Err(_) => self.death(),
            Ok(_) => {
                let answer = answer.trim_end();
                let action = line.split_whitespace().next().unwrap_or("");
                Outcome::Answered(
                    answer
                        .strip_prefix(&format!("{action} "))
                        .unwrap_or(answer)
                        .to_string(),
                )
            }
        }
    }

    fn death(&mut self) -> Outcome {
        self.stdin = None;
        let status = self.child.wait().unwrap();
        Outcome::Died(status.signal())
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// How the fixture is launched.
pub(super) enum Mode<'a> {
    /// No confinement: the positive control.
    Control,
    /// The launch path of every managed plugin.
    Confined { features: &'a BTreeSet<String> },
}

pub(super) fn start(scratch: &Scratch, mode: Mode<'_>) -> Session {
    let base_environment = |command: &mut Command| {
        command
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("AMONG_PLUGIN_DATA_DIR", scratch.data_path());
    };
    match mode {
        Mode::Control => {
            let data = data_dir::open(&scratch.data_home, "12", "hostile").unwrap();
            let mut command = Command::new(scratch.generation.join("plugin"));
            command.current_dir(&scratch.generation).process_group(0);
            base_environment(&mut command);
            command
                .env("AMONG_PLUGIN_DATA_DIR", &data.path)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::null());
            let mut child = command.spawn().unwrap();
            Session {
                stdin: child.stdin.take(),
                stdout: BufReader::new(child.stdout.take().unwrap()),
                child,
                applied: None,
            }
        }
        Mode::Confined { features } => {
            let probe = level::probe();
            let required = probe.best();
            let generation = File::open(&scratch.generation).unwrap();
            let executable = File::open(scratch.generation.join("plugin")).unwrap();
            let data = data_dir::open(&scratch.data_home, "12", "hostile").unwrap();
            let spec = Spec {
                generation: &generation,
                data_dir: &data.directory,
                features,
                probe,
                required,
            };
            let mut confinement = Confinement::prepare(&spec).unwrap();
            let mut command = confined_program(generation, executable, None, &mut confinement);
            base_environment(&mut command);
            command
                .env("AMONG_PLUGIN_DATA_DIR", &data.path)
                .process_group(0)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::null());
            let (mut child, applied) = match confinement.spawn(&mut command) {
                Ok(started) => started,
                Err(error) => panic!(
                    "confined start failed: {}",
                    match error {
                        super::launch::StartError::Failed { reason, .. } => reason,
                        super::launch::StartError::Unproven { reason, .. } => reason,
                    }
                ),
            };
            Session {
                stdin: child.stdin.take(),
                stdout: BufReader::new(child.stdout.take().unwrap()),
                child,
                applied: Some(applied),
            }
        }
    }
}

/// Whether this kernel offers the full level; otherwise the confined half of
/// every test cannot be judged and says so.
pub(super) fn full_protection_available() -> bool {
    let best = level::probe().best();
    if best.is_full() {
        return true;
    }
    assert!(
        std::env::var_os("AMONG_REQUIRE_SANDBOX").is_none(),
        "AMONG_REQUIRE_SANDBOX is set but this kernel offers only {}",
        best.label()
    );
    eprintln!(
        "NOT VERIFIED: this kernel offers only {}; confined behaviour was not exercised",
        best.label()
    );
    false
}

/// A process that stands in for "another process of the same user".
pub(super) struct Victim(pub Child);

impl Victim {
    pub fn new() -> Self {
        let child = Command::new("/usr/bin/sleep")
            .arg("120")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(0)
            .spawn()
            .unwrap();
        Self(child)
    }
    pub fn pid(&self) -> u32 {
        self.0.id()
    }
    /// Whether the process still runs (a zombie counts as dead).
    pub fn alive(&mut self) -> bool {
        self.0.try_wait().unwrap().is_none()
    }
    pub fn limits(&self) -> String {
        std::fs::read_to_string(format!("/proc/{}/limits", self.pid())).unwrap()
    }
}

impl Drop for Victim {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn architecture() -> &'static str {
    std::env::consts::ARCH
}

/// The row of the evidence table: action, what the control did, what the
/// confined run did, and the verdict.
#[derive(Clone)]
pub(super) struct Row {
    pub action: String,
    pub control: String,
    pub confined: String,
    pub verdict: &'static str,
}

thread_local! {
    // each test runs on its own thread, so concurrent tests never mix rows.
    static ROWS: std::cell::RefCell<Vec<Row>> = const { std::cell::RefCell::new(Vec::new()) };
}

pub(super) fn record(action: &str, control: &Outcome, confined: &Outcome, verdict: &'static str) {
    ROWS.with_borrow_mut(|rows| {
        rows.push(Row {
            action: action.into(),
            control: control.text(),
            confined: confined.text(),
            verdict,
        })
    });
}

/// Writes the collected rows when AMONG_HOSTILE_REPORT names a directory.
pub(super) fn write_report(target: &str) {
    let Some(directory) = std::env::var_os("AMONG_HOSTILE_REPORT") else {
        return;
    };
    let kernel = std::fs::read_to_string("/proc/sys/kernel/osrelease").unwrap_or_default();
    let probe = level::probe();
    let mut text = format!(
        "# Hostile actions: {target}\n\n- kernel: {}\n- architecture: {}\n- Landlock ABI: {}\n- seccomp: {}\n- outer confinement: {}\n\n| action | control (no confinement) | under confinement | verdict |\n|---|---|---|---|\n",
        kernel.trim(),
        architecture(),
        probe.landlock_abi.map_or("none".into(), |abi| abi.to_string()),
        if probe.seccomp { "available" } else { "unavailable" },
        std::env::var("AMONG_OUTER_CONFINEMENT").unwrap_or_else(|_| "none declared".into()),
    );
    let rows = ROWS.with_borrow_mut(std::mem::take);
    for row in rows.iter() {
        text.push_str(&format!(
            "| {} | {} | {} | {} |\n",
            row.action, row.control, row.confined, row.verdict
        ));
    }
    let path = Path::new(&directory).join(format!("{target}.md"));
    std::fs::create_dir_all(&directory).unwrap();
    std::fs::write(path, text).unwrap();
}
