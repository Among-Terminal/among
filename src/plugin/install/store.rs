//! a complete generation precedes selection. failure before selection leaves at
//! most an unselected owned generation; it never changes the installed identity.

use std::ffi::{CString, OsStr, OsString};
use std::fs::File;
use std::io::Write;
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, Instant};

use crate::plugin::data::{self, Problem};
use crate::plugin::inventory::{self, Receipt};
use crate::settings::Durability;

const STORE_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const STAGING_BYTES: u64 = 512 * 1024 * 1024;
const MAX_STORE_ENTRIES: usize = 1_048_576;

#[derive(Clone, Debug)]
pub struct Commit {
    pub receipt: Receipt,
    pub durability: Durability,
}

pub(crate) fn removal_pending(root: &Path, identity: &str) -> bool {
    super::operations::Ticket::removal_pending(root, identity)
}

pub(crate) fn while_uninstalled<T>(
    root_path: &Path,
    identity: crate::plugin::manifest::ManagedIdentity,
    edit: impl FnOnce() -> T,
) -> Result<T, Problem> {
    let ticket = super::operations::Ticket::acquire(root_path, identity.clone())?;
    let root = open_root(root_path)?;
    let lock = file(&root, "install.lock", false)?;
    // this path runs on a preference action, so a competing worker is a retry,
    // not a five-second main-loop wait. hold the commit lock through the edit.
    if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        return Err(Problem(
            "package store is busy; retry settings deletion after its operation finishes",
        ));
    }
    verify_root(&root, root_path)?;
    if ticket.cancelled.load(std::sync::atomic::Ordering::Acquire)
        || document(&root)?
            .installations
            .iter()
            .any(|receipt| receipt.identity().ok() == Some(identity.clone()))
    {
        return Err(Problem(
            "plugin is installed or changed; uninstall it before deleting retained settings",
        ));
    }
    Ok(edit())
}

pub(super) struct Transaction {
    root: File,
    root_path: PathBuf,
    initial_revision: Option<String>,
    _lock: File,
    staging: File,
    stage: File,
    name: String,
    path: PathBuf,
    pub archive: File,
    pub payload: File,
}

fn lock_exclusive(lock: &File, check: &impl Fn() -> Result<(), Problem>) -> Result<(), Problem> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        check()?;
        if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
            return Ok(());
        }
        if !matches!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::EWOULDBLOCK | libc::EINTR)
        ) {
            return Err(Problem("cannot acquire managed-store operation lock"));
        }
        if Instant::now() >= deadline {
            return Err(Problem(
                "another managed-store operation is active; wait before retrying",
            ));
        }
        std::thread::sleep(Duration::from_millis(2));
    }
}

fn verify_root(root: &File, path: &Path) -> Result<(), Problem> {
    let pinned = root
        .metadata()
        .map_err(|_| Problem("cannot inspect pinned managed root"))?;
    let current = open_existing_root(path)?
        .metadata()
        .map_err(|_| Problem("cannot inspect current managed root"))?;
    if (pinned.dev(), pinned.ino()) != (current.dev(), current.ino()) {
        return Err(Problem(
            "managed-store root changed externally; refusing hidden publication",
        ));
    }
    Ok(())
}

pub(crate) fn lease(
    root: &File,
    receipt: &Receipt,
) -> Result<Option<std::sync::Arc<File>>, Problem> {
    let path = format!(
        "generations/{}/{}/{}",
        receipt.repository_id, receipt.plugin_id, receipt.generation
    );
    let package = match data::contained_directory(root, &path) {
        Ok(package) => package,
        Err(_) => return Ok(None),
    };
    if !exists(&package, ".among-receipt.json")? {
        return Ok(None);
    }
    let seal: Receipt = serde_json::from_value(data::parse_unique(
        &data::read_bounded(
            &data::contained_file(&package, ".among-receipt.json")?,
            data::MAX_STATIC_BYTES,
        )?,
        data::MAX_STATIC_BYTES,
    )?)
    .map_err(|_| Problem("invalid owned generation receipt"))?;
    if &seal != receipt {
        return Err(Problem("owned generation receipt disagrees with selection"));
    }
    let lease = data::contained_file(&package, ".among-lease")?;
    if unsafe { libc::flock(lease.as_raw_fd(), libc::LOCK_SH | libc::LOCK_NB) } != 0 {
        return Err(Problem(
            "generation cleanup is already active; reopen current inventory",
        ));
    }
    Ok(Some(std::sync::Arc::new(lease)))
}

