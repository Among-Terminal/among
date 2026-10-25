//! user registrations/cache are separate from application-owned source authority.
//! neither selects installed code, rewrites preferences or grants runtime access.

use super::{
    official,
    source::{Inspection, Permit},
    store,
};
use crate::plugin::{
    data::{self, Problem},
    inventory,
    origin::{Change, Origin},
};
use crate::settings::Durability;
use serde::{Deserialize, Serialize};
use std::fs::File;
use std::io::Write;
use std::os::fd::AsRawFd;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub const MAX_REPOSITORIES: usize = 32;
pub const MAX_SNAPSHOTS: usize = 4;
pub const MAX_CACHE_BYTES: usize = 16 * 1024 * 1024;
// reserve the other half for the flushed atomic replacement. a refresh must not
// double a full cache beyond its total on-disk metadata budget.
const MAX_DOCUMENT_BYTES: usize = MAX_CACHE_BYTES / 2;
const STALE_SECONDS: u64 = 24 * 60 * 60;
pub const OFFICIAL_STORAGE_PROBLEM: Problem = Problem(
    "official source metadata is unsafe, corrupt or unavailable; retained custom registrations are unchanged",
);

mod official_storage;
pub(super) use official_storage::run;

#[derive(Clone, Copy)]
enum Storage {
    User,
    Official,
}

