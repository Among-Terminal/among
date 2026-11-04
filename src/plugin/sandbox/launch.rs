//! applies the policy to one plugin process. everything that allocates or
//! reads the filesystem happens in the host before `fork`; the hook that runs
//! between `fork` and `exec` only performs system calls on prepared data.

use super::bpf::Program;
use super::level::{Probe, Protection};
use super::policy;
use super::status::{self, Enforcement, Report, Step};
use landlock::{
    Access, AccessFs, CompatLevel, Compatible, PathBeneath, PathFd, RestrictionStatus, Ruleset,
    RulesetAttr, RulesetCreated, RulesetCreatedAttr, RulesetStatus, Scope, ABI,
};
use std::collections::BTreeSet;
use std::fs::File;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::process::CommandExt;
use std::process::{Child, Command};
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// How long one start may take from `fork` through `exec`.
pub const START_TIMEOUT: Duration = Duration::from_secs(10);
const POLL_SLICE_MS: i32 = 10;
const REPORT_GRACE: Duration = Duration::from_secs(2);

/// Landlock ABI from which scoping of signals and abstract UNIX sockets exists.
const SCOPE_ABI: u32 = 6;

/// What the host passes in to confine one start.
pub struct Spec<'a> {
    /// The pinned installed generation: read and execute.
    pub generation: &'a File,
    /// The plugin's own data directory: read and write.
    pub data_dir: &'a File,
    /// Granted manifest features; they add read-only files.
    pub features: &'a BTreeSet<String>,
    /// The kernel's offer, as probed just before.
    pub probe: Probe,
    /// The least protection the child may end up with.
    pub required: Protection,
}

/// Why a confinement could not even be prepared. The plugin is not started.
#[derive(Debug)]
pub struct Refused(pub String);

impl std::fmt::Display for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// What a finished start proved about the child.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Applied {
    pub protection: Protection,
    pub report: Report,
    pub digest: String,
}

/// Why a start produced no confined process.
pub enum StartError {
    /// `spawn` itself failed: either a confinement step in the child
    /// (`confinement_failed`) or `exec` after a clean report (for example
    /// `ETXTBSY`, which the caller may retry).
    Failed {
        reason: String,
        errno: Option<i32>,
        confinement_failed: bool,
    },
    /// A process exists but its confinement is not proven; the caller must
    /// kill and reap exactly this child.
    Unproven { reason: String, child: Child },
}

/// A confinement prepared in the host for one start (and its `ETXTBSY` retries).
pub struct Confinement {
    required: Protection,
    abi: u32,
    digest: String,
    pipe_fd: Arc<AtomicI32>,
    timeout: Duration,
    parts: Option<PreparedParts>,
    #[cfg(test)]
    inject: Inject,
}

/// Failure injection for tests: each field makes the child misbehave at one
/// point so the host's handling of that case can be observed on a real process.
#[cfg(test)]
#[derive(Clone, Copy, Default)]
pub struct Inject {
    pub fail_step: Option<Step>,
    /// Stop after the first record, never reaching `exec`.
    pub hang: bool,
    /// Skip the final record but still exec.
    pub silent: bool,
    /// Write a short final record.
    pub short: bool,
    /// Write a complete final record, then fail instead of reaching `exec`.
    pub report_then_fail: bool,
    /// Abort on any allocation between `fork` and `exec`.
    pub forbid_allocation: bool,
}

impl Confinement {
    /// Builds the Landlock ruleset and the seccomp program. Allocates and
    /// opens files; call it in the host, never between `fork` and `exec`.
    pub fn prepare(spec: &Spec<'_>) -> Result<Self, Refused> {
        let abi = spec.probe.landlock_abi.unwrap_or(0);
        let ruleset = if abi >= 1 {
            Some(build_ruleset(spec, abi)?)
        } else {
            None
        };
        Ok(Self {
            required: spec.required,
            abi,
            digest: policy::digest(spec.features),
            pipe_fd: Arc::new(AtomicI32::new(-1)),
            timeout: START_TIMEOUT,
            parts: Some(PreparedParts {
                ruleset,
                program: Program::build(),
            }),
            #[cfg(test)]
            inject: Inject::default(),
        })
    }

