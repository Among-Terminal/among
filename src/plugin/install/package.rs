//! archive headers never become filesystem paths directly: only declared regular
//! files are created through pinned owned staging descriptors.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::time::{Duration, Instant};

use super::metadata::{Artifact, Version, MAX_EXPANDED_BYTES, MAX_MEMBERS};
use super::store;
use crate::plugin::data::{self, Problem};
use crate::plugin::manifest::{self, ManagedIdentity};

const BLOCK: usize = 128 * 1024;
const EXTRACTION_TIME: Duration = Duration::from_secs(10);

struct Bounded<'a, R, F> {
    input: R,
    check: &'a F,
    deadline: Instant,
    remaining: u64,
}

impl<R: Read, F: Fn() -> Result<(), Problem>> Read for Bounded<'_, R, F> {
    fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
        (self.check)().map_err(std::io::Error::other)?;
        if Instant::now() >= self.deadline || self.remaining == 0 {
            return Err(std::io::Error::other(
                "archive extraction exceeded its time or expansion budget",
            ));
        }
        let length = bytes.len().min(BLOCK).min(self.remaining as usize);
        let read = self.input.read(&mut bytes[..length])?;
        self.remaining -= read as u64;
        Ok(read)
    }
}

pub(super) fn extract(
    archive: &mut File,
    directory: &File,
    path: &Path,
    identity: ManagedIdentity,
    version: &Version,
    artifact: &Artifact,
    check: impl Fn() -> Result<(), Problem>,
) -> Result<(), Problem> {
    archive
        .seek(SeekFrom::Start(0))
        .map_err(|_| Problem("cannot rewind verified package"))?;
    let compressed = std::io::BufReader::new(archive);
    let decoder = flate2::bufread::GzDecoder::new(compressed);
    let bounded = Bounded {
        input: decoder,
        check: &check,
        deadline: Instant::now() + EXTRACTION_TIME,
        remaining: MAX_EXPANDED_BYTES + (MAX_MEMBERS as u64 * 17 + 2) * 512,
    };
    let mut archive = tar::Archive::new(bounded);
    let expected: BTreeMap<_, _> = artifact
        .members
        .iter()
        .map(|member| (member.path.as_str(), member))
        .collect();
    let mut directories = BTreeSet::new();
    for member in &artifact.members {
        for (index, byte) in member.path.bytes().enumerate() {
            if byte == b'/' {
                directories.insert(&member.path[..index]);
            }
        }
    }
    let mut seen = BTreeSet::new();
    let mut files = BTreeSet::new();
    let mut expanded = 0u64;
    for entry in archive
        .entries()
        .map_err(|_| Problem("invalid tar archive"))?
        .raw(true)
    {
        check()?;
        let mut entry = entry.map_err(|_| Problem("invalid or truncated tar header"))?;
        if seen.len() >= MAX_MEMBERS * 17 {
            return Err(Problem("archive header count exceeds its budget"));
        }
        let kind = entry.header().entry_type();
        let bytes = entry.path_bytes().into_owned();
        let path =
            std::str::from_utf8(&bytes).map_err(|_| Problem("archive paths must be UTF-8"))?;
        let path = if kind.is_dir() {
            path.strip_suffix('/').unwrap_or(path)
        } else {
            path
        };
        if !data::relative_path(path) || !seen.insert(path.to_string()) {
            return Err(Problem("archive contains an unsafe or duplicate path"));
        }
        let mode = entry
            .header()
            .mode()
            .map_err(|_| Problem("invalid archive permissions"))?;
        if mode & !0o755 != 0 {
            return Err(Problem(
                "archive contains privileged or writable-by-others permissions",
            ));
        }
        if kind.is_dir() {
            if entry.size() != 0 || !directories.contains(path) {
                return Err(Problem("archive contains an unexpected directory"));
            }
            let mut parent = directory
                .try_clone()
                .map_err(|_| Problem("cannot retain staging directory"))?;
            for component in path.split('/') {
                parent = store::directory(&parent, component)?;
            }
            continue;
        }
        if !kind.is_file() {
            return Err(Problem(
                "archive links, special files and extended headers are unsupported",
            ));
        }
        let member = expected
            .get(path)
            .ok_or(Problem("archive contains an undeclared file"))?;
        if entry.size() != member.size || (mode & 0o111 != 0) != member.executable {
            return Err(Problem(
                "archive size or executable mode disagrees with selected metadata",
            ));
        }
        expanded = expanded
            .checked_add(member.size)
            .filter(|bytes| *bytes <= MAX_EXPANDED_BYTES)
            .ok_or(Problem("expanded package bytes exceed their bounds"))?;
        let mut components = path.split('/').collect::<Vec<_>>();
        let leaf = components
            .pop()
            .ok_or(Problem("archive file has no name"))?;
        let mut parent = directory
            .try_clone()
            .map_err(|_| Problem("cannot retain staging directory"))?;
        for component in components {
            parent = store::directory(&parent, component)?;
        }
        let mut output = store::file(&parent, leaf, true)?;
        let mut checksum = glib::Checksum::new(glib::ChecksumType::Sha256)
            .ok_or(Problem("SHA-256 is unavailable"))?;
        let mut copied = 0u64;
        let mut buffer = vec![0u8; BLOCK];
        loop {
            check()?;
            let read = entry.read(&mut buffer).map_err(|_| {
                Problem("package extraction was interrupted or exceeded its budget")
            })?;
            if read == 0 {
                break;
            }
            copied += read as u64;
            if copied > member.size {
                return Err(Problem("archive file exceeds its declared size"));
            }
            checksum.update(&buffer[..read]);
            output
                .write_all(&buffer[..read])
                .map_err(|_| Problem("cannot write staged package file"))?;
        }
        if copied != member.size || checksum.string().as_deref() != Some(member.sha256.as_str()) {
            return Err(Problem(
                "packaged file size or digest disagrees with selected metadata",
            ));
        }
        output
            .set_permissions(std::fs::Permissions::from_mode(if member.executable {
                0o700
            } else {
                0o600
            }))
            .map_err(|_| Problem("cannot set safe staged permissions"))?;
        output
            .sync_all()
            .map_err(|_| Problem("cannot flush staged package file"))?;
        files.insert(path.to_string());
    }
    if files.len() != expected.len() {
        return Err(Problem("archive omits a declared package file"));
    }
    // tar stops at its terminator; still consume bounded padding and gzip CRC,
    // refusing hidden appended members or a second compressed stream.
    let mut remaining = archive.into_inner();
    let mut buffer = [0u8; 4096];
    loop {
        let read = remaining
            .read(&mut buffer)
            .map_err(|_| Problem("invalid gzip trailer or archive expansion budget"))?;
        if read == 0 {
            break;
        }
        if buffer[..read].iter().any(|byte| *byte != 0) {
            return Err(Problem("non-padding data follows the tar terminator"));
        }
    }
    let mut compressed = remaining.input.into_inner();
    if compressed
        .read(&mut buffer[..1])
        .map_err(|_| Problem("cannot confirm package end"))?
        != 0
    {
        return Err(Problem(
            "data or another gzip member follows the selected archive",
        ));
    }
    check()?;
    super::entrypoint::validate(
        &data::contained_file(directory, &version.manifest.entrypoint)?,
        artifact,
        &check,
    )?;
    let manifest_bytes = data::read_bounded(
        &data::contained_file(directory, "among-plugin.json")?,
        manifest::MAX_MANIFEST_BYTES,
    )?;
    if crate::plugin::inventory::checksum(&manifest_bytes)? != version.manifest_sha256 {
        return Err(Problem("packaged shared manifest digest changed"));
    }
    let inspected = manifest::inspect(directory, path, identity)?;
    if inspected.manifest != version.manifest {
        return Err(Problem(
            "packaged shared manifest differs from the selected preview",
        ));
    }
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests;