fn exists(parent: &File, component: &str) -> Result<bool, Problem> {
    let component = name(OsStr::new(component))?;
    let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
    if unsafe {
        libc::fstatat(
            parent.as_raw_fd(),
            component.as_ptr(),
            stat.as_mut_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    } == 0
    {
        return Ok(true);
    }
    if std::io::Error::last_os_error().raw_os_error() == Some(libc::ENOENT) {
        Ok(false)
    } else {
        Err(Problem("cannot inspect managed-store entry"))
    }
}

pub(super) fn token() -> String {
    format!(
        "{}{}",
        glib::uuid_string_random(),
        glib::uuid_string_random()
    )
    .replace('-', "")
}

fn name(value: &OsStr) -> Result<CString, Problem> {
    if value.as_bytes().contains(&b'/') || matches!(value.as_bytes(), b"" | b"." | b"..") {
        return Err(Problem("unsafe managed-store component"));
    }
    CString::new(value.as_bytes()).map_err(|_| Problem("unsafe managed-store component"))
}

fn owned(file: File, directory: bool) -> Result<File, Problem> {
    let metadata = file
        .metadata()
        .map_err(|_| Problem("cannot inspect managed-store entry"))?;
    if metadata.uid() != unsafe { libc::geteuid() }
        || metadata.mode() & 0o7022 != 0
        || if directory {
            !metadata.is_dir()
        } else {
            !metadata.is_file() || metadata.nlink() != 1
        }
    {
        return Err(Problem(
            "managed-store entry has unsafe ownership, type or permissions",
        ));
    }
    Ok(file)
}

pub(super) fn directory(parent: &File, component: &str) -> Result<File, Problem> {
    let component = name(OsStr::new(component))?;
    if unsafe { libc::mkdirat(parent.as_raw_fd(), component.as_ptr(), 0o700) } == 0 {
        parent
            .sync_all()
            .map_err(|_| Problem("cannot flush new managed-store directory"))?;
    } else if std::io::Error::last_os_error().raw_os_error() != Some(libc::EEXIST) {
        return Err(Problem("cannot create managed-store directory"));
    }
    let fd = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            component.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return Err(Problem("managed-store directory is unavailable or a link"));
    }
    owned(unsafe { File::from_raw_fd(fd) }, true)
}

pub(super) fn open_root(path: &Path) -> Result<File, Problem> {
    open_root_with_creation(path, true)
}

pub(crate) fn open_existing_root(path: &Path) -> Result<File, Problem> {
    open_root_with_creation(path, false)
}

fn open_root_with_creation(path: &Path, create: bool) -> Result<File, Problem> {
    if !path.is_absolute() {
        return Err(Problem("managed-store root must be absolute"));
    }
    let mut parent = File::open("/").map_err(|_| Problem("cannot open filesystem root"))?;
    for part in path.components() {
        let Component::Normal(part) = part else {
            if matches!(part, Component::RootDir) {
                continue;
            }
            return Err(Problem("unsafe managed-store root"));
        };
        let component = name(part)?;
        if create && unsafe { libc::mkdirat(parent.as_raw_fd(), component.as_ptr(), 0o700) } == 0 {
            parent
                .sync_all()
                .map_err(|_| Problem("cannot flush new managed-store root"))?;
        } else if create && std::io::Error::last_os_error().raw_os_error() != Some(libc::EEXIST) {
            return Err(Problem("cannot create managed-store root"));
        }
        let fd = unsafe {
            libc::openat(
                parent.as_raw_fd(),
                component.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        if fd < 0 {
            return Err(Problem(
                "managed-store root contains an unavailable directory or link",
            ));
        }
        parent = unsafe { File::from_raw_fd(fd) };
    }
    owned(parent, true)
}

pub(super) fn file(parent: &File, component: &str, exclusive: bool) -> Result<File, Problem> {
    let component = name(OsStr::new(component))?;
    let fd = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            component.as_ptr(),
            libc::O_RDWR
                | libc::O_CREAT
                | libc::O_NOFOLLOW
                | libc::O_NONBLOCK
                | libc::O_CLOEXEC
                | if exclusive { libc::O_EXCL } else { 0 },
            0o600,
        )
    };
    if fd < 0 {
        return Err(Problem("cannot create owned managed-store file"));
    }
    owned(unsafe { File::from_raw_fd(fd) }, false)
}

