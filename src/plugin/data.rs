//! static plugin data has a larger byte budget than wire frames, but shares
//! the depth limit. duplicate keys must not acquire parser-dependent authority.

use serde::de::{self, MapAccess, SeqAccess, Visitor};
use serde::Serialize;
use serde::{Deserialize, Deserializer};
use serde_json::{Map, Number, Value};
use std::fmt;
use std::fs::{File, OpenOptions};
use std::io::Read;
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::Path;

pub const MAX_STATIC_BYTES: usize = 256 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Problem(pub &'static str);

impl fmt::Display for Problem {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.0)
    }
}

impl std::error::Error for Problem {}

pub fn parse_unique(bytes: &[u8], limit: usize) -> Result<Value, Problem> {
    if bytes.len() > limit {
        return Err(Problem("static data exceeds its byte limit"));
    }
    if !super::transport::depth_is_bounded(bytes) {
        return Err(Problem("static data exceeds JSON depth 32"));
    }
    serde_json::from_slice::<Unique>(bytes)
        .map(|value| value.0)
        .map_err(|_| Problem("static data is malformed or contains duplicate keys"))
}

pub fn serialized_size(value: &impl Serialize, limit: usize) -> Result<usize, Problem> {
    struct Counter {
        written: usize,
        limit: usize,
    }
    impl std::io::Write for Counter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if bytes.len() > self.limit.saturating_sub(self.written) {
                return Err(std::io::Error::other("data limit"));
            }
            self.written += bytes.len();
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut counter = Counter { written: 0, limit };
    serde_json::to_writer(&mut counter, value)
        .map_err(|_| Problem("encoded data exceeds its byte limit"))?;
    Ok(counter.written)
}

struct Unique(Value);

impl<'de> Deserialize<'de> for Unique {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct DataVisitor;
        impl<'de> Visitor<'de> for DataVisitor {
            type Value = Unique;
            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("bounded JSON with unique keys")
            }
            fn visit_bool<E: de::Error>(self, value: bool) -> Result<Unique, E> {
                Ok(Unique(Value::Bool(value)))
            }
            fn visit_i64<E: de::Error>(self, value: i64) -> Result<Unique, E> {
                Ok(Unique(Value::Number(value.into())))
            }
            fn visit_u64<E: de::Error>(self, value: u64) -> Result<Unique, E> {
                Ok(Unique(Value::Number(value.into())))
            }
            fn visit_f64<E: de::Error>(self, value: f64) -> Result<Unique, E> {
                Number::from_f64(value)
                    .map(|number| Unique(Value::Number(number)))
                    .ok_or_else(|| E::custom("non-finite number"))
            }
            fn visit_str<E: de::Error>(self, value: &str) -> Result<Unique, E> {
                Ok(Unique(Value::String(value.into())))
            }
            fn visit_string<E: de::Error>(self, value: String) -> Result<Unique, E> {
                Ok(Unique(Value::String(value)))
            }
            fn visit_unit<E: de::Error>(self) -> Result<Unique, E> {
                Ok(Unique(Value::Null))
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut sequence: A) -> Result<Unique, A::Error> {
                let mut values = Vec::new();
                while let Some(value) = sequence.next_element::<Unique>()? {
                    values.push(value.0);
                }
                Ok(Unique(Value::Array(values)))
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Unique, A::Error> {
                let mut values = Map::new();
                while let Some(key) = map.next_key::<String>()? {
                    if values.contains_key(&key) {
                        return Err(de::Error::custom("duplicate key"));
                    }
                    values.insert(key, map.next_value::<Unique>()?.0);
                }
                Ok(Unique(Value::Object(values)))
            }
        }
        deserializer.deserialize_any(DataVisitor)
    }
}

pub fn bounded_text(value: &str, maximum: usize, multiline: bool) -> bool {
    value.chars().count() <= maximum
        && value.chars().all(|character| {
            !character.is_control() || (multiline && matches!(character, '\n' | '\t'))
        })
}

pub fn relative_path(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 256
        && bounded_text(value, 256, false)
        && !value.contains(['\\', ':'])
        && value.split('/').count() <= 16
        && value
            .split('/')
            .all(|part| !matches!(part, "" | "." | ".."))
}

pub fn owned_directory(path: &Path) -> Result<File, Problem> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_DIRECTORY)
        .open(path)
        .map_err(|_| Problem("package directory is unavailable or unsafe"))?;
    let metadata = file
        .metadata()
        .map_err(|_| Problem("cannot inspect package directory"))?;
    // the initial managed store is per-user, not a system-wide install location.
    if !private_to_user(&metadata, 0) {
        return Err(Problem(
            "package directory must be owned by the current user and not writable by others \
             or by a group other than the user's own",
        ));
    }
    Ok(file)
}

