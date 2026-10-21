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
    if metadata.uid() != unsafe { libc::geteuid() } || metadata.mode() & 0o022 != 0 {
        return Err(Problem(
            "package directory must be owned and not writable by others",
        ));
    }
    Ok(file)
}

pub fn contained_file(root: &File, relative: &str) -> Result<File, Problem> {
    contained(root, relative, false)
}

pub fn contained_directory(root: &File, relative: &str) -> Result<File, Problem> {
    contained(root, relative, true)
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
        let flags = libc::O_RDONLY
            | libc::O_CLOEXEC
            | libc::O_NOFOLLOW
            | libc::O_NONBLOCK
            | if final_part && !directory_only {
                0
            } else {
                libc::O_DIRECTORY
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
        if metadata.uid() != unsafe { libc::geteuid() } || metadata.mode() & 0o6022 != 0 {
            return Err(Problem(
                "package resource has unsafe ownership or permissions",
            ));
        }
        if final_part {
            if directory_only && metadata.is_dir() {
                return Ok(file);
            }
            if directory_only || !metadata.is_file() || metadata.nlink() != 1 {
                return Err(Problem(
                    "package resource must be a single-link regular file",
                ));
            }
            return Ok(file);
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
    fn resource_paths_reject_aliases_traversal_and_control_characters() {
        assert!(relative_path("bin/my-plugin"));
        for path in [
            "", "/bin/x", "../x", "a/../b", "./x", "a//b", "a/", "a\\b", "a:b", "a\0b", "a\nb",
        ] {
            assert!(!relative_path(path), "{path:?}");
        }
    }
}