    #[cfg(test)]
    pub fn with_inject(mut self, inject: Inject) -> Self {
        self.inject = inject;
        self
    }

    #[cfg(test)]
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Installs the confinement hook on `command`. `keep` is the descriptor
    /// of the executable, the only one above stderr that must survive `exec`.
    /// The hook runs after the hook that clears close-on-exec on that file.
    pub fn attach(&mut self, command: &mut Command, keep: RawFd) {
        let PreparedParts { ruleset, program } = self
            .parts
            .take()
            .expect("a confinement is attached to one command");
        let mut ruleset = ruleset;
        let pipe_fd = self.pipe_fd.clone();
        let abi = self.abi;
        let required = self.required;
        #[cfg(test)]
        let inject = self.inject;
        // the hook may run several times in different forked children; each
        // child owns its own copy of `ruleset`.
        unsafe {
            command.pre_exec(move || {
                #[cfg(test)]
                if inject.forbid_allocation {
                    allocation_guard::forbid();
                }
                let pipe = pipe_fd.load(Ordering::Relaxed);
                let pid = libc::getpid() as u32;
                write_all(pipe, &status::begin_record(pid.max(1)));
                #[cfg(test)]
                if inject.hang {
                    loop {
                        libc::pause();
                    }
                }
                let mut report = Report {
                    no_new_privs: false,
                    seccomp_loaded: false,
                    landlock_abi: 0,
                    enforcement: Enforcement::None,
                    failed: Step::None,
                    errno: 0,
                };
                let outcome = confine(
                    keep,
                    abi,
                    required,
                    &mut ruleset,
                    &program,
                    &mut report,
                    #[cfg(test)]
                    inject,
                );
                if let Err(errno) = outcome {
                    report.errno = errno as u32;
                    write_all(pipe, &report.to_bytes());
                    return Err(io::Error::from_raw_os_error(errno));
                }
                #[cfg(test)]
                if inject.silent {
                    return Ok(());
                }
                #[cfg(test)]
                if inject.short {
                    write_all(pipe, &report.to_bytes()[..status::RECORD_BYTES / 2]);
                    return Ok(());
                }
                write_all(pipe, &report.to_bytes());
                #[cfg(test)]
                if inject.report_then_fail {
                    return Err(io::Error::from_raw_os_error(libc::EACCES));
                }
                Ok(())
            });
        }
    }

    /// Starts the command. One bounded wait covers the whole start: a
    /// watchdog kills a child that has not reached `exec` within the timeout,
    /// which `Command::spawn` alone would wait for forever.
    pub fn spawn(&self, command: &mut Command) -> Result<(Child, Applied), StartError> {
        let (reader, writer) = pipe().map_err(|error| StartError::Failed {
            reason: format!("cannot create the confinement report pipe: {error}"),
            errno: error.raw_os_error(),
            confinement_failed: true,
        })?;
        self.pipe_fd.store(writer.as_raw_fd(), Ordering::Relaxed);
        let state = Arc::new(Mutex::new(Watch::default()));
        let (spawned, collected) = std::thread::scope(|scope| {
            let watcher = {
                let state = state.clone();
                let reader = reader.as_raw_fd();
                let timeout = self.timeout;
                scope.spawn(move || watch(reader, &state, timeout))
            };
            let spawned = command.spawn();
            // the child holds its own copy of the writer; dropping ours lets
            // the reader see the end of the stream when the child execs.
            drop(writer);
            self.pipe_fd.store(-1, Ordering::Relaxed);
            state.lock().expect("watch state").finished = true;
            (spawned, watcher.join().expect("watch thread"))
        });
        drop(reader);
        let Collected { bytes, killed } = collected;
        let begin = status::parse_begin(&bytes);
        let report = bytes
            .get(status::RECORD_BYTES..)
            .map(status::parse_final)
            .unwrap_or(Err(status::Invalid::Short));
        match spawned {
            Err(error) => {
                let failed_step = report
                    .as_ref()
                    .ok()
                    .filter(|report| report.failed != Step::None);
                Err(StartError::Failed {
                    reason: match failed_step {
                        Some(report) => format!("{}: {error}", report.failed.description()),
                        None if killed => {
                            "the plugin did not reach exec in time and was killed".into()
                        }
                        None => format!("the plugin could not be started: {error}"),
                    },
                    errno: error.raw_os_error(),
                    confinement_failed: failed_step.is_some() || killed,
                })
            }
            Ok(child) => {
                let verdict = begin
                    .map_err(|invalid| invalid.description())
                    .and_then(|pid| {
                        if pid != child.id() {
                            return Err("the confinement report came from another process");
                        }
                        let report = report.map_err(|invalid| invalid.description())?;
                        let protection = Protection::of_report(&report);
                        if !protection.satisfies(self.required) {
                            return Err(
                                "the plugin process did not reach the required protection level",
                            );
                        }
                        Ok(Applied {
                            protection,
                            report,
                            digest: self.digest.clone(),
                        })
                    });
                match verdict {
                    Ok(applied) => Ok((child, applied)),
                    Err(reason) => Err(StartError::Unproven {
                        reason: reason.into(),
                        child,
                    }),
                }
            }
        }
    }
}

