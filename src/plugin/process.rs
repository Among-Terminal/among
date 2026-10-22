//! one cancellable worker owns spawn, nonblocking pipes and retirement. retaining
//! its handle through group quiescence and leader reap prevents overlapping generations.

use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};
use std::sync::mpsc::{sync_channel, Receiver, SyncSender, TrySendError};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use super::protocol::Request;
use super::transport::{FrameDecoder, MAX_FRAME_BYTES, MAX_QUEUE_FRAMES};

pub const WRITE_TIMEOUT: Duration = Duration::from_secs(2);
const STOP_GRACE: Duration = Duration::from_millis(500);
const IO_INTERVAL_MS: i32 = 20;
const SPAWN_BUSY_TIMEOUT: Duration = Duration::from_secs(1);

#[cfg(test)]
mod spawn_tests;

#[cfg(test)]
mod retirement_tests;

pub enum Failure {
    Missing(String),
    Fault(&'static str),
}

#[derive(Debug, PartialEq, Eq)]
pub enum SendError {
    Full,
    Fault(&'static str),
}

pub fn pinned_program(
    directory: std::fs::File,
    executable: std::fs::File,
    lease: Option<Arc<std::fs::File>>,
) -> Command {
    let mut program = Command::new(format!("/proc/self/fd/{}", executable.as_raw_fd()));
    program.current_dir(format!("/proc/self/fd/{}", directory.as_raw_fd()));
    // pin both descriptors through fork/exec. only the child's executable fd
    // loses CLOEXEC so a shebang interpreter can open that exact admitted file;
    // changing descriptor flags in pre_exec does not leak it from the host.
    unsafe {
        program.pre_exec(move || {
            // command lives in the supervisor through reaping, including host
            // teardown. a detached stopping worker must still retain its lease.
            let _held = &lease;
            let _keep_package_open = &directory;
            if libc::fcntl(executable.as_raw_fd(), libc::F_SETFD, 0) < 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    program
}

struct Outbound {
    bytes: Box<[u8]>,
    queued: Instant,
    written: usize,
}

pub struct Process {
    pub frames: Receiver<String>,
    failures: Receiver<Failure>,
    input: SyncSender<Outbound>,
    outstanding: Arc<AtomicUsize>,
    cancel: Arc<AtomicBool>,
    pid: Arc<AtomicU32>,
    worker: JoinHandle<()>,
}

impl Process {
    pub fn start(mut command: Command) -> io::Result<Self> {
        command
            .process_group(0)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let (frame_sender, frames) = sync_channel(MAX_QUEUE_FRAMES);
        let (input, requests) = sync_channel(MAX_QUEUE_FRAMES);
        let (failure_sender, failures) = sync_channel(1);
        let cancel = Arc::new(AtomicBool::new(false));
        let outstanding = Arc::new(AtomicUsize::new(0));
        let pid = Arc::new(AtomicU32::new(0));
        let worker_cancel = cancel.clone();
        let worker_outstanding = outstanding.clone();
        let worker_pid = pid.clone();
        // spawn the worker before the child, so a thread-creation failure cannot
        // strand a child or force a GUI-thread wait in the error path.
        let worker = std::thread::Builder::new()
            .name("among-plugin-supervisor".into())
            .spawn(move || {
                if worker_cancel.load(Ordering::Acquire) {
                    return;
                }
                let mut child = match start_child(&mut command, &worker_cancel) {
                    Ok(Some(child)) => child,
                    Ok(None) => return,
                    Err(failure) => {
                        let _ = failure_sender.try_send(failure);
                        return;
                    }
                };
                worker_pid.store(child.id(), Ordering::Release);
                if let Err(reason) = pump(
                    &mut child,
                    requests,
                    frame_sender,
                    &worker_cancel,
                    &worker_outstanding,
                ) {
                    let _ = failure_sender.try_send(Failure::Fault(reason));
                }
                terminate_and_reap(&mut child);
            })?;
        Ok(Self {
            frames,
            failures,
            input,
            outstanding,
            cancel,
            pid,
            worker,
        })
    }

    pub fn started(&self) -> bool {
        self.pid.load(Ordering::Acquire) != 0
    }

    #[cfg(test)]
    pub fn pid(&self) -> u32 {
        self.pid.load(Ordering::Acquire)
    }

    pub fn finished(&self) -> bool {
        self.worker.is_finished()
    }

    pub fn failure(&self) -> Option<Failure> {
        self.failures.try_recv().ok()
    }

    pub fn stop(&self) {
        self.cancel.store(true, Ordering::Release);
    }

    pub fn has_capacity(&self) -> bool {
        self.outstanding.load(Ordering::Acquire) < MAX_QUEUE_FRAMES
    }

    pub fn send(&self, request: &impl serde::Serialize) -> Result<(), SendError> {
        if self.cancel.load(Ordering::Acquire) {
            return Err(SendError::Fault("plugin is stopping"));
        }
        if !self.has_capacity() {
            return Err(SendError::Full);
        }
        let mut encoded = BoundedFrame(Vec::new());
        serde_json::to_writer(&mut encoded, request)
            .map_err(|_| SendError::Fault("host request exceeds 64 KiB"))?;
        encoded
            .write_all(b"\n")
            .map_err(|_| SendError::Fault("host request exceeds 64 KiB"))?;
        // include the actively written frame in the 16-frame/1-MiB budget.
        self.outstanding
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |count| {
                (count < MAX_QUEUE_FRAMES).then_some(count + 1)
            })
            .map_err(|_| SendError::Full)?;
        let frame = Outbound {
            bytes: encoded.0.into_boxed_slice(),
            queued: Instant::now(),
            written: 0,
        };
        match self.input.try_send(frame) {
            Ok(()) => Ok(()),
            Err(error) => {
                self.outstanding.fetch_sub(1, Ordering::AcqRel);
                Err(match error {
                    TrySendError::Full(_) => SendError::Full,
                    TrySendError::Disconnected(_) => SendError::Fault("plugin stdin queue closed"),
                })
            }
        }
    }
}

impl Drop for Process {
    fn drop(&mut self) {
        self.stop();
        // dropping a JoinHandle detaches; only the worker may wait for a child.
        // normal polling retains this object until finished, including faults.
    }
}

struct BoundedFrame(Vec<u8>);

fn start_child(command: &mut Command, cancel: &AtomicBool) -> Result<Option<Child>, Failure> {
    let started = Instant::now();
    loop {
        if cancel.load(Ordering::Acquire) {
            return Ok(None);
        }
        match command.spawn() {
            Ok(child) => return Ok(Some(child)),
            // exec refuses an inode still open for writing. retry only ETXTBSY,
            // off-loop and within the existing admission deadline.
            Err(error)
                if error.raw_os_error() == Some(libc::ETXTBSY)
                    && started.elapsed() < SPAWN_BUSY_TIMEOUT =>
            {
                std::thread::sleep(Duration::from_millis(IO_INTERVAL_MS as u64))
            }
            Err(error) => return Err(Failure::Missing(error.to_string())),
        }
    }
}

impl Write for BoundedFrame {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > MAX_FRAME_BYTES.saturating_sub(self.0.len()) {
            return Err(io::Error::other("frame limit"));
        }
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn nonblocking(fd: RawFd) -> io::Result<()> {
    // the owned pipe remains open for both calls and only this worker uses it.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn pump(
    child: &mut Child,
    requests: Receiver<Outbound>,
    frames: SyncSender<String>,
    cancel: &AtomicBool,
    outstanding: &AtomicUsize,
) -> Result<(), &'static str> {
    let Some(mut stdin) = child.stdin.take() else {
        return Err("plugin stdin unavailable");
    };
    let Some(mut stdout) = child.stdout.take() else {
        return Err("plugin stdout unavailable");
    };
    nonblocking(stdin.as_raw_fd()).map_err(|_| "cannot configure plugin stdin")?;
    nonblocking(stdout.as_raw_fd()).map_err(|_| "cannot configure plugin stdout")?;
    let mut decoder = FrameDecoder::default();
    let mut pending: Option<Outbound> = None;
    let mut buffer = [0u8; 16 * 1024];
    loop {
        if cancel.load(Ordering::Acquire) {
            if pending.is_none() {
                // best effort only; never append shutdown inside a partial frame
                // and never wait for a plugin that has stopped reading.
                if let Ok(mut bytes) = serde_json::to_vec(&Request::Shutdown) {
                    bytes.push(b'\n');
                    let _ = stdin.write(&bytes);
                }
            }
            return Ok(());
        }
        if child_exited(child.id()).map_err(|_| "cannot observe plugin process")? {
            return Err("plugin exited");
        }
        if pending.is_none() {
            pending = requests.try_recv().ok();
        }
        if let Some(frame) = pending.as_mut() {
            if frame.queued.elapsed() >= WRITE_TIMEOUT {
                return Err("plugin stdin write exceeded two seconds");
            }
            match stdin.write(&frame.bytes[frame.written..]) {
                Ok(0) => return Err("plugin closed its input"),
                Ok(written) => {
                    frame.written += written;
                    if frame.written == frame.bytes.len() {
                        pending = None;
                        outstanding.fetch_sub(1, Ordering::AcqRel);
                    }
                }
                Err(err)
                    if matches!(
                        err.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                    ) => {}
                Err(_) => return Err("plugin closed its input"),
            }
        }
        // bound each pass so stdout cannot postpone cancellation or stdin work.
        for _ in 0..MAX_QUEUE_FRAMES {
            match stdout.read(&mut buffer) {
                Ok(0) => return Err("plugin stdout closed"),
                Ok(count) => decoder.push(&buffer[..count], &frames)?,
                Err(err) if err.kind() == io::ErrorKind::WouldBlock => break,
                Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
                Err(_) => return Err("cannot read plugin stdout"),
            }
        }
        let mut fds = [
            libc::pollfd {
                fd: stdout.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: if pending.is_some() {
                    stdin.as_raw_fd()
                } else {
                    -1
                },
                events: if pending.is_some() { libc::POLLOUT } else { 0 },
                revents: 0,
            },
        ];
        // both descriptors and the array outlive this bounded syscall.
        let result =
            unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, IO_INTERVAL_MS) };
        if result < 0 && io::Error::last_os_error().kind() != io::ErrorKind::Interrupted {
            return Err("cannot poll plugin pipes");
        }
    }
}

fn child_exited(pid: u32) -> io::Result<bool> {
    // WNOWAIT keeps the leader's PID reserved until the final group signal;
    // otherwise a fast exit/reap could allow us to signal a recycled group ID.
    let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
    let result = unsafe {
        libc::waitid(
            libc::P_PID,
            pid,
            &mut info,
            libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
        )
    };
    if result < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { info.si_pid() } != 0)
}

fn terminate_and_reap(child: &mut Child) {
    // process_group(0) makes this owned child's PID its group ID. do not claim
    // control of a descendant that deliberately escapes the supervised group.
    if let Ok(pid) = i32::try_from(child.id()) {
        let _ = signal_reserved_group(child, libc::SIGTERM);
        std::thread::sleep(STOP_GRACE);
        let mut group = GroupQuiescence::new(std::path::Path::new("/proc"));
        loop {
            // retain the unreaped leader across every group signal, including
            // retries for a fork already in flight when KILL was first queued.
            if signal_reserved_group(child, libc::SIGKILL).is_ok()
                && matches!(group.poll(pid, member_exited), Ok(true))
            {
                break;
            }
            // unknown procfs/ownership or an uninterruptible member retains this
            // generation. never turn missing confirmation into finished() == true.
            std::thread::sleep(Duration::from_millis(IO_INTERVAL_MS as u64));
        }
    } else {
        let _ = child.kill();
    }
    // an uninterruptible child holds this worker/generation, not the GUI. the
    // caller must not replace it until this function really observes reaping.
    while child.wait().is_err() {
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn signal_reserved_group(child: &Child, signal: i32) -> io::Result<()> {
    let group = i32::try_from(child.id()).map_err(|_| io::Error::other("invalid plugin PID"))?;
    child_exited(child.id())?; // WNOWAIT also refuses a no-longer-owned/reaped PID.
    if unsafe { libc::kill(-group, signal) } < 0 {
        let error = io::Error::last_os_error();
        if error.raw_os_error() != Some(libc::ESRCH) {
            return Err(error);
        }
    }
    Ok(())
}

struct GroupQuiescence<'a> {
    root: &'a std::path::Path,
    entries: Option<std::fs::ReadDir>,
    uncertain: bool,
    prior_quiet: bool,
}

impl<'a> GroupQuiescence<'a> {
    fn new(root: &'a std::path::Path) -> Self {
        Self {
            root,
            entries: None,
            uncertain: false,
            prior_quiet: false,
        }
    }

    fn poll(
        &mut self,
        group: i32,
        mut exited: impl FnMut(u32) -> io::Result<bool>,
    ) -> io::Result<bool> {
        if self.entries.is_none() {
            self.entries = match std::fs::read_dir(self.root) {
                Ok(entries) => Some(entries),
                Err(error) => {
                    self.prior_quiet = false;
                    return Err(error);
                }
            };
            self.uncertain = false;
        }
        // scan incrementally: constant memory and at most 256 entries per worker
        // interval, even on a host with many unrelated processes. no PID signals
        // are derived from this scan; only the still-reserved group is signalled.
        for _ in 0..256 {
            let entry = self.entries.as_mut().and_then(Iterator::next);
            let Some(entry) = entry else {
                self.entries = None;
                if self.uncertain {
                    self.prior_quiet = false;
                    return Err(io::Error::other("cannot confirm plugin group quiescence"));
                }
                // separate complete observations with another group KILL/poll,
                // rather than treating one changing procfs traversal as atomic.
                let confirmed = self.prior_quiet;
                self.prior_quiet = true;
                return Ok(confirmed);
            };
            let entry = match entry {
                Ok(entry) => entry,
                Err(_) => {
                    self.uncertain = true;
                    continue;
                }
            };
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                continue;
            };
            if name.is_empty() || !name.bytes().all(|byte| byte.is_ascii_digit()) {
                continue;
            }
            match member_group(&entry.path().join("stat")) {
                Ok(Some(owner)) if owner == group => {
                    match name.parse::<u32>().ok().map(&mut exited) {
                        Some(Ok(true)) => {}
                        Some(Ok(false)) => {
                            self.entries = None;
                            self.prior_quiet = false;
                            return Ok(false);
                        }
                        _ => self.uncertain = true,
                    }
                }
                Ok(_) => {}
                Err(_) => self.uncertain = true,
            }
        }
        Ok(false)
    }
}

fn member_exited(pid: u32) -> io::Result<bool> {
    // a zombie main thread is not sufficient: other threads may still execute.
    // pidfd readiness observes whole-process exit without waiting/reaping a child
    // belonging to init/another subreaper. keep only this one fd during the check.
    let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) };
    if fd < 0 {
        let error = io::Error::last_os_error();
        return if error.raw_os_error() == Some(libc::ESRCH) {
            Ok(true)
        } else {
            Err(error)
        };
    }
    let fd = unsafe { OwnedFd::from_raw_fd(fd as RawFd) };
    let mut poll = libc::pollfd {
        fd: fd.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    if unsafe { libc::poll(&mut poll, 1, 0) } < 0 {
        return Err(io::Error::last_os_error());
    }
    if poll.revents & (libc::POLLERR | libc::POLLNVAL) != 0 {
        return Err(io::Error::other("cannot confirm plugin member exit"));
    }
    Ok(poll.revents & libc::POLLIN != 0)
}

