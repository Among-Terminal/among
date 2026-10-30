//! owned effect authority across exec: core mutators and read-only helpers see
//! the same terminal scope cells. exporting never copies a sampled true flag.

use super::{Authorization, Job, Kind, EVENT_LIFETIME};
use std::fmt;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::fs::MetadataExt;
use std::ptr::NonNull;
use std::sync::atomic::{fence, AtomicU32, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Instant;

pub const MAX_EFFECT_SCOPES: usize = 5;
pub const MAX_EFFECT_FDS: usize = MAX_EFFECT_SCOPES + 1;
pub const MAX_EFFECT_BINDING: usize = 512;
pub const MAX_EFFECT_HANDLES: usize = 4096;
const CELL_HEADER: usize = 64;
const HEADER: usize = 128;
const RECORD: usize = 40;
const VERSION: u16 = 1;
const CELL_SEALS: i32 =
    libc::F_SEAL_GROW | libc::F_SEAL_SHRINK | libc::F_SEAL_FUTURE_WRITE | libc::F_SEAL_SEAL;
const MANIFEST_SEALS: i32 =
    libc::F_SEAL_GROW | libc::F_SEAL_SHRINK | libc::F_SEAL_WRITE | libc::F_SEAL_SEAL;

#[derive(Debug)]
pub enum EffectAuthorityError {
    Io(io::Error),
    InvalidBinding,
    InvalidDescriptor,
    JobMismatch,
    MetadataUnavailable,
    Revoked,
    Expired,
    Clock,
    Exhausted,
}

impl fmt::Display for EffectAuthorityError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Io(_) => "effect authority IO failed",
            Self::InvalidBinding => "invalid effect authority binding",
            Self::InvalidDescriptor => "invalid effect authority descriptor",
            Self::JobMismatch => "effect authorities belong to different jobs",
            Self::MetadataUnavailable => "effect metadata or origin is unavailable",
            Self::Revoked => "effect authority is revoked",
            Self::Expired => "effect authority expired",
            Self::Clock => "effect monotonic clock is unavailable",
            Self::Exhausted => "effect authority handle budget exhausted",
        })
    }
}
impl std::error::Error for EffectAuthorityError {}
impl From<io::Error> for EffectAuthorityError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub(super) enum ScopeKind {
    Session = 1,
    Channel = 2,
    Registration = 3,
    Source = 4,
    Origin = 5,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct ScopeIdentity {
    pub kind: ScopeKind,
    pub session: u64,
    pub key: u64,
    pub generation: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct FileIdentity {
    device: u64,
    inode: u64,
}

static HANDLES: AtomicUsize = AtomicUsize::new(0);
struct HandleBudget(usize);
impl HandleBudget {
    fn reserve(count: usize) -> Result<Self, EffectAuthorityError> {
        HANDLES
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |live| {
                live.checked_add(count)
                    .filter(|next| *next <= MAX_EFFECT_HANDLES)
            })
            .map_err(|_| EffectAuthorityError::Exhausted)?;
        Ok(Self(count))
    }
}
impl Drop for HandleBudget {
    fn drop(&mut self) {
        HANDLES.fetch_sub(self.0, Ordering::AcqRel);
    }
}

/// retain this bounded set through owned-helper spawn; only raw borrowed handles
/// are deliberately inherited. drop closes the parent copies and budget together.
pub struct EffectDescriptors {
    fds: Vec<OwnedFd>,
    _budget: HandleBudget,
}
impl EffectDescriptors {
    pub fn as_slice(&self) -> &[OwnedFd] {
        &self.fds
    }
    pub fn len(&self) -> usize {
        self.fds.len()
    }
    pub fn is_empty(&self) -> bool {
        self.fds.is_empty()
    }
}
impl AsRef<[OwnedFd]> for EffectDescriptors {
    fn as_ref(&self) -> &[OwnedFd] {
        self.as_slice()
    }
}

struct Mapping {
    pointer: NonNull<u8>,
    length: usize,
}
// mapping lifetime is reference-owned; immutable bytes never change after seals.
// the only mutable location is an aligned, lock-free shared AtomicU32.
unsafe impl Send for Mapping {}
unsafe impl Sync for Mapping {}
impl Mapping {
    fn new(fd: &OwnedFd, length: usize, writable: bool) -> Result<Self, EffectAuthorityError> {
        let protection = libc::PROT_READ | if writable { libc::PROT_WRITE } else { 0 };
        let pointer = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                length,
                protection,
                libc::MAP_SHARED,
                fd.as_raw_fd(),
                0,
            )
        };
        if pointer == libc::MAP_FAILED {
            return Err(io::Error::last_os_error().into());
        }
        let Some(pointer) = NonNull::new(pointer.cast::<u8>()) else {
            unsafe {
                libc::munmap(pointer, length);
            }
            return Err(EffectAuthorityError::InvalidDescriptor);
        };
        Ok(Self { pointer, length })
    }
    fn live(&self) -> &AtomicU32 {
        // both fixed layout and mmap page alignment establish atomic alignment.
        unsafe {
            &*self
                .pointer
                .as_ptr()
                .add(self.length / 2)
                .cast::<AtomicU32>()
        }
    }
    fn prefix(&self, length: usize) -> &[u8] {
        // callers only read immutable prefixes, never racing byte reads of live.
        unsafe { std::slice::from_raw_parts(self.pointer.as_ptr(), length) }
    }
}
impl Drop for Mapping {
    fn drop(&mut self) {
        unsafe {
            libc::munmap(self.pointer.as_ptr().cast(), self.length);
        }
    }
}

