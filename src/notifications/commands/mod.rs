//! optional local command attempts; policy admission is not a delivery receipt.
//!
//! no runtime is created by inspecting configuration. callers explicitly supply
//! the trusted host executable and a private working directory, then register
//! channels off by default. only `poll` dispatches core-admitted work.

mod process;
mod protocol;
mod service;

use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use super::{Channel, DiagnosticKind, Host, Job, Policy};

pub const DEADLINE: Duration = Duration::from_secs(10);
pub const MAX_OUTPUT: usize = 32 * 1024;
pub const MAX_PAYLOAD: usize = 16 * 1024;
pub const MAX_ACTIVE: usize = 2;
pub const MAX_ARGUMENTS: usize = 128;
pub const MAX_CONFIGURATION: usize = 16 * 1024;
pub const CLEANUP_GRACE: Duration = Duration::from_millis(250);
pub const CLEANUP_DEADLINE: Duration = Duration::from_secs(2);
pub const SUPERVISOR_ARGUMENT: &str = "--among-internal-command-supervisor";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    InvalidExecutable,
    InvalidArguments,
    InvalidDirectory,
    StaleChannel,
    Exhausted,
    Stopped,
}

/// validation is syntactic and inert: it never probes an executable by running it.
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Configuration {
    executable: String,
    arguments: Vec<String>,
}

impl Configuration {
    pub fn new(executable: &Path, arguments: Vec<String>) -> Result<Self, Error> {
        let executable = executable.to_str().ok_or(Error::InvalidExecutable)?;
        let config = Self {
            executable: executable.to_owned(),
            arguments,
        };
        config.validate()?;
        Ok(config)
    }

    fn validate(&self) -> Result<(), Error> {
        if !Path::new(&self.executable).is_absolute()
            || self.executable.contains('\0')
            || self.executable.len() > MAX_CONFIGURATION
        {
            return Err(Error::InvalidExecutable);
        }
        if self.arguments.len() > MAX_ARGUMENTS
            || self.arguments.iter().any(|arg| arg.contains('\0'))
            || self.arguments.iter().map(String::len).sum::<usize>()
                > MAX_CONFIGURATION.saturating_sub(self.executable.len())
        {
            return Err(Error::InvalidArguments);
        }
        Ok(())
    }
}

/// open the already-created application-owned directory off the UI loop. the
/// retained fd, not a terminal path or a later path lookup, establishes cwd.
#[derive(Clone)]
pub struct OwnedDirectory {
    file: Arc<File>,
    home: PathBuf,
}