fn children(directory: &File) -> Result<Vec<OsString>, Problem> {
    let entries = std::fs::read_dir(format!("/proc/self/fd/{}", directory.as_raw_fd()))
        .map_err(|_| Problem("cannot enumerate managed-store directory"))?;
    let mut names = Vec::new();
    for entry in entries {
        if names.len() >= 131_072 {
            return Err(Problem("managed-store entry count exceeds its budget"));
        }
        names.push(
            entry
                .map_err(|_| Problem("cannot enumerate managed-store directory"))?
                .file_name(),
        );
    }
    Ok(names)
}

fn open_child(parent: &File, component: &OsStr) -> Result<File, Problem> {
    let component = name(component)?;
    let fd = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            component.as_ptr(),
            libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK,
        )
    };
    if fd < 0 {
        return Err(Problem("unsafe or unreadable managed-store resource"));
    }
    let file = unsafe { File::from_raw_fd(fd) };
    let directory = file
        .metadata()
        .map_err(|_| Problem("cannot inspect managed-store resource"))?
        .is_dir();
    owned(file, directory)
}

fn measure(
    directory: &File,
    depth: usize,
    count: &mut usize,
    check: &impl Fn() -> Result<(), Problem>,
) -> Result<u64, Problem> {
    if depth > 24 {
        return Err(Problem("managed-store nesting exceeds its budget"));
    }
    let mut bytes = 0u64;
    for component in children(directory)? {
        check()?;
        *count += 1;
        if *count > MAX_STORE_ENTRIES {
            return Err(Problem(
                "managed-store total entry count exceeds its budget",
            ));
        }
        let file = open_child(directory, &component)?;
        let metadata = file
            .metadata()
            .map_err(|_| Problem("cannot inspect managed-store resource"))?;
        let length = if metadata.is_dir() {
            measure(&file, depth + 1, count, check)?
        } else {
            metadata.len()
        };
        bytes = bytes
            .checked_add(length)
            .filter(|bytes| *bytes <= STORE_BYTES)
            .ok_or(Problem("managed-store bytes exceed their budget"))?;
    }
    Ok(bytes)
}

fn sync_tree(
    directory: &File,
    depth: usize,
    count: &mut usize,
    check: &impl Fn() -> Result<(), Problem>,
) -> Result<(), Problem> {
    if depth > 24 {
        return Err(Problem("staged directory nesting exceeds its budget"));
    }
    for component in children(directory)? {
        check()?;
        *count += 1;
        if *count > MAX_STORE_ENTRIES {
            return Err(Problem("staged directory entry count exceeds its budget"));
        }
        let child = open_child(directory, &component)?;
        if child
            .metadata()
            .map_err(|_| Problem("cannot inspect staged resource"))?
            .is_dir()
        {
            sync_tree(&child, depth + 1, count, check)?;
        }
    }
    directory
        .sync_all()
        .map_err(|_| Problem("cannot flush staged directories"))
}

fn remove_tree(
    parent: &File,
    component: &OsStr,
    depth: usize,
    count: &mut usize,
    deadline: Instant,
) -> Result<(), Problem> {
    *count += 1;
    if depth > 24 || *count > 65_536 || Instant::now() >= deadline {
        return Err(Problem("staging cleanup nesting exceeds its budget"));
    }
    let child = open_child(parent, component)?;
    let is_directory = child
        .metadata()
        .map_err(|_| Problem("cannot inspect staging cleanup entry"))?
        .is_dir();
    if is_directory {
        for name in children(&child)? {
            remove_tree(&child, &name, depth + 1, count, deadline)?;
        }
    }
    let component = name(component)?;
    if unsafe {
        libc::unlinkat(
            parent.as_raw_fd(),
            component.as_ptr(),
            if is_directory { libc::AT_REMOVEDIR } else { 0 },
        )
    } != 0
    {
        return Err(Problem("owned staging cleanup is deferred"));
    }
    Ok(())
}