/// The data the child hook needs; produced with the [`Confinement`].
pub struct PreparedParts {
    ruleset: Option<RulesetCreated>,
    program: Program,
}

fn build_ruleset(spec: &Spec<'_>, abi: u32) -> Result<RulesetCreated, Refused> {
    let refuse = |what: &str, error: &dyn std::fmt::Display| {
        Refused(format!(
            "cannot prepare the Landlock ruleset ({what}): {error}"
        ))
    };
    let all = AccessFs::from_all(ABI::V3);
    let mut ruleset = Ruleset::default()
        .set_compatibility(CompatLevel::BestEffort)
        .handle_access(all)
        .map_err(|error| refuse("handled access", &error))?;
    if abi >= SCOPE_ABI {
        ruleset = ruleset
            .scope(Scope::from_all(ABI::V6))
            .map_err(|error| refuse("scopes", &error))?;
    }
    let mut created = ruleset
        .create()
        .map_err(|error| refuse("creation", &error))?;
    let read = AccessFs::from_read(ABI::V3);
    // generation and data directory come in as already opened descriptors, so
    // nothing is looked up again by name after they were checked.
    created = created
        .add_rule(PathBeneath::new(spec.generation, read))
        .map_err(|error| refuse("installed package", &error))?;
    created = created
        .add_rule(PathBeneath::new(spec.data_dir, all & !AccessFs::Execute))
        .map_err(|error| refuse("data directory", &error))?;
    let granted = policy::grant_reads(spec.features);
    for path in policy::SYSTEM_READ.iter().copied().chain(granted) {
        let fd = match PathFd::new(path) {
            Ok(fd) => fd,
            // a layout without this path (no /lib64 on arm64, no timezone
            // file) simply has nothing to allow.
            Err(landlock::PathFdError::OpenCall { source, .. })
                if source.kind() == io::ErrorKind::NotFound =>
            {
                continue
            }
            Err(error) => return Err(refuse(path, &error)),
        };
        let is_directory = std::fs::metadata(path)
            .map(|meta| meta.is_dir())
            .unwrap_or(true);
        let access = if is_directory {
            read
        } else {
            AccessFs::ReadFile.into()
        };
        created = created
            .add_rule(PathBeneath::new(fd, access))
            .map_err(|error| refuse(path, &error))?;
    }
    Ok(created)
}

