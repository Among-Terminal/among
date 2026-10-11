use crate::Result;
use serde::de::{self, Deserialize, Deserializer, MapAccess, SeqAccess, Visitor};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::{
    ffi::OsStrExt,
    fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt},
};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

static SERIAL: AtomicU64 = AtomicU64::new(0);

pub(crate) fn publish_directory(source: &Path, destination: &Path) -> Result<()> {
    let source_c = std::ffi::CString::new(source.as_os_str().as_bytes())?;
    let destination_c = std::ffi::CString::new(destination.as_os_str().as_bytes())?;
    // The destination must stay new even if another caller wins after preflight.
    let result = unsafe {
        nix::libc::renameat2(
            nix::libc::AT_FDCWD,
            source_c.as_ptr(),
            nix::libc::AT_FDCWD,
            destination_c.as_ptr(),
            nix::libc::RENAME_NOREPLACE,
        )
    };
    if result != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    File::open(
        destination
            .parent()
            .ok_or("publication destination parent absent")?,
    )?
    .sync_all()?;
    Ok(())
}

pub fn private_directory(root: &Path, prefix: &str) -> Result<PathBuf> {
    relative_path(Path::new(prefix))?;
    if Path::new(prefix).components().count() != 1
        || root.symlink_metadata()?.file_type().is_symlink()
        || !root.is_dir()
    {
        return Err("private directory requires a real root and one safe prefix".into());
    }
    // Wall clocks can return identical nanoseconds to parallel native tests.
    // Reserve each root exclusively before creating any children; never merge
    // with another fixture or discard a pre-existing directory on collision.
    for _ in 0..64 {
        let path = root.join(format!(
            "{prefix}-{}-{}",
            std::process::id(),
            SERIAL.fetch_add(1, Ordering::Relaxed)
        ));
        match fs::DirBuilder::new().mode(0o700).create(&path) {
            Ok(()) => return Ok(path),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error.into()),
        }
    }
    Err("private directory collision limit reached".into())
}

pub fn relative_path(path: &Path) -> Result<()> {
    if path.as_os_str().is_empty()
        || !path
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
    {
        return Err("path must contain only relative normal components".into());
    }
    // components() normalizes interior '.', so reject it before normalization.
    if path
        .to_str()
        .ok_or("path is not UTF-8")?
        .split('/')
        .any(|part| part == "." || part.is_empty())
    {
        return Err("path contains an empty or dot component".into());
    }
    Ok(())
}

pub fn safe_path(root: &Path, relative: &Path) -> Result<PathBuf> {
    relative_path(relative)?;
    if fs::symlink_metadata(root)?.file_type().is_symlink() || !root.is_dir() {
        return Err("owned root must be a real directory".into());
    }
    let mut current = root.canonicalize()?;
    for component in relative.components() {
        current.push(component);
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err("owned path contains a symlink".into())
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(current)
}

pub fn sha256(mut reader: impl Read) -> Result<String> {
    let mut digest = Sha256::new();
    let mut buffer = [0; 64 * 1024];
    loop {
        let count = reader.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        digest.update(&buffer[..count]);
    }
    Ok(format!("{:x}", digest.finalize()))
}

pub fn atomic_write(root: &Path, relative: &Path, bytes: &[u8], mode: u32) -> Result<()> {
    atomic_write_before_commit(root, relative, bytes, mode, || Ok(()))
}

pub fn atomic_write_before_commit(
    root: &Path,
    relative: &Path,
    bytes: &[u8],
    mode: u32,
    before_commit: impl FnOnce() -> Result<()>,
) -> Result<()> {
    let destination = safe_path(root, relative)?;
    let parent = destination.parent().ok_or("destination has no parent")?;
    if !parent.is_dir() {
        return Err("atomic write requires an existing owned parent".into());
    }
    if destination.exists() && !destination.is_file() {
        return Err("destination is not a regular file".into());
    }
    let temporary = parent.join(format!(
        ".among-write-{}-{}",
        std::process::id(),
        SERIAL.fetch_add(1, Ordering::Relaxed)
    ));
    let mut created = false;
    let result = (|| -> Result<()> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(mode)
            .open(&temporary)?;
        created = true;
        file.write_all(bytes)?;
        file.set_permissions(fs::Permissions::from_mode(mode))?;
        file.sync_all()?;
        before_commit()?;
        // recheck after writing so an injected late symlink never gets replaced.
        safe_path(root, relative)?;
        fs::rename(&temporary, &destination)?;
        File::open(parent)?.sync_all()?;
        Ok(())
    })();
    if created && temporary.exists() {
        fs::remove_file(&temporary)?;
    }
    result
}

pub struct ExclusiveLock {
    file: File,
}

impl ExclusiveLock {
    /// Take the lock, refusing immediately if another holder has it.
    ///
    /// This is the right behaviour for the interactive and installation paths:
    /// a second concurrent operation on the same state is refused so the caller
    /// can report it, rather than silently waiting behind work the user cannot
    /// see. Blocking there would make a genuinely stuck process look like a
    /// slow one.
    pub fn acquire(root: &Path, relative: &Path) -> Result<Self> {
        Self::open(root, relative, None)
    }

