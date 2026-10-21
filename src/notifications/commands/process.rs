//! fresh exec isolates subreaping and signal handling from GTK and plugin children.

use std::ffi::CString;
use std::fs::File;
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::Path;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use super::protocol::{Report, Request, MAX_CONTROL};
use super::{Outcome, Reason, CLEANUP_DEADLINE, CLEANUP_GRACE, DEADLINE, MAX_OUTPUT};

static STOP: AtomicBool = AtomicBool::new(false);
pub(super) const INTERVAL: Duration = Duration::from_millis(10);

pub(super) struct ExactExec {
    program: CString,
    arguments: Vec<CString>,
    environment: Vec<CString>,
    argv: Vec<*const libc::c_char>,
    envp: Vec<*const libc::c_char>,
}

// pointers refer only to this frame's owned, immutable CString allocations.
// moving the frame moves their headers, never the pointed-to allocations.
unsafe impl Send for ExactExec {}
unsafe impl Sync for ExactExec {}

impl ExactExec {
    pub fn new(program: &Path, arguments: &[String], home: &Path) -> io::Result<Self> {
        let invalid = || io::Error::other("invalid exact exec input");
        let program = CString::new(program.as_os_str().as_bytes()).map_err(|_| invalid())?;
        let mut argv_owned = vec![program.clone()];
        for argument in arguments {
            argv_owned.push(CString::new(argument.as_bytes()).map_err(|_| invalid())?);
        }
        let mut home_variable = b"HOME=".to_vec();
        home_variable.extend_from_slice(home.as_os_str().as_bytes());
        let environment = vec![
            CString::new(b"PATH=/usr/bin:/bin".as_slice()).map_err(|_| invalid())?,
            CString::new(b"LANG=C.UTF-8".as_slice()).map_err(|_| invalid())?,
            CString::new(home_variable).map_err(|_| invalid())?,
        ];
        let mut argv: Vec<_> = argv_owned
            .iter()
            .map(|argument| argument.as_ptr())
            .collect();
        let mut envp: Vec<_> = environment
            .iter()
            .map(|variable| variable.as_ptr())
            .collect();
        argv.push(std::ptr::null());
        envp.push(std::ptr::null());
        Ok(Self {
            program,
            arguments: argv_owned,
            environment,
            argv,
            envp,
        })
    }

    pub fn execute(&self) -> io::Result<()> {
        let _keep_allocations_alive = (&self.arguments, &self.environment);
        // execvp may interpret ENOEXEC through /bin/sh. bypass that fallback,
        // with argv/envp completely prepared before fork (no child allocations).
        unsafe {
            libc::execve(
                self.program.as_ptr(),
                self.argv.as_ptr(),
                self.envp.as_ptr(),
            );
        }
        Err(io::Error::last_os_error())
    }
}

extern "C" fn stop(_: i32) {
    STOP.store(true, Ordering::Relaxed);
}

