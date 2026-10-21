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

pub(super) struct Transaction {
    root: File,
    _lock: File,
    staging: File,
    stage: File,
    name: String,
    path: PathBuf,
    pub archive: File,
    pub payload: File,
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
        if unsafe { libc::mkdirat(parent.as_raw_fd(), component.as_ptr(), 0o700) } == 0 {
            parent
                .sync_all()
                .map_err(|_| Problem("cannot flush new managed-store root"))?;
        } else if std::io::Error::last_os_error().raw_os_error() != Some(libc::EEXIST) {
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
    let Some(bytes) = selection_bytes(root)? else {
        return Ok(Vec::new());
    };
    let selected: inventory::Selection =
        serde_json::from_value(data::parse_unique(&bytes, data::MAX_STATIC_BYTES)?)
            .map_err(|_| Problem("invalid managed selection document"))?;
    if selected.version != 1 || selected.installations.len() > inventory::MAX_INSTALLATIONS {
        return Err(Problem(
            "unsupported selection version or installation count",
        ));
    }
    let mut ids = std::collections::BTreeSet::new();
    for receipt in &selected.installations {
        receipt.validate()?;
        if !ids.insert(receipt.identity()?.key()) {
            return Err(Problem("duplicate committed plugin identity"));
        }
    }
    Ok(selected.installations)
}

impl Transaction {
    pub fn new(
        root: &Path,
        anticipated: u64,
        check: impl Fn() -> Result<(), Problem>,
    ) -> Result<Self, Problem> {
        check()?;
        let directory = open_root(root)?;
        let lock = file(&directory, "install.lock", false)?;
        if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            return Err(Problem(
                "another managed-store operation is active; wait before retrying",
            ));
        }
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
        receipt.validate()?;
        self.refuse_existing(&receipt)?;
        let previous_selection = selection_bytes(&self.root)?;
        let mut selected = selection(&self.root)?;
        if selection_bytes(&self.root)? != previous_selection {
            return Err(Problem(
                "installed selection changed externally; refusing replacement",
            ));
        }
        if selected.len() >= inventory::MAX_INSTALLATIONS {
            return Err(Problem("installed identity count exceeds its budget"));
        }
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
        selected.push(receipt.clone());
        let bytes = serde_json::to_vec_pretty(&inventory::Selection {
            version: 1,
            installations: selected,
        })
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
