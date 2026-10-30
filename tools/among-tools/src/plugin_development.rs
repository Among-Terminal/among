//! Check that plugin development mode belongs to the primary instance only.
//!
//! GApplication uniqueness is decided on the session bus, so every launch here
//! shares the one bus of the surrounding `ci-x11` run instead of getting its
//! own: that is what makes the second process a secondary. The mode is read
//! from the primary's self-check report, which prints the value the process
//! fixed at startup.

use crate::{process::OwnedProcess, storage, Result};
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs::{self, File};
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Printed by a secondary process that asked for development mode; the same
/// text as `plugin::development::SECONDARY_REFUSAL` in the application.
const REFUSAL: &str =
    "among: --plugin-development applies only when it starts the first Among instance";

/// Exit code of that refused secondary process.
const REFUSED: i32 = 2;

struct Runner {
    base: PathBuf,
    app: PathBuf,
    env: BTreeMap<OsString, OsString>,
    count: usize,
}

struct Launch {
    process: OwnedProcess,
    log: PathBuf,
}

impl Launch {
    fn text(&self) -> String {
        fs::read_to_string(&self.log).unwrap_or_default()
    }

    /// The last mode the process reported, if it has reported one.
    fn mode(&self) -> Option<bool> {
        let text = self.text();
        let (_, value) = text.rsplit_once("plugin_development=")?;
        match value.split_whitespace().next()? {
            "true" => Some(true),
            "false" => Some(false),
            _ => None,
        }
    }

    fn reports(&self) -> usize {
        self.text().matches("plugin_development=").count()
    }
}

impl Runner {
    fn new(binary: &Path) -> Result<Self> {
        crate::preferences::owned_display()?;
        let caller: BTreeMap<OsString, OsString> = std::env::vars_os().collect();
        if !caller.contains_key(&OsString::from("DBUS_SESSION_BUS_ADDRESS")) {
            return Err("plugin development check needs the shared session bus of ci-x11".into());
        }
        let scratch = crate::scratch::root()?;
        let base = storage::private_directory(&scratch, "plugin-development")?;
        for name in ["home", "data", "config", "config/among", "cache", "state"] {
            fs::DirBuilder::new().mode(0o700).create(base.join(name))?;
        }
        fs::write(base.join("config/among/plugins.json"), b"{\"plugins\":[]}")?;
        let mut env = crate::process::allowed_environment(
            &caller,
            &[
                "PATH",
                "LANG",
                "DISPLAY",
                "XDG_RUNTIME_DIR",
                "DBUS_SESSION_BUS_ADDRESS",
                "GSK_RENDERER",
                "LIBGL_ALWAYS_SOFTWARE",
            ],
        );
        for (key, directory) in [
            ("HOME", "home"),
            ("XDG_DATA_HOME", "data"),
            ("XDG_CONFIG_HOME", "config"),
            ("XDG_CACHE_HOME", "cache"),
            ("XDG_STATE_HOME", "state"),
        ] {
            env.insert(key.into(), base.join(directory).into_os_string());
        }
        for (key, value) in [
            ("HISTFILE", "/dev/null"),
            ("AMONG_SELFCHECK", "1"),
            ("AMONG_SELFCHECK_TICKS", "1000"),
            ("GDK_BACKEND", "x11"),
            ("GIO_USE_VFS", "local"),
            ("ADW_DISABLE_PORTAL", "1"),
            ("GTK_A11Y", "none"),
        ] {
            env.insert(key.into(), value.into());
        }
        Ok(Self {
            base,
            app: crate::sessions::launch_path(binary)?,
            env,
            count: 0,
        })
    }

    fn launch(&mut self, phase: &str, development: bool) -> Result<Launch> {
        self.count += 1;
        let log = self.base.join(format!("{:02}-{phase}.log", self.count));
        let file = File::create(&log)?;
        let args: &[&str] = if development {
            &["--plugin-development"]
        } else {
            &[]
        };
        let process = OwnedProcess::spawn(&self.app, args, &self.env, &self.base, &file)?;
        Ok(Launch { process, log })
    }

    /// Start a process that must become the primary and report `expected`.
    fn primary(&mut self, phase: &str, development: bool) -> Result<Launch> {
        let launch = self.launch(phase, development)?;
        wait(&launch, phase, 20, |launch| Ok(launch.mode().is_some()))?;
        if launch.mode() != Some(development) {
            return Err(format!("{phase}: primary reported {:?}", launch.mode()).into());
        }
        Ok(launch)
    }
}