pub(super) fn nonblocking(fd: RawFd) -> io::Result<()> {
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn pause() {
    std::thread::sleep(INTERVAL);
}

fn read_exact_until(reader: &mut impl Read, bytes: &mut [u8], end: Instant) -> io::Result<()> {
    let mut read = 0;
    while read < bytes.len() {
        if Instant::now() >= end || STOP.load(Ordering::Relaxed) {
            return Err(io::Error::other("supervisor input stopped"));
        }
        match reader.read(&mut bytes[read..]) {
            Ok(0) => return Err(io::ErrorKind::UnexpectedEof.into()),
            Ok(count) => read += count,
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                ) =>
            {
                pause()
            }
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

fn report(outcome: Outcome, output_bytes: usize, reaped: bool) {
    // only one small fixed-schema result; child pipes never reach these handles.
    let result = Report {
        outcome,
        output_bytes,
        reaped,
    };
    if let Ok(bytes) = serde_json::to_vec(&result) {
        let mut output = io::stdout().lock();
        let _ = output.write_all(&(bytes.len() as u32).to_le_bytes());
        let _ = output.write_all(&bytes);
        let _ = output.flush();
    }
}

pub(super) fn supervise() -> u8 {
    STOP.store(false, Ordering::Relaxed);
    let mut input = io::stdin().lock();
    if nonblocking(input.as_raw_fd()).is_err() {
        return 2;
    }
    let end = Instant::now() + DEADLINE;
    let mut length = [0; 4];
    if read_exact_until(&mut input, &mut length, end).is_err() {
        return 2;
    }
    let length = u32::from_le_bytes(length) as usize;
    if length > MAX_CONTROL {
        return 2;
    }
    let mut bytes = vec![0; length];
    if read_exact_until(&mut input, &mut bytes, end).is_err() {
        return 2;
    }
    let request = match serde_json::from_slice::<Request>(&bytes) {
        Ok(request) if request.config.validate().is_ok() => request,
        _ => return 2,
    };
    let payload = match request.payload.encode() {
        Ok(payload) => payload,
        Err(_) => return 2,
    };
    // process-local only: never make the GTK/plugin host a global subreaper.
    if unsafe { libc::prctl(libc::PR_SET_CHILD_SUBREAPER, 1, 0, 0, 0) } != 0 {
        return 2;
    }
    unsafe {
        let mut action: libc::sigaction = std::mem::zeroed();
        action.sa_sigaction = stop as *const () as usize;
        libc::sigemptyset(&mut action.sa_mask);
        for signal in [libc::SIGTERM, libc::SIGINT, libc::SIGHUP] {
            if libc::sigaction(signal, &action, std::ptr::null_mut()) != 0 {
                return 2;
            }
        }
    }
    if io::stdout()
        .write_all(b"R")
        .and_then(|()| io::stdout().flush())
        .is_err()
    {
        return 2;
    }
    let mut permit = [0];
    if read_exact_until(&mut input, &mut permit, end).is_err() || permit != *b"G" {
        report(Outcome::NotStarted(Reason::Revoked), 0, true);
        return 0;
    }
    if STOP.load(Ordering::Relaxed) {
        report(Outcome::NotStarted(Reason::Cancelled), 0, true);
        return 0;
    }
    let mut command = Command::new(&request.config.executable);
    let cwd = match std::env::current_dir() {
        Ok(cwd) => cwd,
        Err(_) => {
            report(Outcome::NotStarted(Reason::SpawnFailed), 0, true);
            return 0;
        }
    };
    let exact = match ExactExec::new(
        Path::new(&request.config.executable),
        &request.config.arguments,
        &cwd,
    ) {
        Ok(exact) => exact,
        Err(_) => {
            report(Outcome::NotStarted(Reason::SpawnFailed), 0, true);
            return 0;
        }
    };
    command
        .args(&request.config.arguments)
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("LANG", "C.UTF-8")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);
    command.env("HOME", cwd);
    let parent = unsafe { libc::getpid() };
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
            if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL, 0, 0, 0) != 0 {
                return Err(io::Error::last_os_error());
            }
            if libc::getppid() != parent {
                libc::_exit(125);
            }
            exact.execute()
        });
    }
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(_) => {
            report(Outcome::NotStarted(Reason::SpawnFailed), 0, true);
            return 0;
        }
    };
    let (reason, output_bytes) = pump(&mut child, &payload, &mut input, end);
    let mut cleanup = Cleanup::new(child.id());
    let cleanup_started = Instant::now();
    let mut reported = false;
    loop {
        if cleanup.poll() {
            if !reported {
                let outcome = match reason {
                    Some(reason) => Outcome::Uncertain(reason),
                    None => match cleanup.status {
                        Some(status) => match status.code() {
                            Some(code) => Outcome::Exited(code),
                            None => Outcome::Signaled(status.signal().unwrap_or(0)),
                        },
                        None => Outcome::Uncertain(Reason::SupervisorLost),
                    },
                };
                report(outcome, output_bytes, true);
            }
            return 0;
        }
        if !reported && cleanup_started.elapsed() >= CLEANUP_GRACE + CLEANUP_DEADLINE {
            report(
                Outcome::Uncertain(Reason::CleanupIncomplete),
                output_bytes,
                false,
            );
            reported = true;
        }
        // an uninterruptible descendant retains this isolated supervisor and its
        // global reservation, not a blocked GUI or an accumulating job thread.
        pause();
    }
}

