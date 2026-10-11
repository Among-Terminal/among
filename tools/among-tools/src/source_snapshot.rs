use crate::{storage, Result};
use serde::Serialize;
use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::Path;

pub const FILES: &[&str] = &[
    "Cargo.toml",
    "Cargo.lock",
    "examples/plugins/Cargo.toml",
    "examples/plugins/Cargo.lock",
    "LICENSE",
    "NOTICE",
    "README.md",
    "CONTRIBUTING.md",
    "tools/among-tools/Cargo.toml",
    "tools/among-tools/Cargo.lock",
    "tools/among-tools/README.md",
    "examples/check-clipboard.rs",
    "video/readme-promo/package.json",
];

pub fn check(path: &Path) -> Result<usize> {
    if !path.symlink_metadata()?.file_type().is_file() {
        return Err("snapshot must be a regular non-symlink archive".into());
    }
    let hashes = crate::source_delivery::inventory(fs::File::open(path)?, "source-inventory.json")?;
    if FILES.iter().any(|name| !hashes.contains_key(*name))
        || !hashes.contains_key(".cargo/config.toml")
        || !hashes
            .keys()
            .any(|name| name.starts_with("vendor/") && name.ends_with("/.cargo-checksum.json"))
    {
        return Err("source snapshot is missing mandatory build/vendor inputs".into());
    }
    Ok(hashes.len())
}
pub const DIRECTORIES: &[&str] = &[
    "packaging",
    "tests",
    "src",
    "data",
    "snap",
    "scripts",
    "docs",
    "THIRD_PARTY",
    ".github",
    "examples/plugins/src",
    "examples/plugins/tests",
    "examples/plugins/examples",
    "tools/among-tools/src",
    "tools/among-tools/tests",
    "tools/among-tools/containers",
];

#[derive(Clone)]
pub struct SourceFile {
    pub bytes: Vec<u8>,
    pub mode: u32,
}

fn file(root: &Path, relative: &Path) -> Result<SourceFile> {
    let path = storage::safe_path(root, relative)?;
    let metadata = fs::metadata(&path)?;
    if !metadata.is_file() {
        return Err("source inputs must be regular files".into());
    }
    Ok(SourceFile {
        bytes: fs::read(path)?,
        mode: if metadata.permissions().mode() & 0o111 != 0 {
            0o755
        } else {
            0o644
        },
    })
}

fn walk(
    root: &Path,
    relative: &Path,
    prefix: &str,
    files: &mut BTreeMap<String, SourceFile>,
) -> Result<()> {
    let directory = if relative.as_os_str().is_empty() {
        root.to_path_buf()
    } else {
        storage::safe_path(root, relative)?
    };
    if directory.symlink_metadata()?.file_type().is_symlink() || !directory.is_dir() {
        return Err("source directory must not be a symlink or non-directory".into());
    }
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        if entry.file_name() == "__pycache__" {
            continue;
        }
        let relative = relative.join(entry.file_name());
        let path = storage::safe_path(root, &relative)?;
        if path.is_dir() {
            walk(root, &relative, prefix, files)?;
        } else {
            let name = format!(
                "{prefix}{}",
                relative.to_str().ok_or("source path is not UTF-8")?
            );
            if files.insert(name, file(root, &relative)?).is_some() {
                return Err("duplicate selected source member".into());
            }
        }
    }
    Ok(())
}

pub fn inputs(root: &Path, extra_directories: &[&str]) -> Result<BTreeMap<String, SourceFile>> {
    let mut files = BTreeMap::new();
    for name in FILES {
        files.insert((*name).into(), file(root, Path::new(name))?);
    }
    for name in DIRECTORIES.iter().chain(extra_directories) {
        let path = storage::safe_path(root, Path::new(name))?;
        // Optional directories mirror the original snapshot; existing symlink
        // directory roots fail even when their target does not exist.
        if path.exists() {
            walk(root, Path::new(name), "", &mut files)?;
        }
    }
    Ok(files)
}

fn tar(files: &BTreeMap<String, SourceFile>) -> Result<Vec<u8>> {
    let mut builder = tar::Builder::new(Vec::new());
    for (name, source) in files {
        storage::relative_path(Path::new(name))?;
        let mut header = tar::Header::new_gnu();
        header.set_size(source.bytes.len() as u64);
        header.set_mode(source.mode);
        header.set_uid(0);
        header.set_gid(0);
        header.set_mtime(0);
        header.set_entry_type(tar::EntryType::Regular);
        header.set_cksum();
        builder.append_data(&mut header, name, source.bytes.as_slice())?;
    }
    Ok(builder.into_inner()?)
}

#[derive(Debug, Serialize)]
pub struct Receipt {
    pub files: usize,
    pub archive_sha256: String,
}

pub fn snapshot(root: &Path, vendor: &Path, output: &Path) -> Result<Receipt> {
    let mut members = inputs(root, &[])?;
    walk(vendor, Path::new(""), "vendor/", &mut members)?;
    if !members
        .keys()
        .any(|name| name.starts_with("vendor/") && name.ends_with("/.cargo-checksum.json"))
    {
        return Err("cargo vendor source tree is required".into());
    }
    let count = members.len();
    let config = b"[source.crates-io]\nreplace-with = \"vendored-sources\"\n\n[source.vendored-sources]\ndirectory = \"vendor\"\n";
    members.insert(
        ".cargo/config.toml".into(),
        SourceFile {
            bytes: config.to_vec(),
            mode: 0o644,
        },
    );
    let hashes = members
        .iter()
        .map(|(name, value)| Ok((name.clone(), storage::sha256(value.bytes.as_slice())?)))
        .collect::<Result<BTreeMap<_, _>>>()?;
    let mut inventory = serde_json::to_vec_pretty(&serde_json::json!({"schema":1,"files":hashes}))?;
    inventory.push(b'\n');
    members.insert(
        "source-inventory.json".into(),
        SourceFile {
            bytes: inventory,
            mode: 0o644,
        },
    );
    let bytes = tar(&members)?;
    let receipt = Receipt {
        files: count,
        archive_sha256: storage::sha256(bytes.as_slice())?,
    };
    // Complete validation precedes exclusive output creation; stale/retry output
    // is never overwritten. Immutable consumers must use this new digest.
    let mut stream = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o644)
        .open(output)?;
    stream.write_all(&bytes)?;
    stream.sync_all()?;
    Ok(receipt)
}
