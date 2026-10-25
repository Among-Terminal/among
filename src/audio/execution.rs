//! one process-wide decoder slot. GTK never constructs or retires a decoder in
//! the caller; uncertain cleanup quarantines this slot instead of spawning more.
use super::{authorization::Authorization, Sound};
use crate::notifications::{EffectAuthorization, ProcessEffectAuthorization};
use std::{
    fs::File,
    io::{Read, Write},
    os::{
        fd::{AsRawFd, FromRawFd, OwnedFd},
        unix::process::CommandExt,
    },
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{self, SyncSender},
        Arc, Mutex, OnceLock,
    },
    time::{Duration, Instant},
};

pub(crate) const DEADLINE: Duration = Duration::from_secs(13);
const CONFIRM: Duration = Duration::from_millis(500);
const CLEANUP_GRACE: Duration = Duration::from_millis(250);
const MAX_MAPS: u64 = 512 * 1024;
const MAX_PACKET: usize = 1024 * 1024;
const MAX_STDERR: usize = 4096;
const FLAG: &str = "--among-audio-decoder-v2";
static ROOT: OnceLock<Option<PathBuf>> = OnceLock::new();
static BUSY: AtomicBool = AtomicBool::new(false);
static SHUTTING_DOWN: AtomicBool = AtomicBool::new(false);
static WORKER: OnceLock<Result<SyncSender<Job>, String>> = OnceLock::new();
static CURRENT_AUTHORITY: Mutex<Option<Arc<Authorization>>> = Mutex::new(None);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecoderPhase {
    Launching,
    Constructing,
    Playing,
    Retiring,
    Quarantined,
    Finished,
}