struct ScopeState {
    mapping: Mapping,
    fd: OwnedFd,
    identity: ScopeIdentity,
    file: FileIdentity,
    nonce: [u8; 16],
    _budget: HandleBudget,
}

#[derive(Clone)]
pub(super) struct Scope(Arc<ScopeState>);
impl fmt::Debug for Scope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Scope")
            .field("identity", &self.0.identity)
            .field("current", &self.is_current())
            .finish()
    }
}
impl Scope {
    pub fn new(identity: ScopeIdentity) -> Result<Self, EffectAuthorityError> {
        let budget = HandleBudget::reserve(1)?;
        let size = cell_size()?;
        let fd = memfd(size)?;
        let nonce = nonce()?;
        let mapping = Mapping::new(&fd, size, true)?;
        let mut header = [0u8; CELL_HEADER];
        header[..8].copy_from_slice(b"AMNGRV01");
        header[8..10].copy_from_slice(&VERSION.to_le_bytes());
        header[10] = identity.kind as u8;
        header[16..32].copy_from_slice(&nonce);
        put(&mut header, 32, identity.session);
        put(&mut header, 40, identity.key);
        put(&mut header, 48, identity.generation);
        unsafe {
            std::ptr::copy_nonoverlapping(header.as_ptr(), mapping.pointer.as_ptr(), header.len());
            mapping
                .pointer
                .as_ptr()
                .add(size / 2)
                .cast::<AtomicU32>()
                .write(AtomicU32::new(1));
        }
        // identity is on a read-only owner page; only the second-page atomic is
        // writable. exported mappings can write neither page, even via reopen.
        if unsafe { libc::mprotect(mapping.pointer.as_ptr().cast(), size / 2, libc::PROT_READ) }
            != 0
        {
            return Err(io::Error::last_os_error().into());
        }
        seal(&fd, CELL_SEALS)?;
        let file = file_identity(&fd, size, CELL_SEALS, false)?;
        Ok(Self(Arc::new(ScopeState {
            mapping,
            fd,
            identity,
            file,
            nonce,
            _budget: budget,
        })))
    }
    pub fn revoke(&self) {
        self.0.mapping.live().store(0, Ordering::SeqCst);
    }
    pub fn is_current(&self) -> bool {
        self.0.mapping.live().load(Ordering::SeqCst) == 1
    }
    pub fn identity(&self) -> ScopeIdentity {
        self.0.identity
    }
    pub fn same(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

/// dropping the adapter's registration owner revokes helper-visible authority,
/// independently of Host/Runtime polling. clones of read authority cannot revoke.
#[derive(Debug)]
pub struct Registration {
    pub(super) scope: Scope,
}
impl Registration {
    pub fn revoke(&self) {
        self.scope.revoke();
    }
    pub fn is_current(&self) -> bool {
        self.scope.is_current()
    }
}
impl Drop for Registration {
    fn drop(&mut self) {
        self.revoke();
    }
}

#[derive(Clone, Debug)]
pub struct EffectAuthorization {
    core: Authorization,
    expires_at: Instant,
    monotonic_deadline: u64,
    nonce: [u8; 16],
    namespace: FileIdentity,
}
impl EffectAuthorization {
    pub(super) fn issue(core: &Authorization, now: Instant) -> Result<Self, EffectAuthorityError> {
        if !core.is_current() {
            return Err(EffectAuthorityError::Revoked);
        }
        let expires_at = core
            .job()
            .event()
            .received_at()
            .checked_add(EVENT_LIFETIME)
            .ok_or(EffectAuthorityError::Clock)?;
        if now >= expires_at {
            return Err(EffectAuthorityError::Expired);
        }
        // the clock sample precedes the Instant sample: this conservative mapping
        // can shorten by their sampling interval, never extend original expiry.
        let namespace = time_namespace()?;
        let clock = monotonic_now_ns()?;
        let monotonic_deadline = deadline_from_samples(expires_at, clock, Instant::now())?;
        if time_namespace()? != namespace {
            return Err(EffectAuthorityError::Clock);
        }
        let value = Self {
            core: core.clone(),
            expires_at,
            monotonic_deadline,
            nonce: nonce()?,
            namespace,
        };
        if Instant::now() >= expires_at {
            return Err(EffectAuthorityError::Expired);
        }
        if !value.is_current(now) {
            return Err(EffectAuthorityError::Revoked);
        }
        Ok(value)
    }
    pub fn job(&self) -> Job {
        self.core.job()
    }
    pub fn expires_at(&self) -> Instant {
        self.expires_at
    }
    pub fn is_current(&self, now: Instant) -> bool {
        now < self.expires_at && self.core.is_current()
    }

    pub fn export_process(&self) -> Result<ProcessEffectAuthorization, EffectAuthorityError> {
        if Instant::now() >= self.expires_at {
            return Err(EffectAuthorityError::Expired);
        }
        if !self.core.is_current() {
            return Err(EffectAuthorityError::Revoked);
        }
        if time_namespace()? != self.namespace {
            return Err(EffectAuthorityError::Clock);
        }
        let count = self.core.scopes.len();
        if !(3..=MAX_EFFECT_SCOPES).contains(&count) {
            return Err(EffectAuthorityError::InvalidBinding);
        }
        let _temporary_handles = HandleBudget::reserve(count + 2)?;
        let manifest = memfd(MAX_EFFECT_BINDING)?;
        let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
        if unsafe { libc::fstat(manifest.as_raw_fd(), stat.as_mut_ptr()) } != 0 {
            return Err(io::Error::last_os_error().into());
        }
        let stat = unsafe { stat.assume_init() };
        let mut binding = vec![0u8; HEADER + count * RECORD];
        binding[..8].copy_from_slice(b"AMNGEF01");
        binding[8..10].copy_from_slice(&VERSION.to_le_bytes());
        binding[10] = count as u8;
        let job = self.job();
        let event = job.event();
        binding[11] =
            u8::from(event.source().is_some()) | (u8::from(event.origin().is_some()) << 1);
        binding[12] = kind_code(event.kind());
        put(&mut binding, 16, event.session());
        put(&mut binding, 24, event.id());
        put(&mut binding, 32, job.channel.id);
        put(&mut binding, 40, job.revision);
        put(
            &mut binding,
            48,
            event.source().map_or(0, |source| source.pane()),
        );
        put(
            &mut binding,
            56,
            event.source().map_or(0, |source| source.generation()),
        );
        put(
            &mut binding,
            64,
            event.origin().map_or(0, |origin| origin.id),
        );
        put(&mut binding, 72, self.monotonic_deadline);
        binding[80..96].copy_from_slice(&self.nonce);
        put(&mut binding, 96, stat.st_dev);
        put(&mut binding, 104, stat.st_ino);
        put(&mut binding, 112, self.namespace.device);
        put(&mut binding, 120, self.namespace.inode);
        let mut fds = Vec::with_capacity(count + 1);
        for (index, scope) in self.core.scopes.iter().enumerate() {
            let at = HEADER + index * RECORD;
            binding[at] = scope.identity().kind as u8;
            put(&mut binding, at + 8, scope.0.file.device);
            put(&mut binding, at + 16, scope.0.file.inode);
            binding[at + 24..at + 40].copy_from_slice(&scope.0.nonce);
            fds.push(read_only(&scope.0.fd)?);
        }
        let mut contents = [0u8; MAX_EFFECT_BINDING];
        contents[..binding.len()].copy_from_slice(&binding);
        write_manifest(&manifest, &contents)?;
        seal(&manifest, MANIFEST_SEALS)?;
        fds.insert(0, read_only(&manifest)?);
        let exported = ProcessEffectAuthorization::import_for_helper(&binding, fds)?;
        if !self.core.is_current() {
            return Err(EffectAuthorityError::Revoked);
        }
        Ok(exported)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProcessJobIdentity {
    pub session: u64,
    pub event: u64,
    pub channel: u64,
    pub revision: u64,
    pub kind: Kind,
    pub source: Option<(u64, u64)>,
    pub origin: Option<u64>,
}
struct ProcessScope {
    mapping: Mapping,
    fd: OwnedFd,
}

pub struct ProcessEffectAuthorization {
    binding: Vec<u8>,
    manifest: OwnedFd,
    scopes: Vec<ProcessScope>,
    job: ProcessJobIdentity,
    deadline: u64,
    _budget: HandleBudget,
}
impl fmt::Debug for ProcessEffectAuthorization {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ProcessEffectAuthorization")
            .field("job", &self.job)
            .field("deadline", &self.deadline)
            .finish()
    }
}
impl ProcessEffectAuthorization {
    pub fn binding(&self) -> &[u8] {
        &self.binding
    }
    pub fn job_identity(&self) -> ProcessJobIdentity {
        self.job
    }
    pub fn expires_at_monotonic_ns(&self) -> u64 {
        self.deadline
    }
    pub fn is_current_monotonic(&self, now_ns: u64) -> bool {
        // admission/playback check only: a supplied time is not a final-start
        // proof. use validate_after_claim for its ordered fresh-clock sequence.
        now_ns < self.deadline
            && self
                .scopes
                .iter()
                .all(|scope| scope.mapping.live().load(Ordering::SeqCst) == 1)
    }
    /// call immediately after the successful owned-operation CAS, never before.
    /// the post-CAS fence/SC loads cannot be hoisted into admission/preparation.
    /// terminal scopes observed live here were live at claim; the later fresh
    /// clock proves claim preceded expiry. failure is terminal for that operation.
    pub fn validate_after_claim(&self) -> Result<(), EffectAuthorityError> {
        fence(Ordering::SeqCst);
        if !self
            .scopes
            .iter()
            .all(|scope| scope.mapping.live().load(Ordering::SeqCst) == 1)
        {
            return Err(EffectAuthorityError::Revoked);
        }
        if monotonic_now_ns()? >= self.deadline {
            return Err(EffectAuthorityError::Expired);
        }
        Ok(())
    }
    pub fn duplicate_fds(&self) -> Result<EffectDescriptors, EffectAuthorityError> {
        if !self.is_current_monotonic(monotonic_now_ns()?) {
            return Err(EffectAuthorityError::Revoked);
        }
        let budget = HandleBudget::reserve(self.scopes.len() + 1)?;
        let fds = std::iter::once(&self.manifest)
            .chain(self.scopes.iter().map(|scope| &scope.fd))
            .map(duplicate)
            .collect::<Result<Vec<_>, _>>()?;
        if !self.is_current_monotonic(monotonic_now_ns()?) {
            return Err(EffectAuthorityError::Revoked);
        }
        Ok(EffectDescriptors {
            fds,
            _budget: budget,
        })
    }
    /// expected binding must come from the trusted issuer/owned-operation route,
    /// not be reconstructed from received FDs. seals/stat/nonce validation binds
    /// those bytes to handles; it is not cryptographic same-user authentication.
    pub fn import_for_helper(
        binding: &[u8],
        mut fds: Vec<OwnedFd>,
    ) -> Result<Self, EffectAuthorityError> {
        if binding.len() < HEADER
            || binding.len() > MAX_EFFECT_BINDING
            || &binding[..8] != b"AMNGEF01"
            || binding[8..10] != VERSION.to_le_bytes()
            || binding[11] & !3 != 0
            || binding[13..16] != [0; 3]
            || binding[80..96] == [0; 16]
        {
            return Err(EffectAuthorityError::InvalidBinding);
        }
        let count = usize::from(binding[10]);
        if !(3..=MAX_EFFECT_SCOPES).contains(&count)
            || binding.len() != HEADER + count * RECORD
            || fds.len() != count + 1
        {
            return Err(EffectAuthorityError::InvalidBinding);
        }
        let kind = match binding[12] {
            1 => Kind::TerminalBell,
            2 => Kind::Application,
            3 => Kind::ManagedNotification,
            _ => return Err(EffectAuthorityError::InvalidBinding),
        };
        let source = (binding[11] & 1 != 0).then(|| (get(binding, 48), get(binding, 56)));
        let origin = (binding[11] & 2 != 0).then(|| get(binding, 64));
        let job = ProcessJobIdentity {
            session: get(binding, 16),
            event: get(binding, 24),
            channel: get(binding, 32),
            revision: get(binding, 40),
            kind,
            source,
            origin,
        };
        if job.session == 0
            || job.event == 0
            || job.channel == 0
            || source.is_some_and(|(_, generation)| generation == 0)
            || origin == Some(0)
            || (source.is_none() && (get(binding, 48) != 0 || get(binding, 56) != 0))
            || (origin.is_none() && get(binding, 64) != 0)
            || !matches!(
                (kind, source.is_some(), origin.is_some()),
                (Kind::TerminalBell, true, false)
                    | (Kind::Application, false, false)
                    | (Kind::ManagedNotification, _, true)
            )
        {
            return Err(EffectAuthorityError::InvalidBinding);
        }
        let expected = identities(job);
        if count != expected.len() {
            return Err(EffectAuthorityError::InvalidBinding);
        }
        let budget = HandleBudget::reserve(fds.len())?;
        // restore close-on-exec before any possible native scanner child. the
        // caller deliberately clears it only in the owned helper's fork path.
        for fd in &fds {
            cloexec(fd)?;
        }
        let manifest = fds.remove(0);
        let file = file_identity(&manifest, MAX_EFFECT_BINDING, MANIFEST_SEALS, true)?;
        if file
            != (FileIdentity {
                device: get(binding, 96),
                inode: get(binding, 104),
            })
        {
            return Err(EffectAuthorityError::InvalidDescriptor);
        }
        let manifest_map = Mapping::new(&manifest, MAX_EFFECT_BINDING, false)?;
        let bytes = manifest_map.prefix(MAX_EFFECT_BINDING);
        if &bytes[..binding.len()] != binding
            || bytes[binding.len()..].iter().any(|byte| *byte != 0)
        {
            return Err(EffectAuthorityError::InvalidBinding);
        }
        if time_namespace()?
            != (FileIdentity {
                device: get(binding, 112),
                inode: get(binding, 120),
            })
        {
            return Err(EffectAuthorityError::Clock);
        }
        let mut seen = Vec::with_capacity(count);
        let mut scopes = Vec::with_capacity(count);
        let cell_size = cell_size()?;
        for (index, (fd, identity)) in fds.into_iter().zip(expected).enumerate() {
            let at = HEADER + index * RECORD;
            if binding[at] != identity.kind as u8 || binding[at + 1..at + 8] != [0; 7] {
                return Err(EffectAuthorityError::InvalidBinding);
            }
            let file = file_identity(&fd, cell_size, CELL_SEALS, true)?;
            if file
                != (FileIdentity {
                    device: get(binding, at + 8),
                    inode: get(binding, at + 16),
                })
                || seen.contains(&file)
            {
                return Err(EffectAuthorityError::InvalidDescriptor);
            }
            seen.push(file);
            let mapping = Mapping::new(&fd, cell_size, false)?;
            let header = mapping.prefix(CELL_HEADER);
            if &header[..8] != b"AMNGRV01"
                || header[8..10] != VERSION.to_le_bytes()
                || header[10] != identity.kind as u8
                || header[11..16] != [0; 5]
                || header[16..32] != binding[at + 24..at + 40]
                || header[16..32] == [0; 16]
                || get(header, 32) != identity.session
                || get(header, 40) != identity.key
                || get(header, 48) != identity.generation
                || header[56..64] != [0; 8]
            {
                return Err(EffectAuthorityError::InvalidDescriptor);
            }
            if mapping.live().load(Ordering::SeqCst) != 1 {
                return Err(EffectAuthorityError::Revoked);
            }
            scopes.push(ProcessScope { mapping, fd });
        }
        let value = Self {
            binding: binding.to_vec(),
            manifest,
            scopes,
            job,
            deadline: get(binding, 72),
            _budget: budget,
        };
        let now = monotonic_now_ns()?;
        if now >= value.deadline {
            return Err(EffectAuthorityError::Expired);
        }
        if !value.is_current_monotonic(now) {
            return Err(EffectAuthorityError::Revoked);
        }
        Ok(value)
    }
}

fn identities(job: ProcessJobIdentity) -> Vec<ScopeIdentity> {
    let mut ids = vec![
        ScopeIdentity {
            kind: ScopeKind::Session,
            session: job.session,
            key: 0,
            generation: 0,
        },
        ScopeIdentity {
            kind: ScopeKind::Channel,
            session: job.session,
            key: job.channel,
            generation: job.revision,
        },
        ScopeIdentity {
            kind: ScopeKind::Registration,
            session: job.session,
            key: job.channel,
            generation: 0,
        },
    ];
    if let Some((pane, generation)) = job.source {
        ids.push(ScopeIdentity {
            kind: ScopeKind::Source,
            session: job.session,
            key: pane,
            generation,
        });
    }
    if let Some(origin) = job.origin {
        ids.push(ScopeIdentity {
            kind: ScopeKind::Origin,
            session: job.session,
            key: origin,
            generation: 0,
        });
    }
    ids
}

pub fn monotonic_now_ns() -> Result<u64, EffectAuthorityError> {
    let mut time = std::mem::MaybeUninit::<libc::timespec>::uninit();
    if unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, time.as_mut_ptr()) } != 0 {
        return Err(EffectAuthorityError::Clock);
    }
    let time = unsafe { time.assume_init() };
    let seconds = u64::try_from(time.tv_sec).map_err(|_| EffectAuthorityError::Clock)?;
    let nanos = u64::try_from(time.tv_nsec).map_err(|_| EffectAuthorityError::Clock)?;
    if nanos >= 1_000_000_000 {
        return Err(EffectAuthorityError::Clock);
    }
    seconds
        .checked_mul(1_000_000_000)
        .and_then(|seconds| seconds.checked_add(nanos))
        .ok_or(EffectAuthorityError::Clock)
}

fn deadline_from_samples(
    expires: Instant,
    clock_before: u64,
    instant_after: Instant,
) -> Result<u64, EffectAuthorityError> {
    let remaining = expires
        .checked_duration_since(instant_after)
        .ok_or(EffectAuthorityError::Expired)?;
    let remaining = u64::try_from(remaining.as_nanos()).map_err(|_| EffectAuthorityError::Clock)?;
    clock_before
        .checked_add(remaining)
        .ok_or(EffectAuthorityError::Clock)
}
fn time_namespace() -> Result<FileIdentity, EffectAuthorityError> {
    let metadata =
        std::fs::metadata("/proc/self/ns/time").map_err(|_| EffectAuthorityError::Clock)?;
    Ok(FileIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
    })
}

