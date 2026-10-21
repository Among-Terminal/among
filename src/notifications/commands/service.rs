//! one fixed IO service, two globally reserved attempts, no GUI-thread joins.

use std::io::{self, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{sync_channel, Receiver, SyncSender};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use super::process::{nonblocking, ExactExec, INTERVAL};
use super::protocol::{self, Payload, Report, MAX_REPORT};
use super::{
    Completion, Configuration, Outcome, OwnedDirectory, Reason, CLEANUP_DEADLINE, CLEANUP_GRACE,
    DEADLINE, MAX_ACTIVE, SUPERVISOR_ARGUMENT,
};
use crate::notifications::{Authorization, Channel, Job};

static CAPACITY: Mutex<Vec<Slot>> = Mutex::new(Vec::new());
static SERVICE: OnceLock<Option<SyncSender<Work>>> = OnceLock::new();

struct Slot {
    channel: Channel,
    occupied: Arc<AtomicBool>,
}

pub(super) struct Reservation {
    occupied: Arc<AtomicBool>,
}

pub(super) fn reserve(channel: Channel) -> Option<Reservation> {
    let mut slots = CAPACITY.try_lock().ok()?;
    slots.retain(|slot| slot.occupied.load(Ordering::Acquire));
    if slots.len() >= MAX_ACTIVE || slots.iter().any(|slot| slot.channel == channel) {
        return None;
    }
    let occupied = Arc::new(AtomicBool::new(true));
    slots.push(Slot {
        channel,
        occupied: occupied.clone(),
    });
    Some(Reservation { occupied })
}

impl Drop for Reservation {
    fn drop(&mut self) {
        // release may happen on a GUI-side failed admission. never wait for the
        // service's mutex here; the next try-lock prunes this released slot.
        self.occupied.store(false, Ordering::Release);
    }
}

pub(super) fn active_count() -> usize {
    CAPACITY
        .try_lock()
        .map(|slots| {
            slots
                .iter()
                .filter(|slot| slot.occupied.load(Ordering::Acquire))
                .count()
        })
        .unwrap_or(MAX_ACTIVE)
}

pub(super) struct Ticket {
    pub job: Job,
    cancel: Arc<AtomicBool>,
    result: Receiver<Completion>,
}

impl Ticket {
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Release);
    }
    pub fn completion(&self) -> Option<Completion> {
        self.result.try_recv().ok()
    }
}

struct Work {
    job: Job,
    authorization: Authorization,
    cancel: Arc<AtomicBool>,
    result: SyncSender<Completion>,
    reservation: Reservation,
    frame: Vec<u8>,
    helper: PathBuf,
    cwd: OwnedDirectory,
    admitted: Instant,
}

pub(super) fn submit(
    job: Job,
    authorization: Authorization,
    config: Configuration,
    helper: PathBuf,
    cwd: OwnedDirectory,
    reservation: Reservation,
) -> Result<Ticket, Outcome> {
    let frame = protocol::encode(&protocol::Request {
        config,
        payload: Payload::from_event(job.event()),
    })
    .map_err(|_| Outcome::NotStarted(Reason::Protocol))?;
    let service = SERVICE
        .get_or_init(|| {
            let (send, receive) = sync_channel::<Work>(MAX_ACTIVE);
            std::thread::Builder::new()
                .name("among-command-io".into())
                .spawn(move || serve(receive))
                .ok()
                .map(|_| send)
        })
        .as_ref()
        .ok_or(Outcome::NotStarted(Reason::ServiceUnavailable))?;
    let (result, receive) = sync_channel(1);
    let cancel = Arc::new(AtomicBool::new(false));
    service
        .try_send(Work {
            job,
            authorization,
            cancel: cancel.clone(),
            result,
            reservation,
            frame,
            helper,
            cwd,
            admitted: Instant::now(),
        })
        .map_err(|_| Outcome::NotStarted(Reason::ServiceUnavailable))?;
    Ok(Ticket {
        job,
        cancel,
        result: receive,
    })
}

fn serve(input: Receiver<Work>) {
    let mut active = Vec::<Attempt>::new();
    loop {
        if active.is_empty() {
            match input.recv() {
                Ok(work) => {
                    if let Some(attempt) = Attempt::start(work) {
                        active.push(attempt);
                    }
                }
                Err(_) => return,
            }
        }
        while active.len() < MAX_ACTIVE {
            match input.try_recv() {
                Ok(work) => {
                    if let Some(attempt) = Attempt::start(work) {
                        active.push(attempt);
                    }
                }
                Err(_) => break,
            }
        }
        active.retain_mut(|attempt| !attempt.poll());
        std::thread::sleep(INTERVAL);
    }
}