#[derive(Debug, Clone)]
pub struct DecoderSnapshot {
    pub id: u64,
    pub pid: Option<u32>,
    pub phase: DecoderPhase,
    pub maps: Arc<str>,
    pub stderr: Arc<str>,
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum Outcome {
    Ended,
    Failed(&'static str),
    Cancelled,
}
struct Report {
    snapshot: DecoderSnapshot,
    outcome: Option<Outcome>,
}
pub(crate) struct Operation {
    cancel: AtomicBool,
    authority: Arc<Authorization>,
    effect: Option<EffectAuthorization>,
    cancelled_micros: AtomicU64,
    started: Instant,
    report: Mutex<Report>,
}
impl Operation {
    fn permitted(&self) -> bool {
        !self.cancel.load(Ordering::Acquire)
            && !SHUTTING_DOWN.load(Ordering::Acquire)
            && self
                .effect
                .as_ref()
                .is_none_or(|effect| effect.is_current(Instant::now()))
    }
    pub(crate) fn cancel(&self) {
        // shared revocation wins even while the supervisor is inside spawn.
        self.authority.revoke();
        let elapsed = (self
            .started
            .elapsed()
            .as_micros()
            .min((u64::MAX - 1) as u128) as u64)
            + 1;
        let _ =
            self.cancelled_micros
                .compare_exchange(0, elapsed, Ordering::AcqRel, Ordering::Relaxed);
        self.cancel.store(true, Ordering::Release);
    }
    pub(crate) fn snapshot(&self) -> Option<DecoderSnapshot> {
        self.report.try_lock().ok().map(|report| {
            let mut snapshot = report.snapshot.clone();
            let cancelled = self.cancelled_micros.load(Ordering::Acquire);
            // Also expose uncertainty if the sole supervisor is itself stuck in
            // an OS spawn/proc call. It still owns the slot; readers never wait.
            if snapshot.phase != DecoderPhase::Finished
                && (self.started.elapsed() >= DEADLINE + CONFIRM
                    || cancelled > 0
                        && self.started.elapsed() >= Duration::from_micros(cancelled - 1) + CONFIRM)
            {
                snapshot.phase = DecoderPhase::Quarantined;
            }
            snapshot
        })
    }
    pub(crate) fn outcome(&self) -> Option<Outcome> {
        self.report
            .try_lock()
            .ok()
            .and_then(|report| report.outcome)
    }
    fn update(&self, edit: impl FnOnce(&mut Report)) {
        // only the single supervisor writes; GUI readers never wait for this lock.
        if let Ok(mut report) = self.report.lock() {
            edit(&mut report);
        }
    }
}
struct Job {
    operation: Arc<Operation>,
    sound: Sound,
    started: Instant,
    process_effect: Option<ProcessEffectAuthorization>,
}
struct Slot;
impl Drop for Slot {
    fn drop(&mut self) {
        let retired = CURRENT_AUTHORITY
            .lock()
            .ok()
            .and_then(|mut current| current.take());
        drop(retired);
        BUSY.store(false, Ordering::Release);
    }
}

pub fn initialize(root: Option<&Path>) -> Result<(), String> {
    let root = root
        .map(Path::canonicalize)
        .transpose()
        .map_err(|_| "Audio payload root is unavailable.")?;
    ROOT.set(root)
        .map_err(|_| "Audio execution was already initialized.")?;
    worker().map(|_| ())
}
fn worker() -> Result<&'static SyncSender<Job>, String> {
    WORKER
        .get_or_init(|| {
            let (send, receive) = mpsc::sync_channel::<Job>(1);
            std::thread::Builder::new()
                .name("among-audio-supervisor".into())
                .spawn(move || {
                    for job in receive {
                        let _slot = Slot;
                        let result = supervise(&job);
                        job.operation.update(|report| {
                            report.snapshot.phase = DecoderPhase::Finished;
                            report.outcome = Some(result);
                        });
                    }
                })
                .map_err(|_| "Could not start the audio supervisor.".to_string())?;
            Ok(send)
        })
        .as_ref()
        .map_err(Clone::clone)
}

pub(crate) fn start(
    id: u64,
    sound: Sound,
    effect: Option<EffectAuthorization>,
) -> Result<Option<Arc<Operation>>, &'static str> {
    if SHUTTING_DOWN.load(Ordering::Acquire) {
        return Err("Audio execution has been shut down.");
    }
    if effect
        .as_ref()
        .is_some_and(|effect| !effect.is_current(Instant::now()))
    {
        return Err("The audio event authority is no longer current.");
    }
    let sender = worker().map_err(|_| "The audio supervisor is unavailable.")?;
    if BUSY
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Relaxed)
        .is_err()
    {
        return Ok(None);
    }
    let started = Instant::now();
    let process_effect = match effect
        .as_ref()
        .map(EffectAuthorization::export_process)
        .transpose()
    {
        Ok(exported) => exported,
        Err(_) => {
            BUSY.store(false, Ordering::Release);
            return Err("The audio event authority could not be exported.");
        }
    };
    let authorization = match &process_effect {
        Some(effect) => Authorization::new_bound(id, effect.binding()),
        None => Authorization::new(id),
    };
    let authority = match authorization {
        Ok(authority) => Arc::new(authority),
        Err(_) => {
            BUSY.store(false, Ordering::Release);
            return Err("The audio start authority is unavailable.");
        }
    };
    // GTK never waits for a registry lock or a helper handshake. The supervisor
    // clears this entry before releasing capacity; the exit guard only try-locks.
    if let Ok(mut current) = CURRENT_AUTHORITY.try_lock() {
        *current = Some(authority.clone());
    } else {
        BUSY.store(false, Ordering::Release);
        return Err("The audio start authority registry is unavailable.");
    }
    let operation = Arc::new(Operation {
        cancel: AtomicBool::new(false),
        authority,
        effect,
        cancelled_micros: AtomicU64::new(0),
        started,
        report: Mutex::new(Report {
            snapshot: DecoderSnapshot {
                id,
                pid: None,
                phase: DecoderPhase::Launching,
                maps: Arc::from(""),
                stderr: Arc::from(""),
            },
            outcome: None,
        }),
    });
    if sender
        .try_send(Job {
            operation: operation.clone(),
            sound,
            started,
            process_effect,
        })
        .is_err()
    {
        operation.authority.revoke();
        if let Ok(mut current) = CURRENT_AUTHORITY.try_lock() {
            current.take();
        }
        BUSY.store(false, Ordering::Release);
        return Err("The audio supervisor did not accept this operation.");
    }
    Ok(Some(operation))
}