/// The confinement steps, in the order the child performs them. Returns the
/// errno of the first mandatory step that failed, with `report.failed` set.
fn confine(
    keep: RawFd,
    abi: u32,
    required: Protection,
    ruleset: &mut Option<RulesetCreated>,
    program: &Program,
    report: &mut Report,
    #[cfg(test)] inject: Inject,
) -> Result<(), i32> {
    // the executable stays inheritable; every other descriptor above stderr is
    // marked close-on-exec so only stdin, stdout, stderr and that file survive.
    #[cfg(test)]
    if inject.fail_step == Some(Step::Inheritance) {
        report.failed = Step::Inheritance;
        return Err(libc::EBADF);
    }
    if !close_on_exec_all_but(keep) {
        report.failed = Step::Inheritance;
        return Err(unsafe { *libc::__errno_location() });
    }
    #[cfg(test)]
    if inject.fail_step == Some(Step::NoNewPrivs) {
        report.failed = Step::NoNewPrivs;
        return Err(libc::EPERM);
    }
    if unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) } != 0 {
        report.failed = Step::NoNewPrivs;
        return Err(unsafe { *libc::__errno_location() });
    }
    report.no_new_privs = true;
    #[cfg(test)]
    if inject.fail_step == Some(Step::Landlock) {
        report.failed = Step::Landlock;
        return Err(libc::EACCES);
    }
    if let Some(ruleset) = ruleset.take() {
        match ruleset.restrict_self() {
            Ok(RestrictionStatus {
                ruleset: status, ..
            }) => {
                report.landlock_abi = abi.min(255) as u8;
                report.enforcement = match status {
                    RulesetStatus::FullyEnforced => Enforcement::Full,
                    RulesetStatus::PartiallyEnforced => Enforcement::Partial,
                    RulesetStatus::NotEnforced => Enforcement::None,
                };
            }
            Err(_) => {
                report.failed = Step::Landlock;
                return Err(libc::EPERM);
            }
        }
    }
    #[cfg(test)]
    if inject.fail_step == Some(Step::Seccomp) {
        report.failed = Step::Seccomp;
        return Err(libc::ENOSYS);
    }
    match program.load() {
        Ok(()) => report.seccomp_loaded = true,
        Err(errno) => {
            report.failed = Step::Seccomp;
            return Err(errno);
        }
    }
    // the last gate before `exec`: a process whose protection is below the
    // level it was allowed to run at never reaches the plugin's code.
    if !Protection::of_report(report).satisfies(required) {
        report.failed = Step::Level;
        return Err(libc::EPERM);
    }
    Ok(())
}

/// Marks every descriptor above stderr close-on-exec except `keep`.
/// Async-signal-safe: raw system calls only.
fn close_on_exec_all_but(keep: RawFd) -> bool {
    const CLOSE_RANGE_CLOEXEC: libc::c_uint = 4;
    let close_range = |first: u32, last: u32| unsafe {
        libc::syscall(libc::SYS_close_range, first, last, CLOSE_RANGE_CLOEXEC)
    };
    let keep = keep as u32;
    let ranges: [(u32, u32); 2] = [(3, keep.wrapping_sub(1)), (keep + 1, u32::MAX)];
    for (index, (first, last)) in ranges.into_iter().enumerate() {
        if index == 0 && keep <= 3 {
            continue;
        }
        if close_range(first, last) != 0 {
            return close_on_exec_by_scan(keep as RawFd);
        }
    }
    true
}

/// Fallback for kernels without `close_range` (before 5.11): walk the
/// descriptor table up to the soft limit.
fn close_on_exec_by_scan(keep: RawFd) -> bool {
    let mut limit = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    if unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut limit) } != 0 {
        return false;
    }
    let top = limit.rlim_cur.min(i32::MAX as u64) as RawFd;
    let mut descriptor = 3;
    while descriptor < top {
        if descriptor != keep {
            let flags = unsafe { libc::fcntl(descriptor, libc::F_GETFD) };
            if flags >= 0
                && unsafe { libc::fcntl(descriptor, libc::F_SETFD, flags | libc::FD_CLOEXEC) } != 0
            {
                return false;
            }
        }
        descriptor += 1;
    }
    true
}

/// Writes `bytes` to the status pipe, ignoring a failure: the host treats a
/// missing or short report as an unproven start, which is the safe outcome.
fn write_all(fd: RawFd, bytes: &[u8]) {
    if fd < 0 {
        return;
    }
    let mut done = 0;
    while done < bytes.len() {
        let written = unsafe { libc::write(fd, bytes[done..].as_ptr().cast(), bytes.len() - done) };
        if written < 0 {
            if unsafe { *libc::__errno_location() } == libc::EINTR {
                continue;
            }
            return;
        }
        done += written as usize;
    }
}