fn kind_code(kind: Kind) -> u8 {
    match kind {
        Kind::TerminalBell => 1,
        Kind::Application => 2,
        Kind::ManagedNotification => 3,
    }
}
fn cell_size() -> Result<usize, EffectAuthorityError> {
    let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    let page = usize::try_from(page).map_err(|_| EffectAuthorityError::InvalidDescriptor)?;
    if !(4096..=65536).contains(&page) || !page.is_power_of_two() {
        return Err(EffectAuthorityError::InvalidDescriptor);
    }
    Ok(page * 2)
}
fn put(bytes: &mut [u8], at: usize, value: u64) {
    bytes[at..at + 8].copy_from_slice(&value.to_le_bytes());
}
fn get(bytes: &[u8], at: usize) -> u64 {
    let mut value = [0u8; 8];
    value.copy_from_slice(&bytes[at..at + 8]);
    u64::from_le_bytes(value)
}
fn nonce() -> Result<[u8; 16], EffectAuthorityError> {
    let mut value = [0; 16];
    let mut read = 0;
    // finite retries even under repeated signals; failure never creates a lease.
    for _ in 0..32 {
        let count = unsafe {
            libc::getrandom(
                value[read..].as_mut_ptr().cast(),
                value.len() - read,
                libc::GRND_NONBLOCK,
            )
        };
        if count < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error.into());
        }
        if count == 0 {
            break;
        }
        read += count as usize;
        if read == value.len() && value != [0; 16] {
            return Ok(value);
        }
    }
    Err(EffectAuthorityError::Io(io::Error::other(
        "effect nonce unavailable",
    )))
}
fn memfd(size: usize) -> Result<OwnedFd, EffectAuthorityError> {
    let fd = unsafe {
        libc::memfd_create(
            c"among-effect-authority".as_ptr(),
            libc::MFD_CLOEXEC | libc::MFD_ALLOW_SEALING,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error().into());
    }
    let fd = unsafe { OwnedFd::from_raw_fd(fd) };
    if unsafe { libc::ftruncate(fd.as_raw_fd(), size as libc::off_t) } != 0 {
        return Err(io::Error::last_os_error().into());
    }
    Ok(fd)
}
fn seal(fd: &OwnedFd, seals: i32) -> Result<(), EffectAuthorityError> {
    if unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_ADD_SEALS, seals) } != 0 {
        return Err(io::Error::last_os_error().into());
    }
    Ok(())
}
fn cloexec(fd: &OwnedFd) -> Result<(), EffectAuthorityError> {
    if unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) } < 0 {
        return Err(io::Error::last_os_error().into());
    }
    Ok(())
}
fn read_only(fd: &OwnedFd) -> Result<OwnedFd, EffectAuthorityError> {
    let path = std::ffi::CString::new(format!("/proc/self/fd/{}", fd.as_raw_fd()))
        .map_err(|_| EffectAuthorityError::InvalidDescriptor)?;
    let read = unsafe {
        libc::open(
            path.as_ptr(),
            libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOCTTY,
        )
    };
    if read < 0 {
        return Err(io::Error::last_os_error().into());
    }
    Ok(unsafe { OwnedFd::from_raw_fd(read) })
}
fn duplicate(fd: &OwnedFd) -> Result<OwnedFd, EffectAuthorityError> {
    let duplicated = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 3) };
    if duplicated < 0 {
        return Err(io::Error::last_os_error().into());
    }
    Ok(unsafe { OwnedFd::from_raw_fd(duplicated) })
}
fn file_identity(
    fd: &OwnedFd,
    size: usize,
    seals: i32,
    readonly: bool,
) -> Result<FileIdentity, EffectAuthorityError> {
    let flags = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_GETFL) };
    let actual_seals = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_GET_SEALS) };
    let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
    if flags < 0
        || actual_seals < 0
        || unsafe { libc::fstat(fd.as_raw_fd(), stat.as_mut_ptr()) } != 0
    {
        return Err(EffectAuthorityError::InvalidDescriptor);
    }
    let stat = unsafe { stat.assume_init() };
    if (readonly && flags & libc::O_ACCMODE != libc::O_RDONLY)
        || actual_seals & seals != seals
        || stat.st_mode & libc::S_IFMT != libc::S_IFREG
        || stat.st_size != size as libc::off_t
        || stat.st_nlink != 0
        || stat.st_uid != unsafe { libc::geteuid() }
    {
        return Err(EffectAuthorityError::InvalidDescriptor);
    }
    Ok(FileIdentity {
        device: stat.st_dev,
        inode: stat.st_ino,
    })
}
fn write_manifest(fd: &OwnedFd, bytes: &[u8]) -> Result<(), EffectAuthorityError> {
    let mut offset = 0;
    for _ in 0..64 {
        let count = unsafe {
            libc::pwrite(
                fd.as_raw_fd(),
                bytes[offset..].as_ptr().cast(),
                bytes.len() - offset,
                offset as libc::off_t,
            )
        };
        if count < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error.into());
        }
        if count == 0 {
            break;
        }
        offset += count as usize;
        if offset == bytes.len() {
            return Ok(());
        }
    }
    Err(EffectAuthorityError::Io(io::Error::other(
        "effect manifest write incomplete",
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn delayed_clock_pair_is_conservative_and_overflow_never_mints_a_deadline() {
        let instant = Instant::now();
        let expires = instant + Duration::from_secs(10);
        let clock_before = 1_000_000_000;
        assert_eq!(
            deadline_from_samples(expires, clock_before, instant).unwrap(),
            11_000_000_000
        );
        assert_eq!(
            deadline_from_samples(expires, clock_before, instant + Duration::from_secs(3)).unwrap(),
            8_000_000_000
        );
        assert!(matches!(
            deadline_from_samples(expires, u64::MAX, instant),
            Err(EffectAuthorityError::Clock)
        ));
        assert!(matches!(
            deadline_from_samples(expires, clock_before, expires + Duration::from_nanos(1)),
            Err(EffectAuthorityError::Expired)
        ));
    }

    #[test]
    fn handle_reservation_is_bounded_and_failure_does_not_mutate_a_live_scope() {
        let scope = Scope::new(ScopeIdentity {
            kind: ScopeKind::Session,
            session: 1,
            key: 0,
            generation: 0,
        })
        .unwrap();
        assert!(matches!(
            HandleBudget::reserve(MAX_EFFECT_HANDLES),
            Err(EffectAuthorityError::Exhausted)
        ));
        assert!(matches!(
            HandleBudget::reserve(usize::MAX),
            Err(EffectAuthorityError::Exhausted)
        ));
        assert!(scope.is_current());
        scope.revoke();
        assert!(!scope.is_current());
    }

    #[test]
    fn real_delayed_clock_pair_shortens_instead_of_extending_original_lifetime() {
        let expires = Instant::now() + Duration::from_secs(10);
        let before = monotonic_now_ns().unwrap();
        std::thread::sleep(Duration::from_millis(30));
        let after = Instant::now();
        let deadline = deadline_from_samples(expires, before, after).unwrap();
        let fresh = monotonic_now_ns().unwrap();
        let correct_remaining = expires.duration_since(after).as_nanos();
        assert!(u128::from(deadline - fresh) + 20_000_000 < correct_remaining);
    }

    #[test]
    fn revision_overflow_revokes_old_authority_before_refusing_replacement() {
        let mut host = super::super::Host::new().unwrap();
        let channel = host
            .register_channel(super::super::Policy {
                enabled: true,
                eligibility: super::super::Eligibility::Always,
            })
            .unwrap();
        let state = host.channels.get_mut(&channel).unwrap();
        state.live.revoke();
        state.revision = u64::MAX;
        state.live = Scope::new(ScopeIdentity {
            kind: ScopeKind::Channel,
            session: channel.session,
            key: channel.id,
            generation: u64::MAX,
        })
        .unwrap();
        let now = Instant::now();
        host.application_event(super::super::Focus::default(), now)
            .unwrap();
        let job = host.take_ready(channel, now).unwrap();
        let effect = host.authorization(job).unwrap().effect(now).unwrap();
        let process = effect.export_process().unwrap();
        assert!(matches!(
            host.cancel_channel_work(channel),
            Err(super::super::Failure::Exhausted)
        ));
        assert!(!effect.is_current(now));
        assert!(matches!(
            process.validate_after_claim(),
            Err(EffectAuthorityError::Revoked)
        ));
        assert!(host.pending_jobs(channel).next().is_none());
    }
}