fn selection_bytes(root: &File) -> Result<Option<Vec<u8>>, Problem> {
    let path = CString::new("installed.json").map_err(|_| Problem("invalid selection filename"))?;
    let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
    if unsafe {
        libc::fstatat(
            root.as_raw_fd(),
            path.as_ptr(),
            stat.as_mut_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    } != 0
    {
        return if std::io::Error::last_os_error().raw_os_error() == Some(libc::ENOENT) {
            Ok(None)
        } else {
            Err(Problem("cannot inspect installed selection"))
        };
    }
    data::read_bounded(
        &data::contained_file(root, "installed.json")?,
        data::MAX_STATIC_BYTES,
    )
    .map(Some)
}

fn selection(root: &File) -> Result<Vec<Receipt>, Problem> {
    Ok(document(root)?.installations)
}

fn document(root: &File) -> Result<inventory::Selection, Problem> {
    let Some(bytes) = selection_bytes(root)? else {
        return Ok(inventory::Selection {
            version: 1,
            installations: Vec::new(),
            previous: Vec::new(),
            revision: None,
        });
    };
    let selected: inventory::Selection =
        serde_json::from_value(data::parse_unique(&bytes, data::MAX_STATIC_BYTES)?)
            .map_err(|_| Problem("invalid managed selection document"))?;
    selected.validate()?;
    Ok(selected)
}

fn token_name(value: &OsStr) -> bool {
    value.to_str().is_some_and(|value| {
        value.len() == 64
            && value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    })
}

// only the store's reserved staging namespace and sealed generations can be
// reclaimed. an unsealed directory is not proof of ownership, even with our ID.
fn recover(root: &File, check: &impl Fn() -> Result<(), Problem>) -> Result<(), Problem> {
    let selected = document(root)?;
    let staging = directory(root, "staging")?;
    for component in children(&staging)? {
        check()?;
        if token_name(&component) {
            remove_tree(
                &staging,
                &component,
                0,
                &mut 0,
                Instant::now() + Duration::from_secs(5),
            )?;
        }
    }
    staging
        .sync_all()
        .map_err(|_| Problem("cannot confirm abandoned staging cleanup"))?;
    for component in children(root)? {
        check()?;
        if component
            .to_str()
            .and_then(|value| value.strip_prefix("selection-"))
            .is_some_and(|value| token_name(OsStr::new(value)))
        {
            let file = open_child(root, &component)?;
            if !file
                .metadata()
                .map_err(|_| Problem("cannot inspect selection temporary"))?
                .is_file()
            {
                return Err(Problem("selection temporary is not an owned regular file"));
            }
            let component = name(&component)?;
            if unsafe { libc::unlinkat(root.as_raw_fd(), component.as_ptr(), 0) } != 0 {
                return Err(Problem("selection temporary cleanup is deferred"));
            }
        }
    }
    let generations = directory(root, "generations")?;
    let retained = selected
        .installations
        .iter()
        .chain(&selected.previous)
        .map(|receipt| {
            (
                receipt.repository_id.to_string(),
                receipt.plugin_id.clone(),
                receipt.generation.clone(),
            )
        })
        .collect::<std::collections::BTreeSet<_>>();
    let mut count = 0;
    for repo in children(&generations)? {
        check()?;
        let Some(repo_name) = repo.to_str().filter(|value| {
            value
                .parse::<u64>()
                .is_ok_and(|id| id > 0 && id.to_string() == *value)
        }) else {
            continue;
        };
        let repository = open_child(&generations, &repo)?;
        for plugin in children(&repository)? {
            check()?;
            let Some(plugin_name) = plugin.to_str().filter(|value| {
                crate::plugin::manifest::ManagedIdentity::new(1, (*value).into()).is_ok()
            }) else {
                continue;
            };
            let plugins = open_child(&repository, &plugin)?;
            for generation in children(&plugins)? {
                check()?;
                count += 1;
                if count > 4096 {
                    return Err(Problem("generation recovery count exceeds its budget"));
                }
                if !token_name(&generation) {
                    continue;
                }
                let Some(generation_name) = generation.to_str() else {
                    continue;
                };
                if retained.contains(&(
                    repo_name.into(),
                    plugin_name.into(),
                    generation_name.into(),
                )) {
                    continue;
                }
                let package = open_child(&plugins, &generation)?;
                if !exists(&package, ".among-receipt.json")? {
                    continue;
                }
                let seal: Receipt = serde_json::from_value(data::parse_unique(
                    &data::read_bounded(
                        &data::contained_file(&package, ".among-receipt.json")?,
                        data::MAX_STATIC_BYTES,
                    )?,
                    data::MAX_STATIC_BYTES,
                )?)
                .map_err(|_| Problem("invalid orphan generation receipt; cleanup deferred"))?;
                seal.validate()?;
                if seal.repository_id.to_string() != repo_name
                    || seal.plugin_id != plugin_name
                    || seal.generation != generation_name
                {
                    return Err(Problem(
                        "orphan generation receipt disagrees with its namespace",
                    ));
                }
                let lease = data::contained_file(&package, ".among-lease")?;
                if unsafe { libc::flock(lease.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
                    if std::io::Error::last_os_error().raw_os_error() == Some(libc::EWOULDBLOCK) {
                        continue;
                    }
                    return Err(Problem(
                        "cannot establish exclusive generation cleanup lease",
                    ));
                }
                remove_tree(
                    &plugins,
                    &generation,
                    0,
                    &mut 0,
                    Instant::now() + Duration::from_secs(5),
                )?;
                plugins
                    .sync_all()
                    .map_err(|_| Problem("cannot confirm owned generation cleanup"))?;
            }
        }
    }
    root.sync_all()
        .map_err(|_| Problem("cannot confirm managed-store recovery"))?;
    Ok(())
}

pub(super) fn mutate(
    root_path: &Path,
    expected: &Receipt,
    action: super::mutation::Action,
    stored: Option<&serde_json::Value>,
    check: &impl Fn() -> Result<(), Problem>,
    begin_commit: impl FnOnce() -> Result<(), Problem>,
) -> Result<Commit, Problem> {
    mutate_with_confirmation(
        root_path,
        expected,
        action,
        stored,
        check,
        begin_commit,
        File::sync_all,
    )
}

fn mutate_with_confirmation(
    root_path: &Path,
    expected: &Receipt,
    action: super::mutation::Action,
    stored: Option<&serde_json::Value>,
    check: &impl Fn() -> Result<(), Problem>,
    begin_commit: impl FnOnce() -> Result<(), Problem>,
    confirm: impl FnOnce(&File) -> std::io::Result<()>,
) -> Result<Commit, Problem> {
    use super::mutation::Action;
    expected.validate()?;
    check()?;
    let root = open_existing_root(root_path)?;
    let lock = file(&root, "install.lock", false)?;
    lock_exclusive(&lock, check)?;
    let previous = selection_bytes(&root)?;
    let mut selected = document(&root)?;
    let identity = expected.identity()?;
    let index = selected
        .installations
        .iter()
        .position(|current| current.identity().ok() == Some(identity.clone()))
        .ok_or(Problem(
            "plugin is no longer selected; stale operation refused",
        ))?;
    let current = selected.installations[index].clone();
    let mut restored = None;
    let result = match action {
        Action::RestorePrevious => {
            if &current != expected {
                return Err(Problem(
                    "selected installation changed; stale recovery refused",
                ));
            }
            let index = selected
                .previous
                .iter()
                .position(|old| old.identity().ok() == Some(identity.clone()))
                .ok_or(Problem(
                    "no retained previous verified generation is available",
                ))?;
            let previous = selected.previous[index].clone();
            let _lease = lease(&root, &previous)?;
            let package = data::contained_directory(
                &root,
                &format!(
                    "generations/{}/{}/{}",
                    previous.repository_id, previous.plugin_id, previous.generation
                ),
            )?;
            let bytes = data::read_bounded(
                &data::contained_file(&package, "among-plugin.json")?,
                crate::plugin::manifest::MAX_MANIFEST_BYTES,
            )?;
            if inventory::checksum(&bytes)? != previous.manifest_sha256 {
                return Err(Problem(
                    "previous manifest no longer matches its verified receipt",
                ));
            }
            let inspected = crate::plugin::manifest::inspect(
                &package,
                &previous.package_path(root_path),
                previous.identity()?,
            )?;
            inspected.manifest.compatibility(
                crate::plugin::managed::MAJOR,
                crate::plugin::managed::MINOR,
                crate::plugin::managed::FEATURES,
            )?;
            if let Some(schema) = &inspected.manifest.settings {
                schema.effective(stored)?;
            }
            let current_index = selected
                .installations
                .iter()
                .position(|entry| entry == expected)
                .ok_or(Problem("recovery target changed"))?;
            selected.installations[current_index] = previous.clone();
            selected.previous[index] = current;
            restored = Some(package);
            previous
        }
        Action::Uninstall => {
            // an update that won its commit boundary before revocation may have
            // changed generation, but not installation authority. a reinstall's
            // fresh token is never covered by an old uninstall confirmation.
            if current != *expected
                && (expected.installation.is_none()
                    || current.installation != expected.installation)
            {
                return Err(Problem(
                    "installation authority changed; stale uninstall refused",
                ));
            }
            selected.installations.remove(index);
            selected
                .previous
                .retain(|old| old.identity().ok() != Some(identity.clone()));
            current
        }
    };
    selected.revision = Some(token());
    selected.validate()?;
    let bytes = serde_json::to_vec_pretty(&selected)
        .map_err(|_| Problem("cannot encode package selection"))?;
    if bytes.len() > data::MAX_STATIC_BYTES {
        return Err(Problem("package selection exceeds its byte budget"));
    }
    let temporary_name = format!("selection-{}", token());
    let mut temporary = file(&root, &temporary_name, true)?;
    let outcome = (|| {
        temporary
            .write_all(&bytes)
            .map_err(|_| Problem("cannot write package selection"))?;
        temporary
            .sync_all()
            .map_err(|_| Problem("cannot flush package selection"))?;
        check()?;
        begin_commit()?;
        verify_root(&root, root_path)?;
        if let Some(package) = &restored {
            let pinned = package
                .metadata()
                .map_err(|_| Problem("cannot inspect retained generation"))?;
            let current = data::contained_directory(
                &root,
                &format!(
                    "generations/{}/{}/{}",
                    result.repository_id, result.plugin_id, result.generation
                ),
            )?
            .metadata()
            .map_err(|_| Problem("cannot inspect current retained generation"))?;
            if (pinned.dev(), pinned.ino()) != (current.dev(), current.ino()) {
                return Err(Problem(
                    "retained generation changed externally; refusing recovery",
                ));
            }
        }
        if selection_bytes(&root)? != previous {
            return Err(Problem(
                "installed selection changed externally; refusing replacement",
            ));
        }
        let from = name(OsStr::new(&temporary_name))?;
        if unsafe {
            libc::renameat(
                root.as_raw_fd(),
                from.as_ptr(),
                root.as_raw_fd(),
                c"installed.json".as_ptr(),
            )
        } != 0
        {
            return Err(Problem("cannot commit package selection"));
        }
        Ok(Commit {
            receipt: result,
            durability: if confirm(&root).is_ok() {
                Durability::Durable
            } else {
                Durability::Uncertain
            },
        })
    })();
    if let Ok(temporary) = name(OsStr::new(&temporary_name)) {
        unsafe {
            libc::unlinkat(root.as_raw_fd(), temporary.as_ptr(), 0);
        }
    }
    outcome
}

pub(crate) struct RecoveryJob(std::sync::mpsc::Receiver<Result<(), Problem>>);

impl RecoveryJob {
    pub fn start(root: PathBuf) -> Result<Self, Problem> {
        let permit = super::source::Permit::acquire()?;
        let (send, receive) = std::sync::mpsc::sync_channel(1);
        std::thread::Builder::new().name("among-store-recovery".into()).spawn(move || {
            let _permit=permit;
            let outcome=(|| {
                if matches!(std::fs::symlink_metadata(&root),Err(error) if error.kind()==std::io::ErrorKind::NotFound){return Ok(());}
                let deadline=Instant::now()+Duration::from_secs(10);
                let check=||if Instant::now()<deadline {Ok(())}else{Err(Problem("managed-store recovery exceeded its deadline"))};
                let directory=open_existing_root(&root)?;
                let lock=file(&directory,"install.lock",false)?;
                lock_exclusive(&lock,&check)?;
                verify_root(&directory,&root)?;
                recover(&directory,&check)
            })();
            let _=send.send(outcome);
        }).map_err(|_|Problem("cannot start bounded managed-store recovery"))?;
        Ok(Self(receive))
    }

    pub fn poll(&self) -> Option<Result<(), Problem>> {
        match self.0.try_recv() {
            Ok(outcome) => Some(outcome),
            Err(std::sync::mpsc::TryRecvError::Empty) => None,
            Err(_) => Some(Err(Problem(
                "managed-store recovery worker stopped without a result",
            ))),
        }
    }
}

impl Transaction {
    pub fn new(
        root: &Path,
        anticipated: u64,
        check: impl Fn() -> Result<(), Problem>,
    ) -> Result<Self, Problem> {
        check()?;
        let anticipated = anticipated
            .checked_add(data::MAX_STATIC_BYTES as u64)
            .ok_or(Problem("managed staging reservation overflow"))?;
        let directory = open_root(root)?;
        let lock = file(&directory, "install.lock", false)?;
        lock_exclusive(&lock, &check)?;
        recover(&directory, &check)?;
        let initial_revision = document(&directory)?.revision;
        let generations = self::directory(&directory, "generations")?;
        let staging = self::directory(&directory, "staging")?;
        let mut count = 0;
        let staged = measure(&staging, 0, &mut count, &check)?;
        let installed = measure(&generations, 0, &mut count, &check)?;
        if staged
            .checked_add(anticipated)
            .is_none_or(|bytes| bytes > STAGING_BYTES)
            || installed
                .checked_add(staged)
                .and_then(|bytes| bytes.checked_add(anticipated))
                .is_none_or(|bytes| bytes > STORE_BYTES)
        {
            return Err(Problem(
                "managed package or staging quota would be exceeded",
            ));
        }
        let name = token();
        let component = self::name(OsStr::new(&name))?;
        if unsafe { libc::mkdirat(staging.as_raw_fd(), component.as_ptr(), 0o700) } != 0 {
            return Err(Problem("cannot create fresh managed staging"));
        }
        let stage = data::contained_directory(&staging, &name)?;
        let archive = file(&stage, "archive", true)?;
        let payload = self::directory(&stage, "payload")?;
        Ok(Self {
            root: directory,
            root_path: root.into(),
            initial_revision,
            _lock: lock,
            staging,
            stage,
            name: name.clone(),
            path: root.join("staging").join(name).join("payload"),
            archive,
            payload,
        })
    }

    pub fn payload_path(&self) -> &Path {
        &self.path
    }

    pub fn expect_current(&self, expected: &Receipt) -> Result<(), Problem> {
        if !selection(&self.root)?.contains(expected) {
            return Err(Problem(
                "selected installation changed or was removed; stale update refused",
            ));
        }
        Ok(())
    }

    pub fn refuse_existing(&self, receipt: &Receipt) -> Result<(), Problem> {
        if let Some(previous) = selection(&self.root)?
            .iter()
            .find(|old| old.identity().ok() == receipt.identity().ok())
        {
            if let Some(source) = &receipt.source {
                let change = previous.origin_change(
                    &crate::plugin::origin::Origin {
                        repository_id: receipt.repository_id,
                        publisher_id: source.publisher_id,
                        owner: receipt.owner.clone(),
                        repository: receipt.repository.clone(),
                    },
                    &receipt.plugin_id,
                );
                if change == crate::plugin::origin::Change::PublisherChanged {
                    return Err(Problem("identity already installed; changed or unconfirmed publisher requires explicit update/access review, not first-install replacement"));
                }
                if change == crate::plugin::origin::Change::Renamed {
                    return Err(Problem("repository renamed but this numeric identity is already installed; use an explicit update, not a second installation"));
                }
            }
            return Err(Problem("this identity is already installed; installation never overwrites a selected generation"));
        }
        Ok(())
    }

    pub fn commit(
        &self,
        receipt: Receipt,
        check: impl Fn() -> Result<(), Problem>,
        begin_commit: impl FnOnce() -> Result<(), Problem>,
    ) -> Result<Commit, Problem> {
        self.commit_with_confirmation(receipt, check, begin_commit, File::sync_all)
    }

    fn commit_with_confirmation(
        &self,
        receipt: Receipt,
        check: impl Fn() -> Result<(), Problem>,
        begin_commit: impl FnOnce() -> Result<(), Problem>,
        confirm: impl FnOnce(&File) -> std::io::Result<()>,
    ) -> Result<Commit, Problem> {
        self.commit_selection(receipt, None, check, begin_commit, confirm)
    }

    pub fn commit_update(
        &self,
        receipt: Receipt,
        expected: &Receipt,
        check: impl Fn() -> Result<(), Problem>,
        begin_commit: impl FnOnce() -> Result<(), Problem>,
    ) -> Result<Commit, Problem> {
        self.commit_selection(receipt, Some(expected), check, begin_commit, File::sync_all)
    }

    fn commit_selection(
        &self,
        receipt: Receipt,
        expected: Option<&Receipt>,
        check: impl Fn() -> Result<(), Problem>,
        begin_commit: impl FnOnce() -> Result<(), Problem>,
        confirm: impl FnOnce(&File) -> std::io::Result<()>,
    ) -> Result<Commit, Problem> {
        receipt.validate()?;
        verify_root(&self.root, &self.root_path)?;
        let previous_selection = selection_bytes(&self.root)?;
        let mut selected = document(&self.root)?;
        if expected.is_none() && selected.revision != self.initial_revision {
            return Err(Problem(
                "store authority changed since installation began; stale install refused",
            ));
        }
        if selection_bytes(&self.root)? != previous_selection {
            return Err(Problem(
                "installed selection changed externally; refusing replacement",
            ));
        }
        if let Some(expected) = expected {
            if expected.identity()? != receipt.identity()?
                || !selected.installations.contains(expected)
            {
                return Err(Problem(
                    "selected installation changed or was removed; stale update refused",
                ));
            }
            if receipt.installation != expected.installation {
                return Err(Problem("update cannot replace installation consent token"));
            }
            selected
                .previous
                .retain(|old| old.identity().ok() != receipt.identity().ok());
            selected.previous.push(expected.clone());
            selected
                .installations
                .retain(|old| old.identity().ok() != receipt.identity().ok());
        } else {
            self.refuse_existing(&receipt)?;
        }
        if selected.installations.len() >= inventory::MAX_INSTALLATIONS {
            return Err(Problem("installed identity count exceeds its budget"));
        }
        let seal = serde_json::to_vec(&receipt)
            .map_err(|_| Problem("cannot encode immutable generation receipt"))?;
        let mut seal_file = file(&self.payload, ".among-receipt.json", true)?;
        seal_file
            .write_all(&seal)
            .map_err(|_| Problem("cannot write immutable generation receipt"))?;
        seal_file
            .sync_all()
            .map_err(|_| Problem("cannot flush immutable generation receipt"))?;
        file(&self.payload, ".among-lease", true)?
            .sync_all()
            .map_err(|_| Problem("cannot flush generation lease"))?;
        sync_tree(&self.payload, 0, &mut 0, &check)?;
        let generations = directory(&self.root, "generations")?;
        let repository = directory(&generations, &receipt.repository_id.to_string())?;
        let plugin = directory(&repository, &receipt.plugin_id)?;
        let from = name(OsStr::new("payload"))?;
        let to = name(OsStr::new(&receipt.generation))?;
        if unsafe {
            libc::renameat2(
                self.stage.as_raw_fd(),
                from.as_ptr(),
                plugin.as_raw_fd(),
                to.as_ptr(),
                libc::RENAME_NOREPLACE,
            )
        } != 0
        {
            return Err(Problem(
                "cannot publish the fresh verified generation without replacement",
            ));
        }
        for directory in [&plugin, &repository, &generations, &self.root] {
            directory
                .sync_all()
                .map_err(|_| Problem("cannot confirm complete generation publication"))?;
        }
        self.stage
            .sync_all()
            .map_err(|_| Problem("cannot flush staged generation move"))?;
        selected.installations.push(receipt.clone());
        selected.revision = Some(token());
        selected.validate()?;
        let bytes = serde_json::to_vec_pretty(&selected)
            .map_err(|_| Problem("cannot encode installed selection"))?;
        if bytes.len() > data::MAX_STATIC_BYTES {
            return Err(Problem("installed selection exceeds its byte limit"));
        }
        let temporary_name = format!("selection-{}", self.name);
        let mut temporary = file(&self.root, &temporary_name, true)?;
        let prepared = (|| {
            temporary
                .write_all(&bytes)
                .map_err(|_| Problem("cannot write installed selection"))?;
            temporary
                .sync_all()
                .map_err(|_| Problem("cannot flush installed selection"))?;
            check()?;
            begin_commit()?;
            verify_root(&self.root, &self.root_path)?;
            let published = data::contained_directory(
                &self.root,
                &format!(
                    "generations/{}/{}/{}",
                    receipt.repository_id, receipt.plugin_id, receipt.generation
                ),
            )?
            .metadata()
            .map_err(|_| Problem("cannot inspect published generation"))?;
            let prepared = self
                .payload
                .metadata()
                .map_err(|_| Problem("cannot inspect prepared generation"))?;
            if (published.dev(), published.ino()) != (prepared.dev(), prepared.ino()) {
                return Err(Problem(
                    "published generation changed externally; refusing mixed selection",
                ));
            }
            if selection_bytes(&self.root)? != previous_selection {
                return Err(Problem(
                    "installed selection changed externally; refusing replacement",
                ));
            }
            let from = name(OsStr::new(&temporary_name))?;
            let to = name(OsStr::new("installed.json"))?;
            if unsafe {
                libc::renameat(
                    self.root.as_raw_fd(),
                    from.as_ptr(),
                    self.root.as_raw_fd(),
                    to.as_ptr(),
                )
            } != 0
            {
                return Err(Problem("cannot commit installed selection"));
            }
            Ok(Commit {
                receipt,
                durability: if confirm(&self.root).is_ok() {
                    Durability::Durable
                } else {
                    Durability::Uncertain
                },
            })
        })();
        if let Ok(component) = name(OsStr::new(&temporary_name)) {
            unsafe {
                libc::unlinkat(self.root.as_raw_fd(), component.as_ptr(), 0);
            }
        }
        prepared
    }
}

#[cfg(test)]
mod tests;

impl Drop for Transaction {
    fn drop(&mut self) {
        if let Err(problem) = remove_tree(
            &self.staging,
            OsStr::new(&self.name),
            0,
            &mut 0,
            Instant::now() + Duration::from_secs(5),
        ) {
            eprintln!("among: {problem}");
        }
    }
}