/// Whether nobody but the current user can change an entry.
///
/// The entry must be owned by the effective user and must not be writable by
/// everyone. Group write is accepted only when the entry's group is the
/// user's own primary group: `umask 002` (the default on Debian, Ubuntu and
/// Fedora, where each user has a private group) creates every directory as
/// `775`, and refusing those would make settings and plugins unusable on the
/// most common desktop setups. A group that is not the user's own may have
/// other members, so write access through it stays refused. `special` lists
/// extra mode bits (setuid, setgid, sticky) the caller refuses.
pub(crate) fn private_to_user(metadata: &std::fs::Metadata, special: u32) -> bool {
    metadata.uid() == unsafe { libc::geteuid() }
        && mode_is_private(
            metadata.mode(),
            metadata.gid(),
            unsafe { libc::getegid() },
            special,
        )
}

fn mode_is_private(mode: u32, group: u32, primary_group: u32, special: u32) -> bool {
    const GROUP_WRITE: u32 = 0o020;
    const OTHERS_WRITE: u32 = 0o002;
    mode & (OTHERS_WRITE | special) == 0 && (mode & GROUP_WRITE == 0 || group == primary_group)
}

pub fn contained_file(root: &File, relative: &str) -> Result<File, Problem> {
    contained(root, relative, false)
}

pub fn contained_directory(root: &File, relative: &str) -> Result<File, Problem> {
    contained(root, relative, true)
}

/// How long a reader keeps reopening the final component while a concurrent
/// `rename` finishes publishing its replacement. While that rename runs, the
/// replaced inode can already have lost its last link while the name still
/// resolves to it, and a lookup does not wait for the rename to finish. On a
/// loaded machine or in a second instance the writer can stay descheduled
/// inside the rename long enough that immediate retries keep seeing the same
/// dead inode, so retries give up the CPU and then pause briefly. The total
/// wait is bounded because a file removed without a replacement is already
/// reported by the failing open, which needs no waiting.
const REPLACED_FILE_WAIT: std::time::Duration = std::time::Duration::from_millis(150);

/// The pause between retries starts here and doubles up to the cap.
const REPLACED_FILE_PAUSE: std::time::Duration = std::time::Duration::from_micros(50);
const REPLACED_FILE_PAUSE_CAP: std::time::Duration = std::time::Duration::from_micros(800);

fn open_checked(
    directory: &File,
    name: &std::ffi::CString,
    expect_directory: bool,
) -> Result<(File, std::fs::Metadata), Problem> {
    let flags = libc::O_RDONLY
        | libc::O_CLOEXEC
        | libc::O_NOFOLLOW
        | libc::O_NONBLOCK
        | if expect_directory {
            libc::O_DIRECTORY
        } else {
            0
        };
    // directory owns the parent descriptor, and every component is opened
    // relative to it with no-follow; downloaded links cannot redirect a read.
    let fd = unsafe { libc::openat(directory.as_raw_fd(), name.as_ptr(), flags) };
    if fd < 0 {
        return Err(Problem("package resource is unavailable or a link"));
    }
    let file = unsafe { File::from_raw_fd(fd) };
    let metadata = file
        .metadata()
        .map_err(|_| Problem("cannot inspect package resource"))?;
    if !private_to_user(&metadata, 0o6000) {
        return Err(Problem(
            "package resource has unsafe ownership or permissions",
        ));
    }
    Ok((file, metadata))
}

/// A writer publishes `installed.json` and settings with `rename`, so a reader
/// that reached the previous inode just before that rename sees no remaining
/// links. That means the file was replaced or removed after the open, not that
/// someone substituted a hard link, so the final component is opened again
/// relative to the unchanged parent directory and checked the same way. A real
/// extra link stays refused, and a file that is now gone reports the open
/// failure of the next attempt.
fn open_final_file(directory: &File, name: &std::ffi::CString) -> Result<File, Problem> {
    let deadline = std::time::Instant::now() + REPLACED_FILE_WAIT;
    let mut pause = REPLACED_FILE_PAUSE;
    let mut attempts = 0u32;
    loop {
        let (file, metadata) = open_checked(directory, name, false)?;
        if metadata.is_file() && metadata.nlink() == 1 {
            return Ok(file);
        }
        if !metadata.is_file() || metadata.nlink() > 1 {
            return Err(Problem(
                "package resource must be a single-link regular file",
            ));
        }
        if std::time::Instant::now() >= deadline {
            return Err(Problem(
                "package resource must be a single-link regular file",
            ));
        }
        if attempts < 2 {
            std::thread::yield_now();
        } else {
            std::thread::sleep(pause);
            pause = (pause * 2).min(REPLACED_FILE_PAUSE_CAP);
        }
        attempts += 1;
    }
}