impl Storage {
    fn name(self) -> &'static str {
        match self {
            Self::User => "sources.json",
            Self::Official => "official.json",
        }
    }

    fn c_name(self) -> &'static std::ffi::CStr {
        match self {
            Self::User => c"sources.json",
            Self::Official => c"official.json",
        }
    }

    fn prefix(self) -> &'static str {
        match self {
            Self::User => "sources-",
            Self::Official => "official-",
        }
    }

    fn other(self) -> Self {
        match self {
            Self::User => Self::Official,
            Self::Official => Self::User,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Snapshot {
    origin: Origin,
    revision: String,
    retrieved_at: u64,
    sha256: String,
    catalogue: String,
}

impl Snapshot {
    fn inspection(&self) -> Result<Inspection, Problem> {
        if self.retrieved_at == 0
            || self.retrieved_at > i64::MAX as u64
            || inventory::checksum(self.catalogue.as_bytes())? != self.sha256
        {
            return Err(Problem("cached metadata timestamp or digest is invalid"));
        }
        let mut inspection = Inspection::from_bytes(
            self.origin.clone(),
            self.revision.clone(),
            self.catalogue.as_bytes().to_vec(),
        )?;
        inspection.retrieved_at = self.retrieved_at;
        Ok(inspection)
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Registration {
    pub origin: Origin,
    pub token: String,
    pub revision: String,
    snapshots: Vec<Snapshot>,
}

impl Registration {
    pub fn is_official(&self) -> bool {
        self.origin.repository_id == official::REPOSITORY_ID
    }
    pub fn display_origin(&self) -> Origin {
        if self.is_official() {
            official::origin()
        } else {
            self.origin.clone()
        }
    }
    pub fn display_title(&self) -> String {
        if self.is_official() {
            official::TITLE.into()
        } else {
            format!("{}/{}", self.origin.owner, self.origin.repository)
        }
    }
    pub fn available(&self) -> Result<(), Problem> {
        official::validate(&self.origin)
    }
    pub fn snapshot_count(&self) -> usize {
        self.snapshots.len()
    }
    pub fn retrieved_at(&self) -> Option<u64> {
        self.snapshots.first().map(|snapshot| snapshot.retrieved_at)
    }
    pub fn stale_at(&self, now: u64) -> bool {
        self.retrieved_at().is_none_or(|retrieved| {
            now.checked_sub(retrieved)
                .is_none_or(|age| age >= STALE_SECONDS)
        })
    }
}

pub fn official_source() -> Registration {
    Registration {
        origin: official::origin(),
        // a stable virtual generation permits a first explicit refresh without
        // persisting a registration merely because preferences was opened.
        token: "0".repeat(64),
        revision: "0".repeat(64),
        snapshots: Vec::new(),
    }
}

pub fn effective_sources(mut registrations: Vec<Registration>) -> Vec<Registration> {
    let official = registrations
        .iter()
        .position(Registration::is_official)
        .map(|index| registrations.remove(index))
        .unwrap_or_else(official_source);
    registrations.insert(0, official);
    registrations
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Document {
    version: u32,
    registrations: Vec<Registration>,
}

pub enum Request {
    // retain the persisted user-registry view; application source discovery uses
    // the separate overlay so neither corrupt storage nor omission revokes it.
    List,
    EffectiveList,
    Add(Box<Inspection>),
    Refresh {
        expected: Registration,
        inspection: Box<Inspection>,
    },
    ApprovePublisher {
        expected: Registration,
        inspection: Box<Inspection>,
    },
    Remove(Registration),
    Cached(Registration),
}

pub enum Outcome {
    List(Vec<Registration>),
    EffectiveList {
        registrations: Vec<Registration>,
        problem: Option<Problem>,
    },
    Changed {
        registrations: Vec<Registration>,
        durability: Durability,
        problem: Option<Problem>,
    },
    Cached {
        inspection: Inspection,
        retrieved_at: u64,
        stale: bool,
    },
}

pub fn root() -> Option<PathBuf> {
    inventory::root().and_then(|root| root.parent().map(|parent| parent.join("plugin-sources")))
}

pub fn now() -> Result<u64, Problem> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|time| time.as_secs())
        .map_err(|_| {
            Problem("system time is unavailable; metadata freshness cannot be established")
        })
}

pub struct Job {
    state: Arc<AtomicU8>,
    result: mpsc::Receiver<Result<Outcome, Problem>>,
}

impl Job {
    pub fn start(root: PathBuf, request: Request) -> Result<Self, Problem> {
        let permit = Permit::acquire()?;
        let state = Arc::new(AtomicU8::new(0));
        let worker_state = state.clone();
        let (send, result) = mpsc::sync_channel(1);
        std::thread::Builder::new()
            .name("among-sources".into())
            .spawn(move || {
                let _permit = permit;
                let deadline = Instant::now() + Duration::from_secs(60);
                let check = || {
                    if worker_state.load(Ordering::Acquire) == 3 {
                        Err(Problem("source operation cancelled"))
                    } else if Instant::now() >= deadline {
                        Err(Problem("source operation exceeded its deadline"))
                    } else {
                        Ok(())
                    }
                };
                let mut outcome = run(&root, request, check, || {
                    worker_state
                        .compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire)
                        .map_err(|_| Problem("source operation cancelled"))?;
                    Ok(())
                });
                if outcome.is_ok() && worker_state.load(Ordering::Acquire) != 1 {
                    if let Err(problem) = check() {
                        outcome = Err(problem);
                    }
                }
                if outcome.is_ok() {
                    match worker_state.compare_exchange(0, 2, Ordering::AcqRel, Ordering::Acquire) {
                        Ok(_) => {}
                        Err(1) => worker_state.store(2, Ordering::Release),
                        Err(3) => outcome = Err(Problem("source operation cancelled")),
                        Err(_) => {
                            outcome = Err(Problem("invalid source operation completion authority"))
                        }
                    }
                }
                let _ = send.send(outcome);
            })
            .map_err(|_| Problem("cannot start bounded source storage worker"))?;
        Ok(Self { state, result })
    }

    pub fn cancel(&self) -> bool {
        self.state
            .compare_exchange(0, 3, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }

    pub fn poll(&self) -> Option<Result<Outcome, Problem>> {
        match self.result.try_recv() {
            Ok(result) => Some(result),
            Err(mpsc::TryRecvError::Empty) => None,
            Err(mpsc::TryRecvError::Disconnected) => Some(Err(Problem(
                "source storage worker stopped without a result",
            ))),
        }
    }
}

impl Drop for Job {
    fn drop(&mut self) {
        self.cancel();
    }
}

fn token(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn read_bytes(root: &Path) -> Result<Option<Vec<u8>>, Problem> {
    read_storage_bytes(root, Storage::User)
}

fn read_storage_bytes(root: &Path, storage: Storage) -> Result<Option<Vec<u8>>, Problem> {
    if matches!(std::fs::symlink_metadata(root), Err(error) if error.kind() == std::io::ErrorKind::NotFound)
    {
        return Ok(None);
    }
    let directory = data::owned_directory(root)?;
    // absence is distinguished from every other open error: malformed/unsafe
    // documents are never repaired as an empty registry by a later edit.
    let name = storage.c_name();
    let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
    if unsafe {
        libc::fstatat(
            directory.as_raw_fd(),
            name.as_ptr(),
            stat.as_mut_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    } != 0
    {
        return if std::io::Error::last_os_error().raw_os_error() == Some(libc::ENOENT) {
            Ok(None)
        } else {
            Err(Problem("cannot inspect source registry"))
        };
    }
    data::read_bounded(
        &data::contained_file(&directory, storage.name())?,
        MAX_DOCUMENT_BYTES,
    )
    .map(Some)
}

fn read(
    root: &Path,
    check: &impl Fn() -> Result<(), Problem>,
) -> Result<(Document, Option<Vec<u8>>), Problem> {
    read_storage(root, Storage::User, check)
}

fn read_storage(
    root: &Path,
    storage: Storage,
    check: &impl Fn() -> Result<(), Problem>,
) -> Result<(Document, Option<Vec<u8>>), Problem> {
    let bytes = read_storage_bytes(root, storage)?;
    let document: Document = match &bytes {
        Some(bytes) => serde_json::from_value(data::parse_unique(bytes, MAX_DOCUMENT_BYTES)?)
            .map_err(|_| Problem("invalid source registry document"))?,
        None => Document {
            version: 1,
            registrations: Vec::new(),
        },
    };
    let valid_count = match storage {
        Storage::User => document.registrations.len() <= MAX_REPOSITORIES,
        Storage::Official => {
            document.registrations.len() <= 1
                && document.registrations.iter().all(Registration::is_official)
        }
    };
    if document.version != 1 || !valid_count {
        return Err(Problem(
            "unsupported source registry version or repository count",
        ));
    }
    let mut ids = std::collections::BTreeSet::new();
    for registration in &document.registrations {
        check()?;
        registration.origin.validate()?;
        if !ids.insert(registration.origin.repository_id)
            || !token(&registration.token)
            || !token(&registration.revision)
            || registration.snapshots.len() > MAX_SNAPSHOTS
        {
            return Err(Problem(
                "duplicate source identity or invalid registration generation/cache count",
            ));
        }
        for (index, snapshot) in registration.snapshots.iter().enumerate() {
            check()?;
            if snapshot.origin.repository_id != registration.origin.repository_id
                || (index == 0 && snapshot.origin.publisher_id != registration.origin.publisher_id)
            {
                return Err(Problem("cached source origin disagrees with registration"));
            }
            snapshot.inspection()?;
        }
    }
    Ok((document, bytes))
}

fn save(
    root: &Path,
    directory: &File,
    previous: Option<Vec<u8>>,
    bytes: &[u8],
    check: &impl Fn() -> Result<(), Problem>,
    begin_commit: impl FnOnce() -> Result<(), Problem>,
) -> Result<Durability, Problem> {
    save_with_confirmation(
        root,
        directory,
        previous,
        bytes,
        check,
        begin_commit,
        File::sync_all,
    )
}

fn save_with_confirmation(
    root: &Path,
    directory: &File,
    previous: Option<Vec<u8>>,
    bytes: &[u8],
    check: &impl Fn() -> Result<(), Problem>,
    begin_commit: impl FnOnce() -> Result<(), Problem>,
    confirm: impl FnOnce(&File) -> std::io::Result<()>,
) -> Result<Durability, Problem> {
    save_storage_with_confirmation(
        root,
        directory,
        (Storage::User, previous),
        bytes,
        check,
        begin_commit,
        confirm,
    )
}

// quota accounting must not open a corrupt/linked sibling or follow its target.
// retain its no-follow identity so external replacement cannot invalidate the
// measured publication allowance while the temporary is being flushed.
#[derive(PartialEq, Eq)]
struct BudgetEntry {
    device: u64,
    inode: u64,
    size: i64,
    mode: u32,
    owner: u32,
    group: u32,
    links: u64,
    modified: (i64, i64),
    changed: (i64, i64),
}

fn budget_entry(directory: &File, storage: Storage) -> Result<Option<BudgetEntry>, Problem> {
    let mut metadata = std::mem::MaybeUninit::<libc::stat>::uninit();
    if unsafe {
        libc::fstatat(
            directory.as_raw_fd(),
            storage.c_name().as_ptr(),
            metadata.as_mut_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    } != 0
    {
        return if std::io::Error::last_os_error().raw_os_error() == Some(libc::ENOENT) {
            Ok(None)
        } else {
            Err(Problem("cannot measure retained source metadata"))
        };
    }
    let metadata = unsafe { metadata.assume_init() };
    if metadata.st_size < 0 || metadata.st_size as u64 > MAX_DOCUMENT_BYTES as u64 {
        return Err(Problem("retained source metadata exceeds its byte budget"));
    }
    Ok(Some(BudgetEntry {
        device: metadata.st_dev,
        inode: metadata.st_ino,
        size: metadata.st_size,
        mode: metadata.st_mode,
        owner: metadata.st_uid,
        group: metadata.st_gid,
        links: metadata.st_nlink,
        modified: (metadata.st_mtime, metadata.st_mtime_nsec),
        changed: (metadata.st_ctime, metadata.st_ctime_nsec),
    }))
}

fn replacement_limit(previous: usize, other: &Option<BudgetEntry>) -> Result<usize, Problem> {
    let retained = previous
        .checked_add(other.as_ref().map_or(0, |entry| entry.size as usize))
        .ok_or(Problem("source metadata size overflow"))?;
    MAX_CACHE_BYTES
        .checked_sub(retained)
        .map(|remaining| remaining.min(MAX_DOCUMENT_BYTES))
        .ok_or(Problem(
            "retained source metadata exceeds its total byte budget",
        ))
}

fn write_limit(
    directory: &File,
    storage: Storage,
    previous: Option<&[u8]>,
) -> Result<usize, Problem> {
    replacement_limit(
        previous.map_or(0, <[u8]>::len),
        &budget_entry(directory, storage.other())?,
    )
}

fn save_storage_with_confirmation(
    root: &Path,
    directory: &File,
    state: (Storage, Option<Vec<u8>>),
    bytes: &[u8],
    check: &impl Fn() -> Result<(), Problem>,
    begin_commit: impl FnOnce() -> Result<(), Problem>,
    confirm: impl FnOnce(&File) -> std::io::Result<()>,
) -> Result<Durability, Problem> {
    let (storage, previous) = state;
    let other = budget_entry(directory, storage.other())?;
    if bytes.len() > replacement_limit(previous.as_ref().map_or(0, Vec::len), &other)? {
        return Err(Problem(
            "source document exceeds its atomic replacement budget",
        ));
    }
    let temporary = format!("{}{}", storage.prefix(), store::token());
    let mut file = store::file(directory, &temporary, true)?;
    let outcome = (|| {
        file.write_all(bytes)
            .map_err(|_| Problem("cannot write source registry"))?;
        file.sync_all()
            .map_err(|_| Problem("cannot flush source registry"))?;
        check()?;
        begin_commit()?;
        let pinned = directory
            .metadata()
            .map_err(|_| Problem("cannot inspect pinned source directory"))?;
        let current = data::owned_directory(root)?
            .metadata()
            .map_err(|_| Problem("cannot inspect current source directory"))?;
        if (pinned.dev(), pinned.ino()) != (current.dev(), current.ino()) {
            return Err(Problem(
                "source directory changed externally; refusing hidden or retargeted publication",
            ));
        }
        if read_storage_bytes(root, storage)? != previous {
            return Err(Problem(
                "source registry changed externally; refusing replacement",
            ));
        }
        if budget_entry(directory, storage.other())? != other {
            return Err(Problem(
                "other source metadata changed externally; refusing stale publication budget",
            ));
        }
        let from = std::ffi::CString::new(temporary.as_str())
            .map_err(|_| Problem("invalid source temporary name"))?;
        if unsafe {
            libc::renameat(
                directory.as_raw_fd(),
                from.as_ptr(),
                directory.as_raw_fd(),
                storage.c_name().as_ptr(),
            )
        } != 0
        {
            return Err(Problem("cannot commit source registry"));
        }
        Ok(if confirm(directory).is_ok() {
            Durability::Durable
        } else {
            Durability::Uncertain
        })
    })();
    if let Ok(name) = std::ffi::CString::new(temporary) {
        unsafe {
            libc::unlinkat(directory.as_raw_fd(), name.as_ptr(), 0);
        }
    }
    outcome
}

fn clean_temporaries(
    directory: &File,
    check: &impl Fn() -> Result<(), Problem>,
) -> Result<(), Problem> {
    let entries = std::fs::read_dir(format!("/proc/self/fd/{}", directory.as_raw_fd()))
        .map_err(|_| Problem("cannot inspect source storage namespace"))?;
    let mut changed = false;
    for (count, entry) in entries.enumerate() {
        check()?;
        if count >= 256 {
            return Err(Problem("source storage namespace exceeds its entry budget"));
        }
        let entry = entry.map_err(|_| Problem("cannot inspect source storage namespace"))?;
        let name = entry.file_name();
        let Some(text) = name.to_str().filter(|name| {
            name.strip_prefix("sources-")
                .or_else(|| name.strip_prefix("official-"))
                .is_some_and(token)
        }) else {
            continue;
        };
        let file = data::contained_file(directory, text)?;
        if file
            .metadata()
            .map_err(|_| Problem("cannot inspect abandoned source temporary"))?
            .len()
            > MAX_DOCUMENT_BYTES as u64
        {
            return Err(Problem(
                "abandoned source temporary exceeds its byte budget",
            ));
        }
        let name =
            std::ffi::CString::new(text).map_err(|_| Problem("invalid source temporary name"))?;
        if unsafe { libc::unlinkat(directory.as_raw_fd(), name.as_ptr(), 0) } != 0 {
            return Err(Problem("abandoned source temporary cleanup is deferred"));
        }
        changed = true;
    }
    if changed {
        directory
            .sync_all()
            .map_err(|_| Problem("cannot confirm abandoned source temporary cleanup"))?;
    }
    Ok(())
}

fn lock_registry(lock: &File, check: &impl Fn() -> Result<(), Problem>) -> Result<(), Problem> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        check()?;
        if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
            return Ok(());
        }
        let error = std::io::Error::last_os_error().raw_os_error();
        if !matches!(error, Some(libc::EWOULDBLOCK | libc::EINTR)) {
            return Err(Problem("cannot acquire source registry operation lock"));
        }
        if Instant::now() >= deadline {
            return Err(Problem(
                "another source registry edit is active; retry after it finishes",
            ));
        }
        // forked children can briefly retain an open-file-description lock even
        // with CLOEXEC. bounded off-loop waiting avoids treating that as damage.
        std::thread::sleep(Duration::from_millis(2));
    }
}

fn trim(
    document: &mut Document,
    protected: (u64, &str, u64),
    limit: usize,
    check: &impl Fn() -> Result<(), Problem>,
) -> Result<Vec<u8>, Problem> {
    let bytes =
        serde_json::to_vec(document).map_err(|_| Problem("cannot encode source registry"))?;
    let mut estimate = bytes.len();
    while estimate > limit {
        check()?;
        let candidate = document
            .registrations
            .iter()
            .enumerate()
            .flat_map(|(source, entry)| {
                entry
                    .snapshots
                    .iter()
                    .enumerate()
                    .map(move |(snapshot, value)| {
                        (source, snapshot, entry.origin.repository_id, value)
                    })
            })
            .filter(|(_, _, repository, value)| {
                (*repository, value.sha256.as_str(), value.retrieved_at) != protected
            })
            .min_by_key(|(_, index, _, value)| (value.retrieved_at, std::cmp::Reverse(*index)))
            .map(|(source, snapshot, _, _)| (source, snapshot))
            .ok_or(Problem(
                "source metadata cache cannot fit within its byte budget",
            ))?;
        let removed = document.registrations[candidate.0]
            .snapshots
            .remove(candidate.1);
        estimate = estimate.saturating_sub(
            serde_json::to_vec(&removed)
                .map_err(|_| Problem("cannot measure source cache"))?
                .len(),
        );
    }
    let bytes =
        serde_json::to_vec(document).map_err(|_| Problem("cannot encode source registry"))?;
    if bytes.len() > limit {
        return Err(Problem("source metadata cache exceeds its byte budget"));
    }
    Ok(bytes)
}

fn run_user(
    root: &Path,
    request: Request,
    check: impl Fn() -> Result<(), Problem>,
    begin_commit: impl FnOnce() -> Result<(), Problem>,
) -> Result<Outcome, Problem> {
    check()?;
    if matches!(&request, Request::List | Request::Cached(_)) {
        let (document, _) = read(root, &check)?;
        check()?;
        return match request {
            Request::List => Ok(Outcome::List(document.registrations)),
            Request::Cached(expected) => {
                let current = document
                    .registrations
                    .iter()
                    .find(|source| **source == expected)
                    .ok_or(Problem(
                        "source was removed or changed; reopen its current registration",
                    ))?;
                current.available()?;
                let snapshot = current.snapshots.first().ok_or(Problem(
                    "no retained metadata snapshot; refresh this source when online",
                ))?;
                Ok(Outcome::Cached {
                    inspection: snapshot.inspection()?,
                    retrieved_at: snapshot.retrieved_at,
                    stale: now().map_or(true, |now| current.stale_at(now)),
                })
            }
            _ => Err(Problem("invalid source read request")),
        };
    }
    let directory = store::open_root(root)?;
    let lock = store::file(&directory, "sources.lock", false)?;
    lock_registry(&lock, &check)?;
    clean_temporaries(&directory, &check)?;
    let (mut document, previous) = read(root, &check)?;
    let official = official_storage::cached(root, &check);
    let (inspection, expected, approve) = match request {
        Request::Add(inspection) => (Some(inspection), None, false),
        Request::Refresh {
            expected,
            inspection,
        } => (Some(inspection), Some(expected), false),
        Request::ApprovePublisher {
            expected,
            inspection,
        } => (Some(inspection), Some(expected), true),
        Request::Remove(expected) => (None, Some(expected), false),
        _ => return Err(Problem("invalid source write request")),
    };
    let index = if let Some(expected) = &expected {
        Some(
            document
                .registrations
                .iter()
                .position(|source| source == expected)
                .ok_or(Problem(
                    "source was removed or changed; stale operation was refused",
                ))?,
        )
    } else {
        None
    };
    let mut protected = None;
    if let Some(inspection) = inspection {
        // reparse exact cached bytes before they gain persistence authority.
        let validated = Inspection::from_bytes(
            inspection.origin(),
            inspection.catalogue_revision.clone(),
            inspection.catalogue_bytes.clone(),
        )?;
        if validated.catalogue != inspection.catalogue {
            return Err(Problem("inspection and raw metadata disagree"));
        }
        if inspection.retrieved_at == 0 || inspection.retrieved_at > i64::MAX as u64 {
            return Err(Problem("invalid metadata retrieval time"));
        }
        let origin = inspection.origin();
        official::validate(&origin)?;
        if expected
            .as_ref()
            .is_some_and(|expected| expected.origin.repository_id != origin.repository_id)
        {
            return Err(Problem(
                "numeric source identity changed; add it as a separate source explicitly",
            ));
        }
        let snapshot = Snapshot {
            origin: origin.clone(),
            revision: inspection.catalogue_revision.clone(),
            retrieved_at: inspection.retrieved_at,
            sha256: inventory::checksum(&inspection.catalogue_bytes)?,
            catalogue: String::from_utf8(inspection.catalogue_bytes.clone())
                .map_err(|_| Problem("catalogue is not UTF-8"))?,
        };
        protected = Some((
            origin.repository_id,
            snapshot.sha256.clone(),
            snapshot.retrieved_at,
        ));
        let registration = match index {
            Some(index) => &mut document.registrations[index],
            None => {
                if document
                    .registrations
                    .iter()
                    .any(|source| source.origin.repository_id == origin.repository_id)
                {
                    return Err(Problem(
                        "repository is already registered; use its Refresh action",
                    ));
                }
                if document.registrations.len() >= MAX_REPOSITORIES {
                    return Err(Problem(
                        "source repository limit is 32; remove an unused registration first",
                    ));
                }
                document.registrations.push(Registration {
                    origin: origin.clone(),
                    token: store::token(),
                    revision: store::token(),
                    snapshots: Vec::new(),
                });
                document
                    .registrations
                    .last_mut()
                    .ok_or(Problem("source registration is unavailable"))?
            }
        };
        match origin.compare(&registration.origin) {
            Change::IdentityChanged => return Err(Problem("numeric source identity changed; add it as a separate source explicitly")),
            Change::PublisherChanged if !approve => return Err(Problem("publisher changed; inspect it and explicitly approve the new publisher before saving")),
            Change::PublisherChanged => { registration.token = store::token(); registration.snapshots.clear(); }
            _ => {}
        }
        registration.origin = origin;
        registration.revision = store::token();
        registration.snapshots.retain(|old| {
            old.revision != snapshot.revision
                || old.sha256 != snapshot.sha256
                || old.origin != snapshot.origin
        });
        registration.snapshots.insert(0, snapshot);
        registration.snapshots.truncate(MAX_SNAPSHOTS);
    } else if let Some(index) = index {
        document.registrations.remove(index);
    }
    let bytes = match protected.as_ref() {
        Some((repository, digest, retrieved)) => trim(
            &mut document,
            (*repository, digest, *retrieved),
            write_limit(&directory, Storage::User, previous.as_deref())?,
            &check,
        )?,
        None => {
            serde_json::to_vec(&document).map_err(|_| Problem("cannot encode source registry"))?
        }
    };
    let (registrations, problem) = official_storage::overlay(document.registrations, official);
    let durability = save(root, &directory, previous, &bytes, &check, begin_commit)?;
    Ok(Outcome::Changed {
        registrations,
        durability,
        problem,
    })
}

#[cfg(test)]
mod tests;