fn pipe() -> io::Result<(OwnedFd, OwnedFd)> {
    let mut fds = [0 as RawFd; 2];
    if unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) } != 0 {
        return Err(io::Error::last_os_error());
    }
    let (reader, writer) = unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) };
    let flags = unsafe { libc::fcntl(reader.as_raw_fd(), libc::F_GETFL) };
    if flags < 0
        || unsafe { libc::fcntl(reader.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok((reader, writer))
}

#[derive(Default)]
struct Watch {
    /// Set by the starting thread once `spawn` has returned.
    finished: bool,
}

struct Collected {
    bytes: Vec<u8>,
    /// The watchdog killed the child for exceeding the start timeout.
    killed: bool,
}

/// Reads the child's records while `spawn` blocks, and kills the child if the
/// start outlasts `timeout`. After `spawn` returns it reads until the stream
/// ends (the child's `exec` closed its copy) or a short grace expires.
fn watch(reader: RawFd, state: &Mutex<Watch>, timeout: Duration) -> Collected {
    let started = Instant::now();
    let mut bytes = Vec::with_capacity(2 * status::RECORD_BYTES);
    let mut pid = None;
    let mut killed = false;
    let mut finished_at: Option<Instant> = None;
    loop {
        let mut descriptor = libc::pollfd {
            fd: reader,
            events: libc::POLLIN,
            revents: 0,
        };
        unsafe { libc::poll(&mut descriptor, 1, POLL_SLICE_MS) };
        let mut buffer = [0u8; 64];
        // drain what is there; a zero-length read is the end of the stream,
        // a would-block error only means nothing more has arrived yet.
        let at_end = loop {
            let count = unsafe { libc::read(reader, buffer.as_mut_ptr().cast(), buffer.len()) };
            if count > 0 {
                let room = (2 * status::RECORD_BYTES).saturating_sub(bytes.len());
                bytes.extend_from_slice(&buffer[..(count as usize).min(room)]);
            } else {
                break count == 0;
            }
        };
        if pid.is_none() && bytes.len() >= status::RECORD_BYTES {
            pid = status::parse_begin(&bytes).ok();
        }
        let guard = state.lock().expect("watch state");
        if guard.finished {
            let since = *finished_at.get_or_insert_with(Instant::now);
            if at_end || since.elapsed() > REPORT_GRACE {
                return Collected { bytes, killed };
            }
        } else if !killed && started.elapsed() > timeout {
            // the lock excludes the starting thread's "finished" update, so the
            // process is still unreaped by this thread's spawn.
            if let Some(pid) = pid {
                unsafe { libc::kill(pid as libc::pid_t, libc::SIGKILL) };
                killed = true;
            }
        }
        drop(guard);
    }
}

#[cfg(test)]
pub(crate) mod allocation_guard {
    //! a global allocator that aborts when the forked child allocates between
    //! `fork` and `exec`: such an allocation could deadlock on a lock held by a
    //! thread that does not exist in the child.

    use std::alloc::{GlobalAlloc, Layout, System};
    use std::sync::atomic::{AtomicBool, Ordering};

    static FORBIDDEN: AtomicBool = AtomicBool::new(false);

    pub struct Guard;

    unsafe impl GlobalAlloc for Guard {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            if FORBIDDEN.load(Ordering::Relaxed) {
                // the message is a static slice: formatting would allocate.
                let message = b"allocation between fork and exec\n";
                unsafe {
                    libc::write(2, message.as_ptr().cast(), message.len());
                    libc::_exit(101);
                }
            }
            unsafe { System.alloc(layout) }
        }
        unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
            if FORBIDDEN.load(Ordering::Relaxed) {
                let message = b"deallocation between fork and exec\n";
                unsafe {
                    libc::write(2, message.as_ptr().cast(), message.len());
                    libc::_exit(101);
                }
            }
            unsafe { System.dealloc(pointer, layout) }
        }
        unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, size: usize) -> *mut u8 {
            if FORBIDDEN.load(Ordering::Relaxed) {
                let message = b"reallocation between fork and exec\n";
                unsafe {
                    libc::write(2, message.as_ptr().cast(), message.len());
                    libc::_exit(101);
                }
            }
            unsafe { System.realloc(pointer, layout, size) }
        }
    }

    /// Called in the forked child only: the flag is that process's own copy.
    pub fn forbid() {
        FORBIDDEN.store(true, Ordering::Relaxed);
    }
}