impl OwnedDirectory {
    pub fn open(path: &Path) -> Result<Self, Error> {
        if !path.is_absolute() {
            return Err(Error::InvalidDirectory);
        }
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(path)
            .map_err(|_| Error::InvalidDirectory)?;
        let metadata = file.metadata().map_err(|_| Error::InvalidDirectory)?;
        if metadata.uid() != unsafe { libc::geteuid() } || metadata.mode() & 0o777 != 0o700 {
            return Err(Error::InvalidDirectory);
        }
        Ok(Self {
            file: Arc::new(file),
            home: path.to_owned(),
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Reason {
    Revoked,
    Cancelled,
    Deadline,
    OutputLimit,
    SpawnFailed,
    Protocol,
    PipeIo,
    SupervisorLost,
    CleanupIncomplete,
    ServiceUnavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    NotStarted(Reason),
    /// only the local executable's exit status; never proof of remote receipt.
    Exited(i32),
    Signaled(i32),
    /// execution may already have performed an external side effect.
    Uncertain(Reason),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Completion {
    pub job: Job,
    pub outcome: Outcome,
    pub output_bytes: usize,
    pub elapsed: Duration,
    pub reaped: bool,
}

pub struct Runtime {
    helper: PathBuf,
    cwd: OwnedDirectory,
    configurations: BTreeMap<Channel, Configuration>,
    registrations: BTreeMap<Channel, Arc<AtomicBool>>,
    attempts: Vec<service::Ticket>,
    cursor: usize,
    stopped: bool,
}

impl Runtime {
    pub fn new(helper: &Path, cwd: OwnedDirectory) -> Result<Self, Error> {
        if !helper.is_absolute() || helper.to_str().is_none_or(|path| path.contains('\0')) {
            return Err(Error::InvalidExecutable);
        }
        Ok(Self {
            helper: helper.to_owned(),
            cwd,
            configurations: BTreeMap::new(),
            registrations: BTreeMap::new(),
            attempts: Vec::new(),
            cursor: 0,
            stopped: false,
        })
    }

    pub fn register(&mut self, host: &mut Host, config: Configuration) -> Result<Channel, Error> {
        if self.stopped {
            return Err(Error::Stopped);
        }
        config.validate()?;
        let channel = host
            .register_channel(Policy::DELIVERY_DEFAULT)
            .map_err(core_error)?;
        let registration = host.registration(channel).ok_or(Error::StaleChannel)?;
        self.registrations.insert(channel, registration);
        self.configurations.insert(channel, config);
        Ok(channel)
    }

    pub fn replace(
        &mut self,
        host: &mut Host,
        channel: Channel,
        config: Configuration,
    ) -> Result<(), Error> {
        config.validate()?;
        if !self.configurations.contains_key(&channel) {
            return Err(Error::StaleChannel);
        }
        host.cancel_channel_work(channel).map_err(core_error)?;
        self.cancel(channel);
        self.configurations.insert(channel, config);
        Ok(())
    }

    pub fn set_policy(
        &mut self,
        host: &mut Host,
        channel: Channel,
        policy: Policy,
    ) -> Result<(), Error> {
        if !self.configurations.contains_key(&channel) {
            return Err(Error::StaleChannel);
        }
        host.set_channel_policy(channel, policy).map_err(core_error)
    }

    pub fn remove(&mut self, host: &mut Host, channel: Channel) {
        if self.configurations.remove(&channel).is_none() {
            return;
        }
        host.remove_channel(channel);
        self.cancel(channel);
        if let Some(registration) = self.registrations.remove(&channel) {
            registration.store(false, Ordering::Release);
        }
    }

    fn cancel(&self, channel: Channel) {
        for attempt in &self.attempts {
            if attempt.job.channel() == channel {
                attempt.cancel();
            }
        }
    }

    /// nonblocking admission/poll only: all spawn, pipe IO and reaping occur in
    /// the bounded service or the fresh supervisor, never on this caller.
    pub fn poll(&mut self, host: &mut Host, now: Instant) {
        let mut completed = Vec::new();
        self.attempts.retain(|attempt| {
            if !host.is_current(attempt.job) {
                attempt.cancel();
            }
            if let Some(completion) = attempt.completion() {
                completed.push(completion);
                false
            } else {
                true
            }
        });
        for completion in completed {
            host.diagnose(DiagnosticKind::CommandCompletion(completion));
        }
        if self.stopped || self.configurations.is_empty() {
            return;
        }
        let channels: Vec<_> = self.configurations.keys().copied().collect();
        for offset in 0..channels.len() {
            let index = (self.cursor + offset) % channels.len();
            let channel = channels[index];
            let Some(reservation) = service::reserve(channel) else {
                continue;
            };
            let Some(job) = host.take_ready(channel, now) else {
                continue;
            };
            let Some(authorization) = host.authorization(job) else {
                continue;
            };
            let Some(config) = self.configurations.get(&channel) else {
                continue;
            };
            match service::submit(
                job,
                authorization,
                config.clone(),
                self.helper.clone(),
                self.cwd.clone(),
                reservation,
            ) {
                Ok(ticket) => self.attempts.push(ticket),
                Err(outcome) => host.diagnose(DiagnosticKind::CommandCompletion(Completion {
                    job,
                    outcome,
                    output_bytes: 0,
                    elapsed: Duration::ZERO,
                    reaped: true,
                })),
            }
        }
        self.cursor = (self.cursor + 1) % channels.len();
    }

    pub fn shutdown(&mut self, host: &mut Host) {
        self.stopped = true;
        let channels: Vec<_> = self.configurations.keys().copied().collect();
        for channel in channels {
            self.remove(host, channel);
        }
        for attempt in &self.attempts {
            attempt.cancel();
        }
    }
}

impl Drop for Runtime {
    fn drop(&mut self) {
        // no join or child wait here; the service retains capacity through reap.
        for registration in self.registrations.values() {
            registration.store(false, Ordering::Release);
        }
        for attempt in &self.attempts {
            attempt.cancel();
        }
    }
}

fn core_error(error: super::Failure) -> Error {
    match error {
        super::Failure::Exhausted => Error::Exhausted,
        _ => Error::StaleChannel,
    }
}

pub fn active_count() -> usize {
    service::active_count()
}

/// call before GTK initialization in the trusted application executable.
/// this is an internal pipe protocol, not a user-configurable interpreter.
pub fn supervisor_entry() -> Option<u8> {
    let mut arguments = std::env::args_os();
    arguments.next();
    if arguments.next().as_deref() != Some(std::ffi::OsStr::new(SUPERVISOR_ARGUMENT)) {
        return None;
    }
    Some(if arguments.next().is_none() {
        process::supervise()
    } else {
        2
    })
}