fn wait(
    launch: &Launch,
    phase: &str,
    seconds: u64,
    mut ready: impl FnMut(&Launch) -> Result<bool>,
) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(seconds);
    while Instant::now() < deadline {
        if ready(launch)? {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Err(format!("{phase}: timed out; log {}", launch.log.display()).into())
}

/// Wait for a process that must end by itself and return its exit code.
fn exit_code(launch: &mut Launch, phase: &str) -> Result<i32> {
    let status = launch
        .process
        .wait(Duration::from_secs(20))
        .map_err(|error| format!("{phase}: {error}; log {}", launch.log.display()))?;
    status
        .code()
        .ok_or_else(|| format!("{phase}: ended by a signal: {status}").into())
}

/// The primary keeps its mode after a secondary came and went: it is still
/// running and its next reports carry the same value.
fn unchanged(primary: &mut Launch, phase: &str, development: bool) -> Result<()> {
    let seen = primary.reports();
    wait(
        primary,
        phase,
        10,
        |launch| Ok(launch.reports() >= seen + 2),
    )?;
    if !primary.process.is_running()? || primary.mode() != Some(development) {
        return Err(format!("{phase}: primary mode changed to {:?}", primary.mode()).into());
    }
    Ok(())
}

fn stop(mut launch: Launch) -> Result<()> {
    launch.process.cleanup()
}

pub fn run(binary: &Path) -> Result<()> {
    let mut runner = Runner::new(binary)?;

    let mut ordinary = runner.primary("ordinary-primary", false)?;
    let mut refused = runner.launch("development-secondary", true)?;
    let code = exit_code(&mut refused, "development-secondary")?;
    if code != REFUSED || !refused.text().contains(REFUSAL) {
        return Err(format!("development secondary exited {code} without the refusal").into());
    }
    unchanged(&mut ordinary, "ordinary-primary-after-secondary", false)?;
    println!("PASS a secondary --plugin-development exits {REFUSED} with its diagnostic; the ordinary primary stays ordinary");
    stop(ordinary)?;

    let mut development = runner.primary("development-primary", true)?;
    let mut forwarded = runner.launch("ordinary-secondary", false)?;
    let code = exit_code(&mut forwarded, "ordinary-secondary")?;
    if code != 0 {
        return Err(format!("ordinary secondary exited {code}").into());
    }
    unchanged(
        &mut development,
        "development-primary-after-secondary",
        true,
    )?;
    println!(
        "PASS an ordinary secondary activates the development primary without changing its mode"
    );
    stop(development)?;

    let relaunched = runner.primary("ordinary-relaunch", false)?;
    println!("PASS a later ordinary relaunch is ordinary: the mode was not saved");
    stop(relaunched)?;

    // Both orders of a simultaneous start: whichever process registers first is
    // the primary and keeps its own mode; the other cannot change it.
    for (round, first_development) in [(1, true), (2, false), (3, true), (4, false)] {
        let first = runner.launch(&format!("race{round}-a"), first_development)?;
        let second = runner.launch(&format!("race{round}-b"), !first_development)?;
        let mut launches = [first, second];
        let deadline = Instant::now() + Duration::from_secs(20);
        let primary = loop {
            let running: Vec<usize> = (0..2)
                .filter(|&index| launches[index].mode().is_some())
                .collect();
            if let [index] = running.as_slice() {
                break *index;
            }
            if running.len() > 1 {
                return Err(format!("race {round}: two processes became primary").into());
            }
            if Instant::now() >= deadline {
                return Err(format!("race {round}: no primary reported a mode").into());
            }
            std::thread::sleep(Duration::from_millis(100));
        };
        let other = 1 - primary;
        let primary_development = if primary == 0 {
            first_development
        } else {
            !first_development
        };
        let code = exit_code(&mut launches[other], &format!("race{round}-secondary"))?;
        let refused_text = launches[other].text().contains(REFUSAL);
        let expected = if primary_development {
            code == 0 && !refused_text
        } else {
            code == REFUSED && refused_text
        };
        if !expected {
            return Err(format!("race {round}: secondary exited {code}").into());
        }
        let [a, b] = launches;
        let mut primary_launch = if primary == 0 { a } else { b };
        unchanged(
            &mut primary_launch,
            &format!("race{round}-primary"),
            primary_development,
        )?;
        println!("PASS race {round}: the actual primary kept development={primary_development}; the secondary could not change it");
        stop(primary_launch)?;
    }
    Ok(())
}