fn nonblocking(file: &impl AsRawFd) -> std::io::Result<()> {
    let fd = file.as_raw_fd();
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

struct OwnedDecoder {
    child: Child,
    reaped: bool,
}
impl OwnedDecoder {
    fn kill(&self) -> std::io::Result<()> {
        // the leader is not reaped until after group signalling. Its owned PID
        // therefore cannot be recycled into another process group between calls.
        if unsafe { libc::kill(-(self.child.id() as i32), libc::SIGKILL) } != 0 {
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() != Some(libc::ESRCH) {
                return Err(error);
            }
        }
        Ok(())
    }
    fn exited(&self) -> std::io::Result<bool> {
        let mut info = std::mem::MaybeUninit::<libc::siginfo_t>::zeroed();
        if unsafe {
            libc::waitid(
                libc::P_PID,
                self.child.id(),
                info.as_mut_ptr(),
                libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
            )
        } != 0
        {
            return Err(std::io::Error::last_os_error());
        }
        Ok(unsafe { info.assume_init().si_pid() } != 0)
    }
    fn group_quiet(&self) -> std::io::Result<bool> {
        let group = self.child.id() as i32;
        let mut members = 0;
        // getpgid + pidfds avoid parsing /proc/stat's parenthesized comm field.
        // the unreaped leader pins the group ID throughout this observation.
        for entry in std::fs::read_dir("/proc")? {
            let entry = entry?;
            let Some(pid) = entry
                .file_name()
                .to_str()
                .and_then(|name| name.parse::<i32>().ok())
            else {
                continue;
            };
            if pid == group || unsafe { libc::getpgid(pid) } != group {
                continue;
            }
            members += 1;
            if members > 64 {
                return Err(std::io::Error::other("decoder descendant budget exceeded"));
            }
            let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) } as i32;
            if fd < 0 {
                if unsafe { libc::getpgid(pid) } != group {
                    continue;
                }
                return Err(std::io::Error::last_os_error());
            }
            let handle = unsafe { File::from_raw_fd(fd) };
            if unsafe { libc::getpgid(pid) } != group {
                continue;
            }
            // a recycled numeric PID cannot turn an exited pidfd into proof
            // about its replacement. Retry the snapshot on that race.
            if unsafe {
                libc::syscall(
                    libc::SYS_pidfd_send_signal,
                    handle.as_raw_fd(),
                    0,
                    std::ptr::null::<libc::siginfo_t>(),
                    0,
                )
            } != 0
            {
                return Ok(false);
            }
            let mut poll = libc::pollfd {
                fd: handle.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            if unsafe { libc::poll(&mut poll, 1, 0) } != 1 || poll.revents & libc::POLLIN == 0 {
                return Ok(false);
            }
        }
        Ok(true)
    }
}
impl Drop for OwnedDecoder {
    fn drop(&mut self) {
        if !self.reaped {
            let _ = self.kill();
        }
        // no blocking wait here. The supervisor normally retains this object
        // until nonblocking exit observation and reaping have succeeded.
    }
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Wire {
    state: u8,
    maps: Option<String>,
}
fn packet(state: u8, maps: bool, check: &impl Fn() -> Result<(), String>) -> Result<(), String> {
    let maps = if maps {
        let mut text = String::new();
        File::open("/proc/self/maps")
            .and_then(|file| file.take(MAX_MAPS).read_to_string(&mut text))
            .map_err(|_| "Could not observe decoder modules.")?;
        Some(text)
    } else {
        None
    };
    let mut bytes = serde_json::to_vec(&Wire { state, maps })
        .map_err(|_| "Decoder protocol serialization failed.")?;
    bytes.push(b'\n');
    let mut output = std::io::stdout().lock();
    let mut remaining = bytes.as_slice();
    // maps can exceed pipe capacity while the real supervisor is parked. A
    // blocking write would hide direct scope revocation during active output.
    while !remaining.is_empty() {
        check()?;
        match output.write(remaining) {
            Ok(0) => return Err("Decoder supervisor disconnected.".into()),
            Ok(count) => remaining = &remaining[count..],
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(2));
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => return Err("Decoder supervisor disconnected.".into()),
        }
    }
    loop {
        check()?;
        match output.flush() {
            Ok(()) => return Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(2));
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => return Err("Decoder supervisor disconnected.".into()),
        }
    }
}

