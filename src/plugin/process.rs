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

pub fn pinned_program(directory: std::fs::File, executable: std::fs::File) -> Command {
    let mut program = Command::new(format!("/proc/self/fd/{}", executable.as_raw_fd()));
    program.current_dir(format!("/proc/self/fd/{}", directory.as_raw_fd()));
    // pin both descriptors through fork/exec. only the child's executable fd
    // loses CLOEXEC so a shebang interpreter can open that exact admitted file;
    // changing descriptor flags in pre_exec does not leak it from the host.
    unsafe {
        program.pre_exec(move || {
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
    while child.wait().is_err() {
        std::thread::sleep(Duration::from_millis(100));
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
        match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
            Ok(stat) => !stat
                .rsplit_once(')')
                .unwrap()
                .1
                .trim_start()
                .starts_with('Z'),
            Err(err) if err.kind() == io::ErrorKind::NotFound => false,
            Err(err) => panic!("cannot inspect owned fixture {pid}: {err}"),
        }
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
        let program = pinned_program(directory, executable);
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