fn leader_exited(pid: u32) -> io::Result<bool> {
    let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
    // keep the leader PID reserved until the final process-group signal.
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

fn pump(
    child: &mut Child,
    payload: &[u8],
    control: &mut impl Read,
    end: Instant,
) -> (Option<Reason>, usize) {
    let (Some(stdin), Some(mut stdout), Some(mut stderr)) =
        (child.stdin.take(), child.stdout.take(), child.stderr.take())
    else {
        return (Some(Reason::PipeIo), 0);
    };
    for fd in [stdin.as_raw_fd(), stdout.as_raw_fd(), stderr.as_raw_fd()] {
        if nonblocking(fd).is_err() {
            return (Some(Reason::PipeIo), 0);
        }
    }
    let mut stdin = Some(stdin);
    let mut written = 0;
    let mut output = 0;
    let mut buffer = [0; 4096];
    loop {
        if STOP.load(Ordering::Relaxed) {
            return (Some(Reason::Cancelled), output);
        }
        if Instant::now() >= end {
            return (Some(Reason::Deadline), output);
        }
        let mut cancel = [0];
        match control.read(&mut cancel) {
            Ok(_) => return (Some(Reason::Cancelled), output), // EOF also revokes ownership
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                ) => {}
            Err(_) => return (Some(Reason::PipeIo), output),
        }
        if let Some(pipe) = stdin.as_mut() {
            match pipe.write(&payload[written..]) {
                Ok(0) => return (Some(Reason::PipeIo), output),
                Ok(count) => {
                    written += count;
                    if written == payload.len() {
                        stdin.take();
                    } // actual EOF
                }
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                    ) => {}
                Err(_) => return (Some(Reason::PipeIo), output),
            }
        }
        let exited = match leader_exited(child.id()) {
            Ok(exited) => exited,
            Err(_) => return (Some(Reason::PipeIo), output),
        };
        let mut drained = true;
        for reader in [&mut stdout as &mut dyn Read, &mut stderr as &mut dyn Read] {
            // finite passes preserve deadline/cancellation progress under a flood.
            for _ in 0..8 {
                let maximum = buffer.len().min(MAX_OUTPUT - output + 1);
                match reader.read(&mut buffer[..maximum]) {
                    Ok(0) => break,
                    Ok(count) => {
                        if count > MAX_OUTPUT - output {
                            return (Some(Reason::OutputLimit), MAX_OUTPUT);
                        }
                        output += count;
                        drained = false;
                    }
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                    Err(_) => return (Some(Reason::PipeIo), output),
                }
            }
        }
        // drain bytes already written by a fast exiting leader before success;
        // inherited open pipes do not keep a completed leader alive indefinitely.
        if exited && drained {
            return (None, output);
        }
        pause();
    }
}

struct Cleanup {
    leader: u32,
    started: Instant,
    killed: bool,
    status: Option<ExitStatus>,
}

impl Cleanup {
    fn new(leader: u32) -> Self {
        unsafe {
            libc::kill(-(leader as i32), libc::SIGTERM);
        }
        Self {
            leader,
            started: Instant::now(),
            killed: false,
            status: None,
        }
    }

    fn poll(&mut self) -> bool {
        if !self.killed {
            if self.started.elapsed() < CLEANUP_GRACE {
                return false;
            }
            // no more group signals after reaping the leader: its ID could be reused.
            unsafe {
                libc::kill(-(self.leader as i32), libc::SIGKILL);
            }
            self.killed = true;
        }
        // subreaping is scoped to this fresh helper. only it reaps these PIDs,
        // so direct-child identities remain reserved until our waitpid below.
        let path = format!("/proc/self/task/{}/children", std::process::id());
        if let Ok(file) = File::open(path) {
            let mut bytes = String::new();
            if file.take(64 * 1024).read_to_string(&mut bytes).is_ok() {
                for pid in bytes
                    .split_whitespace()
                    .filter_map(|pid| pid.parse::<i32>().ok())
                {
                    if pid > 0 {
                        unsafe {
                            libc::kill(pid, libc::SIGKILL);
                        }
                    }
                }
            }
        }
        for _ in 0..256 {
            let mut status = 0;
            let pid = unsafe { libc::waitpid(-1, &mut status, libc::WNOHANG) };
            if pid == self.leader as i32 {
                self.status = Some(ExitStatus::from_raw(status));
            }
            if pid == 0 {
                return false;
            }
            if pid < 0 {
                return io::Error::last_os_error().raw_os_error() == Some(libc::ECHILD);
            }
        }
        false
    }
}