fn member_group(path: &std::path::Path) -> io::Result<Option<i32>> {
    let gone = |error: &io::Error| {
        error.kind() == io::ErrorKind::NotFound || error.raw_os_error() == Some(libc::ESRCH)
    };
    let mut file = match std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
    {
        Ok(file) => file,
        Err(error) if gone(&error) => return Ok(None),
        Err(error) => return Err(error),
    };
    if !file.metadata()?.is_file() {
        return Err(io::Error::other("procfs observation is not a regular file"));
    }
    let mut bytes = [0; 4096];
    let mut used = 0;
    for _ in 0..32 {
        if used == bytes.len() {
            break;
        }
        match file.read(&mut bytes[used..]) {
            Ok(0) => {
                let (pid, group) = stat_group(&bytes[..used])?;
                let directory_pid = path
                    .parent()
                    .and_then(std::path::Path::file_name)
                    .and_then(|name| stat_number(name.as_encoded_bytes(), 1, i32::MAX.into()))
                    .ok_or_else(|| io::Error::other("invalid procfs PID directory"))?;
                if directory_pid != i128::from(pid) {
                    return Err(io::Error::other("procfs PID identity mismatch"));
                }
                return Ok(Some(group));
            }
            Ok(count) => used += count,
            Err(error) if gone(&error) => return Ok(None),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::other("unconfirmed bounded procfs stat read"))
}

fn stat_group(bytes: &[u8]) -> io::Result<(i32, i32)> {
    let invalid = || io::Error::other("invalid procfs group observation");
    if bytes.len() >= 4096 || bytes.contains(&0) {
        return Err(invalid());
    }
    let space = bytes
        .iter()
        .position(|byte| *byte == b' ')
        .ok_or_else(invalid)?;
    let pid = stat_number(&bytes[..space], 1, i32::MAX.into()).ok_or_else(invalid)?;
    if bytes.get(space + 1) != Some(&b'(') {
        return Err(invalid());
    }
    // comm can contain whitespace, ')' and non-UTF-8 bytes. only a full bounded
    // read through real EOF may establish its last ')' and the framed pgrp field.
    let end = bytes
        .iter()
        .rposition(|byte| *byte == b')')
        .ok_or_else(invalid)?;
    if end < space + 2 {
        return Err(invalid());
    }
    let tail = bytes[end + 1..]
        .strip_prefix(b" ")
        .and_then(|tail| tail.strip_suffix(b"\n"))
        .ok_or_else(invalid)?;
    let mut fields = tail.split(|byte| *byte == b' ');
    if !matches!(
        fields.next(),
        Some(b"R" | b"S" | b"D" | b"Z" | b"T" | b"t" | b"X" | b"x" | b"K" | b"W" | b"P" | b"I")
    ) {
        return Err(invalid());
    }
    // validate the complete Linux 52-field record before classifying even a
    // foreign group. truncated records and unknown future suffixes stay unknown;
    // neither a plausible pgrp nor a mocked prefix establishes a valid record.
    let mut group = 0;
    for index in 4..=52 {
        let (min, max) = match index {
            4..=6 | 20 => (0, i32::MAX.into()),
            7 | 8 | 38 | 39 | 52 => (i32::MIN.into(), i32::MAX.into()),
            9 | 40 | 41 => (0, u32::MAX.into()),
            16..=19 | 21 | 24 | 44 => (libc::c_long::MIN.into(), libc::c_long::MAX.into()),
            14 | 15 | 22 | 42 | 43 => (0, u64::MAX.into()),
            _ => (0, libc::c_ulong::MAX.into()),
        };
        let number =
            stat_number(fields.next().ok_or_else(invalid)?, min, max).ok_or_else(invalid)?;
        if index == 5 {
            group = number as i32;
        }
    }
    if fields.next().is_some() {
        return Err(invalid());
    }
    Ok((pid as i32, group))
}

fn stat_number(field: &[u8], min: i128, max: i128) -> Option<i128> {
    let digits = field.strip_prefix(b"-").unwrap_or(field);
    if digits.is_empty()
        || !digits.iter().all(u8::is_ascii_digit)
        || (digits.len() > 1 && digits[0] == b'0')
        || field == b"-0"
    {
        return None;
    }
    std::str::from_utf8(field)
        .ok()?
        .parse::<i128>()
        .ok()
        .filter(|value| (min..=max).contains(value))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(mode: &str) -> Process {
        let executable = std::env::current_exe().unwrap();
        let path = executable
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("examples/fake-plugin");
        assert!(path.is_file(), "build fake-plugin before filtered tests");
        let mut command = Command::new(path);
        command.arg(mode);
        Process::start(command).unwrap()
    }

    pub(super) fn until(mut check: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !check() {
            assert!(
                Instant::now() < deadline,
                "supervisor did not make progress"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    pub(super) fn live(pid: u32) -> bool {
        live_from_stat(pid, std::fs::read_to_string(format!("/proc/{pid}/stat")))
    }

    fn live_from_stat(pid: u32, stat: io::Result<String>) -> bool {
        match stat {
            Ok(stat) => !stat
                .rsplit_once(')')
                .unwrap()
                .1
                .trim_start()
                .starts_with('Z'),
            // procfs can open stat before the task vanishes, then return ESRCH
            // from the read. both outcomes mean this owned fixture is gone.
            Err(err)
                if err.kind() == io::ErrorKind::NotFound
                    || err.raw_os_error() == Some(libc::ESRCH) =>
            {
                false
            }
            Err(err) => panic!("cannot inspect owned fixture {pid}: {err}"),
        }
    }

    #[test]
    fn an_open_proc_stat_for_a_reaped_fixture_is_not_live() {
        let executable = std::env::current_exe().unwrap();
        let path = executable
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("examples/fake-plugin");
        let mut child = Command::new(path)
            .arg("echo")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .spawn()
            .unwrap();
        let pid = child.id();
        let mut stat = std::fs::File::open(format!("/proc/{pid}/stat")).unwrap();
        assert!(live(pid));
        child.kill().unwrap();
        child.wait().unwrap();
        let mut contents = String::new();
        let result = stat.read_to_string(&mut contents).map(|_| contents);
        assert_eq!(
            result.as_ref().unwrap_err().raw_os_error(),
            Some(libc::ESRCH)
        );
        assert!(!live_from_stat(pid, result));
        assert!(!live(pid));
    }

    #[test]
    #[should_panic(expected = "cannot inspect owned fixture")]
    fn unrelated_proc_stat_errors_still_fail_the_observer() {
        live_from_stat(0, Err(io::Error::from_raw_os_error(libc::EACCES)));
    }

    #[test]
    fn a_full_stdin_pipe_hits_the_real_write_deadline_off_loop() {
        let process = fixture("never-read");
        until(|| process.started());
        let request = Request::Event {
            event: super::super::protocol::EventKind::PaneOpened,
            pane: 1,
            detail: Some("x".repeat(60 * 1024)),
        };
        let start = Instant::now();
        for _ in 0..8 {
            process.send(&request).unwrap();
        }
        assert!(start.elapsed() < Duration::from_millis(500));
        let mut fault = None;
        until(|| {
            fault = process.failure();
            fault.is_some()
        });
        assert!(matches!(fault, Some(Failure::Fault(reason)) if reason.contains("two seconds")));
        until(|| process.finished());
        assert!(!live(process.pid()));
    }

    #[test]
    fn cancellation_kills_term_ignoring_descendants_and_inherited_pipes() {
        for mode in ["group-tree", "inherited"] {
            let process = fixture(mode);
            let mut descendant = None;
            until(|| {
                for frame in process.frames.try_iter() {
                    let value: serde_json::Value = serde_json::from_str(&frame).unwrap();
                    if value["type"] == "notify" {
                        descendant = value["message"].as_str().unwrap().parse::<u32>().ok();
                    }
                }
                descendant.is_some()
            });
            let child = descendant.unwrap();
            assert!(live(child));
            let start = Instant::now();
            if mode == "group-tree" {
                process.stop();
                assert!(start.elapsed() < Duration::from_millis(100));
            } // inherited's leader exits while its descendant retains stdout.
            until(|| process.finished());
            assert!(!live(process.pid()), "leader still live");
            assert!(!live(child), "descendant still live after {mode}");
        }
    }

    #[test]
    fn queue_and_encoded_frame_limits_are_enforced_before_sending() {
        let process = fixture("never-read");
        until(|| process.started());
        let mut frame = Request::Event {
            event: super::super::protocol::EventKind::PaneOpened,
            pane: 1,
            detail: Some("x".repeat(60 * 1024)),
        };
        let mut refused = false;
        for _ in 0..64 {
            if process.send(&frame).is_err() {
                refused = true;
                break;
            }
        }
        assert!(refused);
        assert!(process.outstanding.load(Ordering::Acquire) <= MAX_QUEUE_FRAMES);
        if let Request::Event { detail, .. } = &mut frame {
            *detail = Some("x".repeat(MAX_FRAME_BYTES));
        }
        // use a fresh generation to test the independent encoder
        // bound; temporary fullness must be distinguishable from a bad frame.
        let healthy = fixture("echo");
        assert_eq!(
            healthy.send(&frame),
            Err(SendError::Fault("host request exceeds 64 KiB"))
        );
        healthy.stop();
        until(|| healthy.finished());
        process.stop();
        until(|| process.finished());
    }

    #[test]
    fn pinned_shebang_execution_keeps_the_admitted_inode_after_a_path_is_replaced() {
        use std::os::unix::fs::PermissionsExt;
        let root = std::env::temp_dir()
            .join("opencode")
            .join(format!("among-pinned-{}", glib::uuid_string_random()));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
        let path = root.join("plugin");
        std::fs::write(
            &path,
            b"#!/bin/sh\nprintf '{\"type\":\"hello\",\"major\":1,\"minor\":0}\\n'\nread keep\n",
        )
        .unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
        let directory = super::super::data::owned_directory(&root).unwrap();
        let executable = super::super::data::contained_file(&directory, "plugin").unwrap();
        let program = pinned_program(directory, executable, None);
        std::fs::rename(&path, root.join("admitted")).unwrap();
        std::os::unix::fs::symlink("/bin/false", &path).unwrap();
        let process = Process::start(program).unwrap();
        let mut frame = None;
        until(|| {
            frame = process.frames.try_recv().ok().or(frame.take());
            frame.is_some()
        });
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&frame.unwrap()).unwrap()["type"],
            "hello"
        );
        process.stop();
        until(|| process.finished());
        std::fs::remove_dir_all(root).unwrap();
    }
}