struct Attempt {
    work: Work,
    child: Option<Child>,
    stdin: Option<ChildStdin>,
    stdout: Option<ChildStdout>,
    written: usize,
    received: Vec<u8>,
    ready: bool,
    granted: bool,
    reason: Option<Reason>,
    stopping: Option<Instant>,
    report: Option<Report>,
    notified: bool,
    quarantined: bool,
}

impl Attempt {
    fn start(work: Work) -> Option<Self> {
        if !work.authorization.is_current() || work.cancel.load(Ordering::Acquire) {
            send(&work, Outcome::NotStarted(Reason::Revoked), 0, true);
            return None;
        }
        let mut command = Command::new(&work.helper);
        let exact =
            match ExactExec::new(&work.helper, &[SUPERVISOR_ARGUMENT.into()], &work.cwd.home) {
                Ok(exact) => exact,
                Err(_) => {
                    send(&work, Outcome::NotStarted(Reason::SpawnFailed), 0, true);
                    return None;
                }
            };
        command
            .arg(SUPERVISOR_ARGUMENT)
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("LANG", "C.UTF-8")
            .env("HOME", &work.cwd.home)
            // cwd is resolved in the forked child, before CLOEXEC closes this fd.
            .current_dir(format!("/proc/self/fd/{}", work.cwd.file.as_raw_fd()))
            .process_group(0)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        unsafe {
            command.pre_exec(move || {
                if libc::syscall(
                    libc::SYS_close_range,
                    3u32,
                    u32::MAX,
                    libc::CLOSE_RANGE_CLOEXEC,
                ) != 0
                {
                    return Err(io::Error::last_os_error());
                }
                exact.execute()
            });
        }
        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(_) => {
                send(&work, Outcome::NotStarted(Reason::SpawnFailed), 0, true);
                return None;
            }
        };
        let stdin = child.stdin.take();
        let stdout = child.stdout.take();
        let valid = stdin
            .as_ref()
            .zip(stdout.as_ref())
            .is_some_and(|(input, output)| {
                nonblocking(input.as_raw_fd()).is_ok() && nonblocking(output.as_raw_fd()).is_ok()
            });
        Some(Self {
            work,
            child: Some(child),
            stdin,
            stdout,
            written: 0,
            received: Vec::new(),
            ready: false,
            granted: false,
            reason: (!valid).then_some(Reason::PipeIo),
            stopping: None,
            report: None,
            notified: false,
            quarantined: false,
        })
    }

    fn poll(&mut self) -> bool {
        if self.quarantined {
            return false;
        } // capacity remains held; never retry unknown effects
        let mut parent_reason = self.reason;
        if self.work.admitted.elapsed() >= DEADLINE {
            parent_reason.get_or_insert(Reason::Deadline);
        }
        if !self.work.authorization.is_current() {
            parent_reason.get_or_insert(Reason::Revoked);
        }
        if self.work.cancel.load(Ordering::Acquire) {
            parent_reason.get_or_insert(Reason::Cancelled);
        }
        self.reason = parent_reason;
        if self.reason.is_some() && self.stopping.is_none() {
            self.stdin.take(); // EOF cancels even if a control-byte write would block
            if let Some(child) = &self.child {
                unsafe {
                    libc::kill(child.id() as i32, libc::SIGTERM);
                }
            }
            self.stopping = Some(Instant::now());
        }
        if self.reason.is_none() {
            self.write_request();
        }
        self.read_report();
        if self.reason.is_none()
            && self.ready
            && self.written == self.work.frame.len()
            && !self.granted
        {
            // the permission check occurs after the real helper's ready handshake,
            // not merely at dequeue, and is repeated during the entire attempt.
            if !self.work.authorization.is_current() || self.work.cancel.load(Ordering::Acquire) {
                self.reason = Some(Reason::Revoked);
            } else if let Some(stdin) = &mut self.stdin {
                match stdin.write(b"G") {
                    Ok(1) => self.granted = true,
                    Err(error)
                        if matches!(
                            error.kind(),
                            io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                        ) => {}
                    _ => self.reason = Some(Reason::PipeIo),
                }
            }
        }
        if !self.granted
            && self
                .stopping
                .is_some_and(|start| start.elapsed() >= CLEANUP_GRACE)
        {
            if let Some(child) = &self.child {
                let _ = unsafe { libc::kill(child.id() as i32, libc::SIGKILL) };
            }
        }
        let exited = match self.child.as_mut().map(Child::try_wait) {
            Some(Ok(Some(status))) => Some(status),
            Some(Ok(None)) => None,
            _ => {
                self.finish(Outcome::Uncertain(Reason::SupervisorLost), 0, false);
                self.quarantined = true;
                return false;
            }
        };
        if let Some(status) = exited {
            self.read_report();
            let (outcome, bytes, reaped) = if let Some(report) = &self.report {
                (
                    self.reason
                        .map(|reason| {
                            if self.granted {
                                Outcome::Uncertain(reason)
                            } else {
                                Outcome::NotStarted(reason)
                            }
                        })
                        .unwrap_or(report.outcome),
                    report.output_bytes,
                    report.reaped,
                )
            } else if !self.granted {
                (
                    Outcome::NotStarted(self.reason.unwrap_or(Reason::SupervisorLost)),
                    0,
                    true,
                )
            } else {
                (Outcome::Uncertain(Reason::SupervisorLost), 0, false)
            };
            self.finish(outcome, bytes, reaped);
            self.child.take();
            // the trusted helper exits zero only after ECHILD. an earlier
            // cleanup-incomplete report stays uncertain, but capacity can now
            // be released instead of stranding a recovered kernel child slot.
            if !reaped && self.granted && !(status.success() && self.report.is_some()) {
                self.quarantined = true;
                return false;
            }
            return true;
        }
        if self.report.as_ref().is_some_and(|report| !report.reaped)
            || self.work.admitted.elapsed()
                >= DEADLINE + CLEANUP_GRACE + CLEANUP_DEADLINE + Duration::from_millis(250)
        {
            self.finish(
                Outcome::Uncertain(Reason::CleanupIncomplete),
                self.report.as_ref().map_or(0, |report| report.output_bytes),
                false,
            );
        }
        false
    }

    fn write_request(&mut self) {
        if self.written == self.work.frame.len() {
            return;
        }
        if let Some(stdin) = &mut self.stdin {
            match stdin.write(&self.work.frame[self.written..]) {
                Ok(0) => self.reason = Some(Reason::PipeIo),
                Ok(count) => self.written += count,
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                    ) => {}
                Err(_) => self.reason = Some(Reason::PipeIo),
            }
        }
    }

    fn read_report(&mut self) {
        if self.report.is_some() {
            return;
        }
        let mut bytes = [0; MAX_REPORT + 5];
        if let Some(stdout) = &mut self.stdout {
            match stdout.read(&mut bytes) {
                Ok(count) => {
                    if self.received.len() + count > MAX_REPORT + 5 {
                        self.reason = Some(Reason::Protocol);
                        return;
                    }
                    self.received.extend_from_slice(&bytes[..count]);
                }
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                    ) => {}
                Err(_) => self.reason = Some(Reason::PipeIo),
            }
        }
        if !self.ready && !self.received.is_empty() {
            if self.received.remove(0) != b'R' {
                self.reason = Some(Reason::Protocol);
                return;
            }
            self.ready = true;
        }
        if self.received.len() >= 4 {
            let length = u32::from_le_bytes([
                self.received[0],
                self.received[1],
                self.received[2],
                self.received[3],
            ]) as usize;
            if length > MAX_REPORT {
                self.reason = Some(Reason::Protocol);
                return;
            }
            if self.received.len() >= 4 + length {
                match serde_json::from_slice::<Report>(&self.received[4..4 + length]) {
                    Ok(report) if report.output_bytes <= super::MAX_OUTPUT => {
                        self.report = Some(report)
                    }
                    _ => self.reason = Some(Reason::Protocol),
                }
            }
        }
    }

    fn finish(&mut self, outcome: Outcome, bytes: usize, reaped: bool) {
        if !self.notified {
            send(&self.work, outcome, bytes, reaped);
            self.notified = true;
        }
    }
}

fn send(work: &Work, outcome: Outcome, output_bytes: usize, reaped: bool) {
    let _ = &work.reservation; // reservation lives through helper reap, not result delivery
    let _ = work.result.try_send(Completion {
        job: work.job,
        outcome,
        output_bytes,
        elapsed: work.admitted.elapsed(),
        reaped,
    });
}
