use crate::Result;
use nix::sys::signal::{killpg, Signal};
use nix::sys::wait::{waitid, Id, WaitPidFlag, WaitStatus};
use nix::unistd::Pid;
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs::File;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Child, ChildStdin, ChildStdout, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

pub fn allowed_environment(
    input: &BTreeMap<OsString, OsString>,
    allowed: &[&str],
) -> BTreeMap<OsString, OsString> {
    input
        .iter()
        .filter(|(key, _)| {
            allowed
                .iter()
                .any(|name| key.as_os_str() == std::ffi::OsStr::new(name))
        })
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect()
}

pub struct OwnedProcess {
    child: Child,
    group: Pid,
    cleaned: bool,
}

impl OwnedProcess {
    pub fn spawn_interactive(
        program: &Path,
        args: &[&str],
        env: &BTreeMap<OsString, OsString>,
        cwd: &Path,
        log: &File,
    ) -> Result<Self> {
        let child = Command::new(program)
            .args(args)
            .env_clear()
            .envs(env)
            .current_dir(cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(log.try_clone()?)
            .process_group(0)
            .spawn()?;
        let group = Pid::from_raw(i32::try_from(child.id())?);
        Ok(Self {
            child,
            group,
            cleaned: false,
        })
    }

    pub fn take_stdin(&mut self) -> Result<ChildStdin> {
        self.child
            .stdin
            .take()
            .ok_or_else(|| "owned process has no interactive stdin".into())
    }
    pub fn spawn_capture(
        program: &Path,
        args: &[&str],
        env: &BTreeMap<OsString, OsString>,
        cwd: &Path,
        log: &File,
    ) -> Result<Self> {
        let child = Command::new(program)
            .args(args)
            .env_clear()
            .envs(env)
            .current_dir(cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(log.try_clone()?)
            .process_group(0)
            .spawn()?;
        let group = Pid::from_raw(i32::try_from(child.id())?);
        Ok(Self {
            child,
            group,
            cleaned: false,
        })
    }

    pub fn take_stdout(&mut self) -> Result<ChildStdout> {
        self.child
            .stdout
            .take()
            .ok_or_else(|| "owned process has no captured stdout".into())
    }

    pub fn is_running(&mut self) -> Result<bool> {
        if self.cleaned {
            return Ok(false);
        }
        Ok(matches!(
            waitid(
                Id::Pid(self.group),
                WaitPidFlag::WEXITED | WaitPidFlag::WNOHANG | WaitPidFlag::WNOWAIT
            )?,
            WaitStatus::StillAlive
        ))
    }
    pub fn spawn(
        program: &Path,
        args: &[&str],
        env: &BTreeMap<OsString, OsString>,
        cwd: &Path,
        log: &File,
    ) -> Result<Self> {
        Self::spawn_split(program, args, env, cwd, log, log)
    }

    pub fn spawn_split(
        program: &Path,
        args: &[&str],
        env: &BTreeMap<OsString, OsString>,
        cwd: &Path,
        stdout: &File,
        stderr: &File,
    ) -> Result<Self> {
        let child = Command::new(program)
            .args(args)
            .env_clear()
            .envs(env)
            .current_dir(cwd)
            .stdin(Stdio::null())
            .stdout(stdout.try_clone()?)
            .stderr(stderr.try_clone()?)
            .process_group(0)
            .spawn()?;
        let group = Pid::from_raw(i32::try_from(child.id())?);
        Ok(Self {
            child,
            group,
            cleaned: false,
        })
    }

    pub fn id(&self) -> u32 {
        self.child.id()
    }

    pub fn signal(&mut self, signal: Signal) -> Result<()> {
        if !self.cleaned {
            match killpg(self.group, signal) {
                Ok(()) | Err(nix::errno::Errno::ESRCH) => {}
                Err(error) => return Err(error.into()),
            }
        }
        Ok(())
    }

    pub fn wait(&mut self, timeout: Duration) -> Result<ExitStatus> {
        if self.cleaned {
            return Ok(self.child.wait()?);
        }
        let deadline = Instant::now() + timeout;
        loop {
            if !self.is_running()? {
                self.cleanup()?;
                return Ok(self.child.wait()?);
            }
            if Instant::now() >= deadline {
                self.cleanup()?;
                return Err("owned process timed out; its process group was stopped".into());
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    pub fn cleanup(&mut self) -> Result<()> {
        if self.cleaned {
            return Ok(());
        }
        // WNOWAIT keeps the leader (even a zombie) unreaped until group cleanup,
        // preventing its PID/group ID being reused for an unrelated child.
        match killpg(self.group, Signal::SIGTERM) {
            Ok(()) | Err(nix::errno::Errno::ESRCH) => {}
            Err(error) => return Err(error.into()),
        }
        std::thread::sleep(Duration::from_millis(50));
        match killpg(self.group, Signal::SIGKILL) {
            Ok(()) | Err(nix::errno::Errno::ESRCH) => {}
            Err(error) => return Err(error.into()),
        }
        self.child.wait()?;
        self.cleaned = true;
        Ok(())
    }
}

impl Drop for OwnedProcess {
    fn drop(&mut self) {
        let _ = self.cleanup();
    }
}