fn contained(root: &File, relative: &str, directory_only: bool) -> Result<File, Problem> {
    if !relative_path(relative) {
        return Err(Problem("unsafe package resource path"));
    }
    let mut directory = root
        .try_clone()
        .map_err(|_| Problem("cannot retain package directory"))?;
    let parts: Vec<_> = relative.split('/').collect();
    for (index, part) in parts.iter().enumerate() {
        let name =
            std::ffi::CString::new(*part).map_err(|_| Problem("unsafe package resource path"))?;
        let final_part = index + 1 == parts.len();
        if final_part && !directory_only {
            return open_final_file(&directory, &name);
        }
        let (file, metadata) = open_checked(&directory, &name, !final_part || directory_only)?;
        if final_part {
            if directory_only && metadata.is_dir() {
                return Ok(file);
            }
            return Err(Problem(
                "package resource must be a single-link regular file",
            ));
        }
        directory = file;
    }
    Err(Problem("unsafe package resource path"))
}

pub fn read_bounded(file: &File, limit: usize) -> Result<Vec<u8>, Problem> {
    if file
        .metadata()
        .map_err(|_| Problem("cannot inspect static data"))?
        .len()
        > limit as u64
    {
        return Err(Problem("static data exceeds its byte limit"));
    }
    let mut bytes = Vec::new();
    file.take(limit.saturating_add(1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| Problem("cannot read static data"))?;
    if bytes.len() > limit {
        return Err(Problem("static data exceeds its byte limit"));
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn duplicate_keys_are_rejected_at_every_depth_without_values_in_errors() {
        for text in [
            r#"{"id":1,"id":2}"#,
            r#"{"settings":{"x":"secret","x":"other"}}"#,
            r#"{"id":1,"\u0069d":2}"#,
        ] {
            let error = parse_unique(text.as_bytes(), 1024).unwrap_err();
            assert!(!error.to_string().contains("secret"));
        }
        assert_eq!(
            parse_unique(br#"{"a":[true,1,-2,0.5,null,"x"]}"#, 1024).unwrap()["a"][3],
            0.5
        );
        assert!(parse_unique(b"{}", 1).is_err());
        assert!(parse_unique("[".repeat(33).as_bytes(), 1024).is_err());
    }

    #[test]
    fn depth_and_serialized_size_are_measured_before_unbounded_work() {
        let depth32 = format!("{}0{}", "[".repeat(32), "]".repeat(32));
        assert!(parse_unique(depth32.as_bytes(), 1024).is_ok());
        let depth33 = format!("{}0{}", "[".repeat(33), "]".repeat(33));
        assert_eq!(
            parse_unique(depth33.as_bytes(), 1024).unwrap_err(),
            Problem("static data exceeds JSON depth 32")
        );
        let value = serde_json::json!({"text":"\0四\""});
        let size = serde_json::to_vec(&value).unwrap().len();
        assert_eq!(serialized_size(&value, size).unwrap(), size);
        assert!(serialized_size(&value, size - 1).is_err());
        let before = value.clone();
        assert!(serialized_size(&value, 1).is_err());
        assert_eq!(value, before);
    }

    #[test]
    fn mode_rule_accepts_a_private_group_and_refuses_everything_wider() {
        const MINE: u32 = 1000;
        const OTHER: u32 = 100;
        // (mode, group, special bits refused, accepted)
        for (mode, group, special, accepted) in [
            (0o700, OTHER, 0, true),
            (0o755, OTHER, 0, true),
            (0o775, MINE, 0, true),
            (0o770, MINE, 0, true),
            (0o775, OTHER, 0, false),
            (0o770, OTHER, 0, false),
            (0o757, MINE, 0, false),
            (0o777, MINE, 0, false),
            (0o702, MINE, 0, false),
            // setuid, setgid and sticky only count when the caller refuses them.
            (0o2775, MINE, 0, true),
            (0o2775, MINE, 0o6000, false),
            (0o4755, MINE, 0o6000, false),
            (0o1755, MINE, 0o7000, false),
            (0o1755, MINE, 0o6000, true),
        ] {
            assert_eq!(
                mode_is_private(mode, group, MINE, special),
                accepted,
                "mode {mode:o} group {group} refused-special {special:o}"
            );
        }
    }

    /// A group other than the primary one that the current user belongs to, so
    /// the test can hand a directory to it without privileges.
    fn foreign_group() -> Option<u32> {
        let primary = unsafe { libc::getegid() };
        let count = unsafe { libc::getgroups(0, std::ptr::null_mut()) };
        let mut groups = vec![0 as libc::gid_t; usize::try_from(count).ok()?];
        let count = unsafe { libc::getgroups(count, groups.as_mut_ptr()) };
        groups.truncate(usize::try_from(count).ok()?);
        groups.into_iter().find(|group| *group != primary)
    }

    fn directory_with(name: &str, mode: u32) -> std::path::PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let path = std::env::temp_dir().join(format!("among-data-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir(&path).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
        path
    }

    #[test]
    fn owned_directory_accepts_private_and_own_group_writable_and_refuses_the_rest() {
        for (name, mode, accepted) in [
            ("private", 0o700, true),
            ("readable", 0o755, true),
            ("own-group", 0o775, true),
            ("world-writable", 0o757, false),
            ("open", 0o777, false),
        ] {
            let path = directory_with(name, mode);
            assert_eq!(owned_directory(&path).is_ok(), accepted, "{name}");
            std::fs::remove_dir_all(&path).unwrap();
        }
    }

    #[test]
    fn owned_directory_refuses_group_write_through_a_foreign_group() {
        let Some(group) = foreign_group() else {
            eprintln!("NOT VERIFIED: the user has no group besides the primary one");
            return;
        };
        let path = directory_with("foreign-group", 0o775);
        let name = std::ffi::CString::new(path.to_str().unwrap()).unwrap();
        assert_eq!(unsafe { libc::chown(name.as_ptr(), u32::MAX, group) }, 0);
        assert!(owned_directory(&path).is_err());
        // without group write the same foreign group is harmless.
        std::fs::set_permissions(&path, std::os::unix::fs::PermissionsExt::from_mode(0o755))
            .unwrap();
        assert!(owned_directory(&path).is_ok());
        std::fs::remove_dir_all(&path).unwrap();
    }

    #[test]
    fn owned_directory_refuses_a_directory_owned_by_another_user() {
        if unsafe { libc::geteuid() } == 0 {
            eprintln!("NOT VERIFIED: running as root, no foreign-owned directory to observe");
            return;
        }
        // the filesystem root belongs to root.
        assert!(owned_directory(Path::new("/")).is_err());
    }

    #[test]
    fn resource_paths_reject_aliases_traversal_and_control_characters() {
        assert!(relative_path("bin/my-plugin"));
        for path in [
            "", "/bin/x", "../x", "a/../b", "./x", "a//b", "a/", "a\\b", "a:b", "a\0b", "a\nb",
        ] {
            assert!(!relative_path(path), "{path:?}");
        }
    }

    #[test]
    fn a_file_replaced_after_open_is_reopened_instead_of_refused() {
        use std::os::unix::fs::PermissionsExt;
        use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
        use std::sync::{Arc, Barrier};
        use std::time::{Duration, Instant};

        let path = directory_with("replaced-read", 0o700);
        let target = path.join("installed.json");
        std::fs::write(&target, b"initial generation").unwrap();
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o600)).unwrap();
        let root = owned_directory(&path).unwrap();
        let requested = Arc::new(AtomicU64::new(0));
        let finished = Arc::new(AtomicBool::new(false));
        // both threads start together; every reader iteration asks for exactly
        // one replacement, so the writer cannot outrun the reader and starve it
        // of readable files under load.
        let start = Arc::new(Barrier::new(2));
        let writer = {
            let requested = Arc::clone(&requested);
            let finished = Arc::clone(&finished);
            let start = Arc::clone(&start);
            let directory = path.clone();
            std::thread::spawn(move || {
                let deadline = Instant::now() + Duration::from_secs(10);
                start.wait();
                let mut served = 0u64;
                let mut generation = 0u64;
                while !finished.load(Ordering::Acquire) {
                    assert!(
                        Instant::now() < deadline,
                        "replacement writer made no progress before its deadline"
                    );
                    // preparing the next file first leaves the replacement as
                    // one atomic rename that races with the read.
                    generation += 1;
                    let temporary = directory.join(format!("replacement-{generation}"));
                    std::fs::write(&temporary, format!("generation {generation}")).unwrap();
                    std::fs::set_permissions(&temporary, std::fs::Permissions::from_mode(0o600))
                        .unwrap();
                    while requested.load(Ordering::Acquire) <= served {
                        if finished.load(Ordering::Acquire) {
                            return;
                        }
                        assert!(
                            Instant::now() < deadline,
                            "replacement writer made no progress before its deadline"
                        );
                        std::hint::spin_loop();
                    }
                    served += 1;
                    std::fs::rename(&temporary, directory.join("installed.json")).unwrap();
                }
            })
        };
        start.wait();
        for _ in 0..100_000 {
            requested.fetch_add(1, Ordering::AcqRel);
            let file = contained_file(&root, "installed.json").unwrap();
            assert!(!read_bounded(&file, MAX_STATIC_BYTES).unwrap().is_empty());
        }
        finished.store(true, Ordering::Release);
        writer.join().unwrap();
        std::fs::remove_dir_all(&path).unwrap();
    }
}