fn monotonic_ns() -> std::io::Result<u64> {
    let mut time = std::mem::MaybeUninit::<libc::timespec>::uninit();
    if unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, time.as_mut_ptr()) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    let time = unsafe { time.assume_init() };
    u64::try_from(time.tv_sec)
        .ok()
        .and_then(|seconds| seconds.checked_mul(1_000_000_000))
        .and_then(|nanos| nanos.checked_add(time.tv_nsec as u64))
        .ok_or_else(|| std::io::Error::other("invalid monotonic clock"))
}

fn hex_binding(binding: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(binding.len() * 2);
    for &byte in binding {
        encoded.push(char::from(HEX[usize::from(byte >> 4)]));
        encoded.push(char::from(HEX[usize::from(byte & 15)]));
    }
    encoded
}

fn parse_binding(encoded: &str) -> Result<Vec<u8>, String> {
    if encoded.is_empty()
        || encoded.len() > crate::notifications::MAX_EFFECT_BINDING * 2
        || !encoded.len().is_multiple_of(2)
        || !encoded.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return Err("Invalid bounded audio effect binding.".into());
    }
    encoded
        .as_bytes()
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| {
            let text = std::str::from_utf8(pair).map_err(|_| "Invalid binding byte.")?;
            u8::from_str_radix(text, 16).map_err(|_| "Invalid binding byte.".into())
        })
        .collect()
}

fn parse_effect_fds(list: &str, operation_fd: i32) -> Result<Vec<OwnedFd>, String> {
    if list.len() > 96 {
        return Err("Audio effect descriptor list exceeds its bound.".into());
    }
    let mut raw = Vec::new();
    for item in list.split(',') {
        let fd = item
            .parse::<i32>()
            .map_err(|_| "Invalid audio effect descriptor.")?;
        if fd < 3
            || fd == operation_fd
            || raw.contains(&fd)
            || raw.len() > crate::notifications::MAX_EFFECT_SCOPES
            || unsafe { libc::fcntl(fd, libc::F_GETFD) } < 0
        {
            return Err("Missing, aliased or invalid audio effect descriptor.".into());
        }
        raw.push(fd);
    }
    // no OwnedFd is constructed before the complete numeric set is validated;
    // duplicates or an operation-FD alias must never cause double ownership.
    Ok(raw
        .into_iter()
        .map(|fd| unsafe { OwnedFd::from_raw_fd(fd) })
        .collect())
}

