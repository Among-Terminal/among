//! one cancellable worker owns spawn, nonblocking pipes and reaping. retaining
//! its handle until exit prevents rapid toggles from accumulating generations.

use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};
use std::sync::mpsc::{sync_channel, Receiver, SyncSender, TrySendError};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use super::admission::Lease;
use super::protocol::Request;
use super::transport::{FrameDecoder, MAX_FRAME_BYTES, MAX_QUEUE_FRAMES};

pub const WRITE_TIMEOUT: Duration = Duration::from_secs(2);
const STOP_GRACE: Duration = Duration::from_millis(500);
const IO_INTERVAL_MS: i32 = 20;
const SPAWN_BUSY_TIMEOUT: Duration = Duration::from_secs(1);

#[cfg(test)]
mod spawn_tests;

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
    admission: Arc<Lease>,
    #[cfg(test)]
    proof_control: Arc<ProofControl>,
}

impl Process {
    #[cfg(test)]
    pub fn start(command: Command) -> io::Result<Self> {
        Self::start_admitted(command, Arc::new(Lease::new()))
    }

    pub(super) fn start_admitted(mut command: Command, admission: Arc<Lease>) -> io::Result<Self> {
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
        let worker_admission = admission.clone();
        #[cfg(test)]
        let proof_control = Arc::new(ProofControl::default());
        #[cfg(test)]
        let worker_proof_control = proof_control.clone();
        // spawn the worker before the child, so a thread-creation failure cannot
        // strand a child or force a GUI-thread wait in the error path.
        let worker = std::thread::Builder::new()
            .name("among-plugin-supervisor".into())
            .spawn(move || {
                let _held_admission = &worker_admission;
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
                terminate_and_reap(
                    &mut child,
                    &worker_admission,
                    #[cfg(test)]
                    &worker_proof_control,
                );
            })?;
        Ok(Self {
            frames,
            failures,
            input,
            outstanding,
            cancel,
            pid,
            worker,
            admission,
            #[cfg(test)]
            proof_control,
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

    pub fn retirement_reason(&self) -> Option<&'static str> {
        self.admission.reason()
    }

    #[cfg(test)]
    pub(crate) fn hold_group_proof(&self, errno: Option<i32>) -> ProofHold {
        self.proof_control.observations.store(0, Ordering::Release);
        self.proof_control
            .mode
            .store(errno.unwrap_or(-1), Ordering::Release);
        ProofHold(self.proof_control.clone())
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

fn terminate_and_reap(
    child: &mut Child,
    admission: &Lease,
    #[cfg(test)] proof_control: &ProofControl,
) {
    // process_group(0) makes this owned child's PID its group ID. do not claim
    // control of a descendant that deliberately escapes the supervised group.
    let pgid = i32::try_from(child.id()).ok().filter(|pid| *pid > 1);
    if let Some(pid) = pgid {
        unsafe {
            libc::kill(-pid, libc::SIGTERM);
        }
        std::thread::sleep(STOP_GRACE);
        unsafe {
            libc::kill(-pid, libc::SIGKILL);
        }
    } else {
        let _ = child.kill();
    }
    // an uninterruptible child holds this worker/generation, not the GUI. the
    // caller must not replace it until this function really observes reaping.
    admission.reaping();
    while child.wait().is_err() {
        std::thread::sleep(Duration::from_millis(100));
    }
    // Never deliver another signal after reap: the numeric group ID could now
    // be recycled. Signal zero is a read-only existence/permission query. Only
    // ESRCH proves absence; success and every other error retain this generation.
    // Unlike a procfs scan this cannot omit hidden group members. Zombies may
    // delay absence, but only this identity and one shared budget slot are held.
    loop {
        let result = match pgid {
            Some(pgid) => group_absent(pgid),
            None => Err(io::Error::from_raw_os_error(libc::EINVAL)),
        };
        #[cfg(test)]
        let result = proof_control.observe(result);
        if matches!(result, Ok(true)) {
            break;
        }
        admission.pending(&result);
        std::thread::sleep(Duration::from_millis(IO_INTERVAL_MS as u64));
    }
}

fn group_absent(pgid: i32) -> io::Result<bool> {
    if pgid <= 1 {
        return Err(io::Error::from_raw_os_error(libc::EINVAL));
    }
    let result = unsafe { libc::kill(-pgid, 0) };
    let error = (result < 0).then(io::Error::last_os_error);
    group_absent_result(result, error)
}

fn group_absent_result(result: i32, error: Option<io::Error>) -> io::Result<bool> {
    if result == 0 {
        return Ok(false);
    }
    let error = error.unwrap_or_else(|| io::Error::other("invalid process-group query result"));
    if error.raw_os_error() == Some(libc::ESRCH) {
        Ok(true)
    } else {
        Err(error)
    }
}

#[cfg(test)]
#[derive(Default)]
struct ProofControl {
    mode: std::sync::atomic::AtomicI32,
    observations: AtomicUsize,
}

#[cfg(test)]
impl ProofControl {
    fn observe(&self, real: io::Result<bool>) -> io::Result<bool> {
        let mode = self.mode.load(Ordering::Acquire);
        if mode == 0 {
            return real;
        }
        self.observations.fetch_add(1, Ordering::AcqRel);
        if mode < 0 {
            Ok(false)
        } else {
            Err(io::Error::from_raw_os_error(mode))
        }
    }
}

#[cfg(test)]
pub(crate) struct ProofHold(Arc<ProofControl>);

#[cfg(test)]
impl ProofHold {
    pub(crate) fn observations(&self) -> usize {
        self.0.observations.load(Ordering::Acquire)
    }
}

#[cfg(test)]
impl Drop for ProofHold {
    fn drop(&mut self) {
        // Releasing the control restores real inspection; it never manufactures
        // absence, including when another assertion unwinds the test.
        self.0.mode.store(0, Ordering::Release);
    }
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

    fn until(mut check: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !check() {
            assert!(
                Instant::now() < deadline,
                "supervisor did not make progress"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn live(pid: u32) -> bool {
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
    fn group_absence_requires_kernel_esrch_not_success_or_an_inspection_error() {
        assert!(!group_absent_result(0, None).unwrap());
        assert!(group_absent_result(-1, Some(io::Error::from_raw_os_error(libc::ESRCH))).unwrap());
        for errno in [
            libc::EPERM,
            libc::EACCES,
            libc::EINTR,
            libc::EIO,
            libc::EINVAL,
        ] {
            assert_eq!(
                group_absent_result(-1, Some(io::Error::from_raw_os_error(errno)))
                    .unwrap_err()
                    .raw_os_error(),
                Some(errno)
            );
        }
        for pgid in [-1, 0, 1] {
            assert_eq!(
                group_absent(pgid).unwrap_err().raw_os_error(),
                Some(libc::EINVAL)
            );
        }
    }

    #[test]
    fn proof_pending_or_denied_after_real_leader_reap_is_not_finished() {
        for errno in [None, Some(libc::EPERM), Some(libc::EACCES), Some(libc::EIO)] {
            let process = fixture("stubborn");
            until(|| process.started());
            let pending = process.hold_group_proof(errno);
            process.stop();
            // These are repeated nonblocking Pending results, not a barrier
            // that hides a worker which ignores an inspection result.
            until(|| pending.observations() >= 2);
            assert!(
                !live(process.pid()),
                "real leader was not reaped before the proof probes"
            );
            assert!(
                !process.finished(),
                "pending/error was treated as retirement proof"
            );
            assert!(process.retirement_reason().is_some());
            drop(pending);
            until(|| process.finished());
            assert!(!live(process.pid()));
        }
    }

    #[test]
    fn a_non_live_unreaped_zombie_group_is_conservatively_present() {
        let executable = std::env::current_exe()
            .unwrap()
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("examples/fake-plugin");
        let mut child = Command::new(executable)
            .arg("crashing")
            .process_group(0)
            .stdout(Stdio::null())
            .spawn()
            .unwrap();
        let pid = child.id();
        until(|| child_exited(pid).unwrap());
        let non_live = !live(pid);
        let before_reap = group_absent(i32::try_from(pid).unwrap());
        // Reap before assertions, even if a platform observation is surprising.
        child.wait().unwrap();
        assert!(non_live);
        assert!(
            !before_reap.unwrap(),
            "a zombie is not an empty kernel group"
        );
        assert!(group_absent(i32::try_from(pid).unwrap()).unwrap());
    }

    #[test]
    fn proof_pending_worker_retains_the_actual_package_lease_after_process_drop() {
        use std::os::unix::fs::PermissionsExt;
        let root = std::env::temp_dir().join(format!(
            "among-retirement-lease-{}",
            glib::uuid_string_random()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let script = root.join("plugin");
        std::fs::write(&script, b"#!/bin/sh\nread keep\n").unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
        let lease_path = root.join("lease");
        let lease = Arc::new(std::fs::File::create(&lease_path).unwrap());
        assert_eq!(
            unsafe { libc::flock(lease.as_raw_fd(), libc::LOCK_SH | libc::LOCK_NB) },
            0
        );
        let contender = std::fs::File::open(&lease_path).unwrap();
        let command = pinned_program(
            std::fs::File::open(&root).unwrap(),
            std::fs::File::open(&script).unwrap(),
            Some(lease.clone()),
        );
        drop(lease);
        let process = Process::start(command).unwrap();
        let hold = process.hold_group_proof(None);
        until(|| process.started());
        process.stop();
        until(|| hold.observations() >= 2);
        assert!(!process.finished());
        assert_ne!(
            unsafe { libc::flock(contender.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) },
            0
        );
        drop(process);
        assert_ne!(
            unsafe { libc::flock(contender.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) },
            0,
            "detached worker lost its real package lease"
        );
        drop(hold);
        until(|| unsafe { libc::flock(contender.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0);
        std::fs::remove_dir_all(root).unwrap();
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
