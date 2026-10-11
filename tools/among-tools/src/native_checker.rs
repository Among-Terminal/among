use crate::{deb, storage, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Record {
    pub schema: u32,
    pub source_sha: String,
    pub tag: String,
    pub target: String,
    pub architecture: String,
    pub filename: String,
    pub sha256: String,
}

fn identity(source: &str, tag: &str, target: &str, architecture: &str) -> Result<()> {
    deb::stable_version(tag)?;
    if source.len() != 40
        || !source
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
    {
        return Err("invalid checker source identity".into());
    }
    if !matches!(target, "debian13" | "ubuntu24.04") || !matches!(architecture, "amd64" | "arm64") {
        return Err("unsupported native checker target".into());
    }
    Ok(())
}

pub fn package(
    binary: &Path,
    source: &str,
    tag: &str,
    target: &str,
    architecture: &str,
    output: &Path,
) -> Result<Record> {
    identity(source, tag, target, architecture)?;
    if binary.symlink_metadata()?.file_type().is_symlink() || !binary.is_file() {
        return Err("checker input must be a regular native executable".into());
    }
    deb::elf_architecture(binary, architecture)?;
    let bytes = fs::read(binary)?;
    let record = Record {
        schema: 1,
        source_sha: source.into(),
        tag: tag.into(),
        target: target.into(),
        architecture: architecture.into(),
        filename: "among-tools".into(),
        sha256: storage::sha256(bytes.as_slice())?,
    };
    // This internal checker directory is transferred separately; it is not a
    // staging input and is never part of the user-facing DEB payload.
    fs::DirBuilder::new().mode(0o755).create(output)?;
    storage::atomic_write(output, Path::new("among-tools"), &bytes, 0o755)?;
    storage::atomic_write(
        output,
        Path::new("checker-record.json"),
        &serde_json::to_vec_pretty(&record)?,
        0o644,
    )?;
    Ok(record)
}

pub fn validate(
    directory: &Path,
    source: &str,
    tag: &str,
    target: &str,
    architecture: &str,
) -> Result<PathBuf> {
    identity(source, tag, target, architecture)?;
    let record: Record = serde_json::from_value(storage::parse_json(&fs::read(
        storage::safe_path(directory, Path::new("checker-record.json"))?,
    )?)?)?;
    if record.schema != 1
        || record.source_sha != source
        || record.tag != tag
        || record.target != target
        || record.architecture != architecture
        || record.filename != "among-tools"
    {
        return Err("native checker identity differs from release source/target".into());
    }
    let path = storage::safe_path(directory, Path::new(&record.filename))?;
    let metadata = fs::metadata(&path)?;
    if !metadata.is_file()
        || metadata.permissions().mode() & 0o111 == 0
        || storage::sha256(fs::File::open(&path)?)? != record.sha256
    {
        return Err("native checker bytes/mode differ from build receipt".into());
    }
    deb::elf_architecture(&path, architecture)?;
    let mut names = fs::read_dir(directory)?
        .map(|entry| entry.map(|entry| entry.file_name()))
        .collect::<std::io::Result<Vec<_>>>()?;
    names.sort();
    if names
        != [
            std::ffi::OsString::from("among-tools"),
            std::ffi::OsString::from("checker-record.json"),
        ]
    {
        return Err("unexpected internal checker inventory".into());
    }
    Ok(path)
}