fn supervise(job: &Job) -> Outcome {
    if !job.operation.permitted() {
        return Outcome::Cancelled;
    }
    // SIGKILL cannot run the helper's registry-directory destructor. The fixed
    // supervisor owns its entire temporary subtree until cleanup is confirmed.
    let Some(temporary) = glib::mkdtemp(std::env::temp_dir().join("among-audio-operation-XXXXXX"))
    else {
        return Outcome::Failed("The decoder temporary directory is unavailable.");
    };
    let mut outcome = supervise_decoder(job, &temporary);
    loop {
        match std::fs::remove_dir_all(&temporary) {
            Ok(()) => break,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => break,
            Err(_) => {
                outcome = Outcome::Failed(
                    "Audio temporary cleanup is unconfirmed; the decoder slot is quarantined.",
                );
                job.operation.update(|report| {
                    report.snapshot.phase = DecoderPhase::Quarantined;
                    report.outcome = Some(outcome);
                });
            }
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    if matches!(outcome, Outcome::Ended) && !job.operation.permitted() {
        return Outcome::Cancelled;
    }
    if matches!(outcome, Outcome::Ended) && job.started.elapsed() >= DEADLINE {
        return Outcome::Failed("Audio exceeded its 13-second operation deadline.");
    }
    outcome
}

fn supervise_decoder(job: &Job, temporary: &Path) -> Outcome {
    if !job.operation.permitted() {
        return Outcome::Cancelled;
    }
    if job.started.elapsed() >= DEADLINE {
        return Outcome::Failed("Audio expired before decoder construction.");
    }
    let result = (|| -> std::io::Result<Child> {
        let descriptors = job
            .process_effect
            .as_ref()
            .map(ProcessEffectAuthorization::duplicate_fds)
            .transpose()
            .map_err(|_| std::io::Error::other("audio event descriptor export failed"))?;
        let inherited: Vec<i32> = descriptors
            .as_ref()
            .map(|descriptors| {
                descriptors
                    .as_slice()
                    .iter()
                    .map(AsRawFd::as_raw_fd)
                    .collect()
            })
            .unwrap_or_default();
        let mut command = Command::new(std::env::current_exe()?);
        let deadline = monotonic_ns()?
            .checked_add(DEADLINE.saturating_sub(job.started.elapsed()).as_nanos() as u64)
            .ok_or_else(|| std::io::Error::other("decoder deadline overflow"))?;
        command
            .arg(FLAG)
            .arg(
                ROOT.get()
                    .and_then(Option::as_ref)
                    .map(|root| root.as_os_str())
                    .unwrap_or(std::ffi::OsStr::new("-")),
            )
            .arg(deadline.to_string())
            .arg(job.operation.authority.id().to_string())
            .arg(job.operation.authority.fd().to_string())
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .stdin(Stdio::from(job.sound.decoder_file()?))
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0);
        if let Some(effect) = &job.process_effect {
            command.arg(hex_binding(effect.binding())).arg(
                inherited
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(","),
            );
        }
        for key in [
            "HOME",
            "LANG",
            "LC_ALL",
            "DISPLAY",
            "WAYLAND_DISPLAY",
            "XAUTHORITY",
            "DBUS_SESSION_BUS_ADDRESS",
            "XDG_RUNTIME_DIR",
            "XDG_CONFIG_HOME",
            "XDG_DATA_HOME",
            "XDG_CACHE_HOME",
            "TMPDIR",
            "PULSE_SERVER",
            "PULSE_SINK",
            "PULSE_SOURCE",
            "GDK_BACKEND",
            "GSK_RENDERER",
            "GTK_A11Y",
        ] {
            if let Some(value) = std::env::var_os(key) {
                command.env(key, value);
            }
        }
        command.env("TMPDIR", temporary);
        let parent = unsafe { libc::getpid() };
        let authority_fd = job.operation.authority.fd();
        // only async-signal-safe syscalls run between fork/exec. A crashing host
        // cannot leave its decoder alive; the parent-race check covers early exit.
        unsafe {
            command.pre_exec(move || {
                // Keep the already-reserved descriptor number: dup2 onto a fixed
                // number could overwrite Command's private exec-error pipe.
                if libc::fcntl(authority_fd, libc::F_SETFD, 0) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                for &fd in &inherited {
                    if libc::fcntl(fd, libc::F_SETFD, 0) < 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                }
                if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                if libc::getppid() != parent {
                    libc::_exit(125);
                }
                Ok(())
            });
        }
        if !job.operation.permitted() {
            return Err(std::io::Error::other(
                "audio authority revoked before spawn",
            ));
        }
        let child = command.spawn();
        // retain the library's FD budget reservation through the actual spawn
        // return, including delayed parent-side spawn observation.
        drop(descriptors);
        child
    })();
    let child = match result {
        Ok(child) => child,
        Err(_) => return Outcome::Failed("The audio decoder could not start."),
    };
    let mut child = OwnedDecoder {
        child,
        reaped: false,
    };
    job.operation.update(|report| {
        report.snapshot.pid = Some(child.child.id());
        report.snapshot.phase = DecoderPhase::Constructing;
    });
    let Some(mut output) = child.child.stdout.take() else {
        return quarantine(job, child, "The decoder protocol pipe is unavailable.");
    };
    let Some(mut errors) = child.child.stderr.take() else {
        return quarantine(job, child, "The decoder diagnostic pipe is unavailable.");
    };
    if nonblocking(&output)
        .and_then(|()| nonblocking(&errors))
        .is_err()
    {
        return quarantine(
            job,
            child,
            "The decoder pipes could not be made nonblocking.",
        );
    }
    if unsafe { libc::getpgid(child.child.id() as i32) } != child.child.id() as i32 {
        return quarantine(
            job,
            child,
            "The decoder process group could not be confirmed.",
        );
    }
    // no grant exists until real spawn observation, owned group identity and
    // bounded pipes are established. Revocation cannot transition back to granted.
    if job.operation.permitted() && job.started.elapsed() < DEADLINE {
        job.operation.authority.grant();
    }
    let mut buffer = Vec::new();
    let mut diagnostics = Vec::new();
    let mut terminal = None;
    let mut retire_at = None;
    let mut killed_at = None;
    let mut exited_at = None;
    let mut output_closed = false;
    let mut protocol_phase = 0;
    loop {
        let now = Instant::now();
        if !job.operation.permitted() {
            terminal = Some(Outcome::Cancelled);
            retire_at = Some(now);
        }
        if now.duration_since(job.started) >= DEADLINE {
            terminal = Some(Outcome::Failed(
                "Audio exceeded its 13-second operation deadline.",
            ));
            retire_at = Some(now);
        }
        let mut bytes = [0; 16 * 1024];
        match output.read(&mut bytes) {
            Ok(count) => {
                output_closed = count == 0;
                buffer.extend_from_slice(&bytes[..count]);
                if buffer.len() > MAX_PACKET {
                    terminal = Some(Outcome::Failed(
                        "The audio decoder exceeded its protocol budget.",
                    ));
                    retire_at = Some(now);
                    buffer.clear();
                }
                while let Some(end) = buffer.iter().position(|&byte| byte == b'\n') {
                    let wire = serde_json::from_slice::<Wire>(&buffer[..end]);
                    buffer.drain(..=end);
                    match wire {
                        Ok(wire)
                            if matches!((protocol_phase, wire.state), (0, 1) | (1, 2) | (2, 3)) =>
                        {
                            protocol_phase = wire.state;
                            job.operation.update(|report| {
                                report.snapshot.phase = if wire.state == 1 {
                                    DecoderPhase::Constructing
                                } else if wire.state == 2 {
                                    DecoderPhase::Playing
                                } else {
                                    DecoderPhase::Retiring
                                };
                                if let Some(maps) = wire.maps {
                                    report.snapshot.maps = Arc::from(maps);
                                }
                            });
                            if protocol_phase == 3 && terminal.is_none() {
                                terminal = Some(Outcome::Ended);
                                retire_at = Some((now + CLEANUP_GRACE).min(job.started + DEADLINE));
                            }
                        }
                        _ => {
                            terminal = Some(Outcome::Failed(
                                "The audio decoder returned an invalid protocol response.",
                            ));
                            retire_at = Some(now);
                        }
                    }
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(_) => {
                terminal = Some(Outcome::Failed("The audio decoder protocol failed."));
                retire_at = Some(now);
            }
        }
        if let Ok(count) = errors.read(&mut bytes) {
            if count > 0 && diagnostics.len() < MAX_STDERR {
                diagnostics.extend_from_slice(&bytes[..count.min(MAX_STDERR - diagnostics.len())]);
                job.operation.update(|report| {
                    report.snapshot.stderr =
                        Arc::from(String::from_utf8_lossy(&diagnostics).as_ref())
                });
            }
        }
        if output_closed && !buffer.is_empty() {
            terminal = Some(Outcome::Failed(
                "The audio decoder returned a truncated protocol frame.",
            ));
            retire_at = Some(now);
        }
        if child.exited().unwrap_or(false) && child.kill().is_ok() {
            exited_at.get_or_insert(now);
            if output_closed && child.group_quiet().unwrap_or(false) {
                if let Ok(Some(status)) = child.child.try_wait() {
                    child.reaped = true;
                    if !job.operation.permitted() {
                        return Outcome::Cancelled;
                    }
                    if job.started.elapsed() >= DEADLINE {
                        return Outcome::Failed("Audio exceeded its 13-second operation deadline.");
                    }
                    return match terminal {
                        Some(Outcome::Cancelled) => Outcome::Cancelled,
                        Some(Outcome::Ended) if status.success() && killed_at.is_none() => {
                            Outcome::Ended
                        }
                        Some(Outcome::Failed(reason)) => Outcome::Failed(reason),
                        _ => Outcome::Failed(
                            "The audio decoder failed or did not retire successfully.",
                        ),
                    };
                }
            }
        }
        if exited_at.is_some_and(|at| now.duration_since(at) >= CONFIRM) {
            return quarantine(
                job,
                child,
                "The decoder exited without completing its bounded protocol.",
            );
        }
        if killed_at.is_none() && retire_at.is_some_and(|at| now >= at) {
            job.operation.authority.revoke();
            let _ = child.kill();
            killed_at = Some(now);
            job.operation
                .update(|report| report.snapshot.phase = DecoderPhase::Retiring);
        }
        if killed_at.is_some_and(|at| now.duration_since(at) >= CONFIRM) {
            return quarantine(
                job,
                child,
                "Audio cleanup is unconfirmed; the decoder slot is quarantined.",
            );
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// after the UI loop has exited, allow bounded confirmation before host exit.
/// never call this from a GTK callback; Audio::shutdown itself never waits.
pub fn finish_shutdown() -> bool {
    SHUTTING_DOWN.store(true, Ordering::Release);
    let started = Instant::now();
    loop {
        if let Ok(current) = CURRENT_AUTHORITY.try_lock() {
            if let Some(authority) = current.as_ref() {
                authority.revoke();
            }
        }
        if !BUSY.load(Ordering::Acquire) || started.elapsed() >= CONFIRM {
            break;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    !BUSY.load(Ordering::Acquire)
}

fn quarantine(job: &Job, mut child: OwnedDecoder, reason: &'static str) -> Outcome {
    job.operation.authority.revoke();
    job.operation.update(|report| {
        report.snapshot.phase = DecoderPhase::Quarantined;
        report.outcome = Some(Outcome::Failed(reason));
    });
    // retain the single worker/slot and unreaped leader on uncertainty. No new
    // helper is admitted, no extra reaper is leaked, and no reused PID is killed.
    loop {
        let _ = child.kill();
        if child.exited().unwrap_or(false)
            && child.kill().is_ok()
            && child.group_quiet().unwrap_or(false)
        {
            if let Ok(Some(_)) = child.child.try_wait() {
                child.reaped = true;
                return Outcome::Failed(reason);
            }
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// dispatch only at the beginning of the host executable (and test probes),
/// before the UI/commands bootstrap. GTK objects stay on this helper's thread.
pub fn dispatch() -> Option<glib::ExitCode> {
    let args: Vec<_> = std::env::args_os().collect();
    if args.get(1).is_none_or(|arg| arg != FLAG) {
        return None;
    }
    let result = (|| -> Result<(), String> {
        if !matches!(args.len(), 6 | 8) {
            return Err("Invalid audio decoder invocation.".into());
        }
        let id = args[4]
            .to_str()
            .and_then(|value| value.parse::<u64>().ok())
            .ok_or("Invalid audio operation identity.")?;
        let fd = args[5]
            .to_str()
            .and_then(|value| value.parse::<i32>().ok())
            .filter(|&fd| fd >= 3 && unsafe { libc::fcntl(fd, libc::F_GETFD) } >= 0)
            .ok_or("Invalid audio authority descriptor.")?;
        let binding = match args.get(6) {
            Some(encoded) => {
                parse_binding(encoded.to_str().ok_or("Invalid effect binding encoding.")?)?
            }
            None => Vec::new(),
        };
        let authority_file = unsafe { File::from_raw_fd(fd) };
        let authority = if binding.is_empty() {
            Authorization::receive(authority_file, id)
        } else {
            Authorization::receive_bound(authority_file, id, &binding)
        }
        .map_err(|error| error.to_string())?;
        let process_effect = match args.get(7) {
            Some(list) => {
                let fds =
                    parse_effect_fds(list.to_str().ok_or("Invalid effect descriptor list.")?, fd)?;
                Some(
                    ProcessEffectAuthorization::import_for_helper(&binding, fds)
                        .map_err(|error| error.to_string())?,
                )
            }
            None => None,
        };
        nonblocking(&std::io::stdout()).map_err(|_| "Decoder protocol output is unavailable.")?;
        let deadline = args[3]
            .to_str()
            .and_then(|value| value.parse::<u64>().ok())
            .ok_or("Invalid decoder deadline.")?;
        let check_deadline = || -> Result<(), String> {
            let mut protocol = libc::pollfd {
                fd: 1,
                events: 0,
                revents: 0,
            };
            let result = unsafe { libc::poll(&mut protocol, 1, 0) };
            if result < 0
                || protocol.revents & (libc::POLLERR | libc::POLLHUP | libc::POLLNVAL) != 0
            {
                return Err("Decoder supervisor disconnected.".into());
            }
            if authority.revoked() {
                return Err("Audio operation was cancelled before playback.".into());
            }
            let now = monotonic_ns().map_err(|_| "Decoder monotonic clock is unavailable.")?;
            if now >= deadline || deadline - now > DEADLINE.as_nanos() as u64 {
                return Err("Decoder operation expired or has an invalid deadline.".into());
            }
            if process_effect
                .as_ref()
                .is_some_and(|effect| !effect.is_current_monotonic(now))
            {
                return Err("Audio event authority was revoked or expired.".into());
            }
            Ok(())
        };
        check_deadline()?;
        let seals = unsafe { libc::fcntl(0, libc::F_GET_SEALS) };
        let required =
            libc::F_SEAL_WRITE | libc::F_SEAL_GROW | libc::F_SEAL_SHRINK | libc::F_SEAL_SEAL;
        if seals < 0 || seals & required != required {
            return Err("Audio decoder requires an owned sealed input.".into());
        }
        let sound =
            Sound::read_local(Path::new("/proc/self/fd/0")).map_err(|error| error.to_string())?;
        packet(1, false, &check_deadline)?;
        if args[2] != "-" {
            unsafe {
                super::runtime::initialize_packaged(Path::new(&args[2]))?;
            }
        } else {
            gtk::init().map_err(|_| "GTK audio initialization failed.")?;
        }
        use gtk::prelude::*;
        check_deadline()?;
        let media = gtk::MediaFile::for_filename(sound.decoder_path());
        media.set_loop(false);
        media.set_volume(1.0);
        loop {
            check_deadline()?;
            for _ in 0..100 {
                if !glib::MainContext::default().iteration(false) {
                    break;
                }
            }
            if media.error().is_some() {
                return Err("The WAV decoder or audio output is unavailable.".into());
            }
            if media.is_prepared() && authority.claim()? {
                if let Some(effect) = &process_effect {
                    effect
                        .validate_after_claim()
                        .map_err(|_| "Audio event authority failed post-claim validation.")?;
                }
                media.play();
                break;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        packet(2, true, &check_deadline)?;
        loop {
            check_deadline()?;
            for _ in 0..100 {
                if !glib::MainContext::default().iteration(false) {
                    break;
                }
            }
            if media.error().is_some() {
                return Err("The WAV decoder or audio output is unavailable.".into());
            }
            if media.is_ended() {
                packet(3, true, &check_deadline)?;
                break;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        media.pause();
        media.clear();
        drop(media);
        Ok(())
    })();
    Some(match result {
        Ok(()) => glib::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("among audio decoder: {error}");
            glib::ExitCode::FAILURE
        }
    })
}