    /// Take the lock, waiting up to `wait` for a concurrent holder.
    ///
    /// Used only where the second operation is a harmless retry of the same
    /// intent rather than a conflicting user action: a re-run of a publication
    /// must wait for the first to finish, then observe the work is done, rather
    /// than fail and leave the release unpromoted.
    pub fn acquire_waiting(root: &Path, relative: &Path, wait: Duration) -> Result<Self> {
        Self::open(root, relative, Some(wait))
    }

    fn open(root: &Path, relative: &Path, wait: Option<Duration>) -> Result<Self> {
        let path = safe_path(root, relative)?;
        if path.exists() && !path.is_file() {
            return Err("lock must be a regular file".into());
        }
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(path)?;
        let Some(wait) = wait else {
            file.try_lock()?;
            return Ok(Self { file });
        };
        let deadline = std::time::Instant::now() + wait;
        loop {
            match file.try_lock() {
                Ok(()) => return Ok(Self { file }),
                Err(std::fs::TryLockError::WouldBlock) => {
                    // Exceeding the bound is a refusal, never a silent proceed:
                    // publishing without the lock could double-publish.
                    if std::time::Instant::now() >= deadline {
                        return Err(
                            "another publication holds the lock; refusing to publish concurrently"
                                .into(),
                        );
                    }
                    std::thread::sleep(Duration::from_millis(50));
                }
                Err(error) => return Err(error.into()),
            }
        }
    }
}

impl Drop for ExclusiveLock {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

pub fn deterministic_tar(files: &BTreeMap<String, Vec<u8>>) -> Result<Vec<u8>> {
    let mut builder = tar::Builder::new(Vec::new());
    for (name, bytes) in files {
        relative_path(Path::new(name))?;
        let mut header = tar::Header::new_gnu();
        header.set_size(bytes.len() as u64);
        header.set_mode(0o644);
        header.set_uid(0);
        header.set_gid(0);
        header.set_mtime(0);
        header.set_entry_type(tar::EntryType::Regular);
        header.set_cksum();
        builder.append_data(&mut header, name, bytes.as_slice())?;
    }
    Ok(builder.into_inner()?)
}

pub fn inspect_tar(bytes: &[u8]) -> Result<BTreeMap<String, String>> {
    let mut archive = tar::Archive::new(bytes);
    let mut inventory = BTreeMap::new();
    for entry in archive.entries()? {
        let mut entry = entry?;
        if !entry.header().entry_type().is_file() {
            return Err("archive members must be regular files".into());
        }
        let path = entry.path()?.into_owned();
        relative_path(&path)?;
        let name = path
            .to_str()
            .ok_or("archive name is not UTF-8")?
            .to_string();
        if inventory.insert(name, sha256(&mut entry)?).is_some() {
            return Err("duplicate archive member".into());
        }
    }
    Ok(inventory)
}

struct StrictValue(Value);

impl<'de> Deserialize<'de> for StrictValue {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        struct StrictVisitor;
        impl<'de> Visitor<'de> for StrictVisitor {
            type Value = StrictValue;
            fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
                formatter.write_str("JSON without duplicate object keys")
            }
            fn visit_bool<E: de::Error>(self, v: bool) -> std::result::Result<Self::Value, E> {
                Ok(StrictValue(Value::Bool(v)))
            }
            fn visit_i64<E: de::Error>(self, v: i64) -> std::result::Result<Self::Value, E> {
                Ok(StrictValue(v.into()))
            }
            fn visit_u64<E: de::Error>(self, v: u64) -> std::result::Result<Self::Value, E> {
                Ok(StrictValue(v.into()))
            }
            fn visit_f64<E: de::Error>(self, v: f64) -> std::result::Result<Self::Value, E> {
                serde_json::Number::from_f64(v)
                    .map(|n| StrictValue(Value::Number(n)))
                    .ok_or_else(|| E::custom("invalid JSON number"))
            }
            fn visit_str<E: de::Error>(self, v: &str) -> std::result::Result<Self::Value, E> {
                Ok(StrictValue(Value::String(v.into())))
            }
            fn visit_unit<E: de::Error>(self) -> std::result::Result<Self::Value, E> {
                Ok(StrictValue(Value::Null))
            }
            fn visit_seq<A: SeqAccess<'de>>(
                self,
                mut seq: A,
            ) -> std::result::Result<Self::Value, A::Error> {
                let mut values = Vec::new();
                while let Some(value) = seq.next_element::<StrictValue>()? {
                    values.push(value.0);
                }
                Ok(StrictValue(Value::Array(values)))
            }
            fn visit_map<A: MapAccess<'de>>(
                self,
                mut map: A,
            ) -> std::result::Result<Self::Value, A::Error> {
                let mut values = serde_json::Map::new();
                while let Some((key, value)) = map.next_entry::<String, StrictValue>()? {
                    if values.insert(key, value.0).is_some() {
                        return Err(de::Error::custom("duplicate JSON key"));
                    }
                }
                Ok(StrictValue(Value::Object(values)))
            }
        }
        deserializer.deserialize_any(StrictVisitor)
    }
}

pub fn parse_json(bytes: &[u8]) -> Result<Value> {
    Ok(serde_json::from_slice::<StrictValue>(bytes)?.0)
}
