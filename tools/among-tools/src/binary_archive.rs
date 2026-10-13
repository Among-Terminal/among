use crate::{deb, process::OwnedProcess, storage, Result};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File},
    io::Read,
    os::unix::fs::PermissionsExt,
    path::Path,
    time::Duration,
};

fn add(builder: &mut tar::Builder<File>, name: &str, bytes: &[u8], mode: u32) -> Result<()> {
    storage::relative_path(Path::new(name))?;
    let mut header = tar::Header::new_gnu();
    header.set_size(bytes.len() as u64);
    header.set_mode(mode);
    header.set_uid(0);
    header.set_gid(0);
    header.set_mtime(0);
    header.set_entry_type(tar::EntryType::Regular);
    header.set_cksum();
    builder.append_data(&mut header, name, bytes)?;
    Ok(())
}

fn attribution(builder: &mut tar::Builder<File>, root: &Path, relative: &Path) -> Result<()> {
    let path = storage::safe_path(root, relative)?;
    let kind = path.symlink_metadata()?.file_type();
    if kind.is_dir() {
        let mut entries = fs::read_dir(&path)?.collect::<std::io::Result<Vec<_>>>()?;
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            attribution(builder, root, &relative.join(entry.file_name()))?;
        }
    } else if kind.is_file() {
        let bytes = fs::read(&path)?;
        if bytes.is_empty() {
            return Err("empty binary archive attribution".into());
        }
        add(
            builder,
            relative
                .to_str()
                .ok_or("archive notice path is not UTF-8")?,
            &bytes,
            0o644,
        )?;
    } else {
        return Err("binary archive notices must be regular files, not links".into());
    }
    Ok(())
}

fn instructions(tag: &str, target: &str, architecture: &str, dependencies: &str) -> String {
    format!("Among Terminal {tag}\nTarget: {target}, {architecture}; not a universal or self-contained Linux binary.\nRequired distribution packages: {dependencies}\n\nExtract this archive and run ./bin/among. No compiler or Rust toolchain is needed.\nLibraries are NOT bundled: install the distribution runtime dependencies above.\nPrefer the DEB if you want APT to install dependencies and a desktop launcher.\nOptional plugin executables are in plugins/ and remain disabled by default.\nMatching source and licence information accompanies the GitHub release.\n")
}

/// The archive name a tag, distribution and architecture produce. Release
/// assembly derives the expected name from here instead of trusting a record.
pub fn filename(tag: &str, target: &str, architecture: &str) -> Result<String> {
    Ok(format!(
        "among-{}-{target}-{architecture}.tar.gz",
        tag.strip_prefix('v').ok_or("archive tag lacks prefix")?
    ))
}

/// Archives retain the exact inspected package executables, not a second build.
/// They deliberately do not pretend to bundle distribution runtime libraries.
pub fn create(
    payload: &Path,
    tag: &str,
    target: &str,
    architecture: &str,
    dependencies: &str,
    output: &Path,
) -> Result<(String, String)> {
    let output = output.canonicalize()?;
    deb::release_version(tag)?;
    if !matches!(target, "debian13" | "ubuntu24.04") || dependencies.is_empty() {
        return Err(
            "binary archive requires an explicit supported distribution and runtime dependencies"
                .into(),
        );
    }
    for name in deb::EXECUTABLES {
        let relative = if name == "among" {
            "usr/bin/among".into()
        } else {
            format!("usr/share/among/plugins/{name}")
        };
        deb::elf_architecture(
            &storage::safe_path(payload, Path::new(&relative))?,
            architecture,
        )?;
    }
    let filename = filename(tag, target, architecture)?;
    if output.join(&filename).symlink_metadata().is_ok() {
        return Err("binary archive output must be new".into());
    }
    let scratch = storage::private_directory(&std::env::temp_dir(), "among-binary-archive")?;
    let result = (|| -> Result<(String, String)> {
        let tar_path = scratch.join(filename.strip_suffix(".gz").ok_or("gzip suffix absent")?);
        let file = File::options()
            .write(true)
            .create_new(true)
            .open(&tar_path)?;
        let mut builder = tar::Builder::new(file);
        for name in deb::EXECUTABLES {
            let relative = if name == "among" {
                "usr/bin/among".into()
            } else {
                format!("usr/share/among/plugins/{name}")
            };
            let name = if name == "among" {
                "bin/among".into()
            } else {
                format!("plugins/{name}")
            };
            add(
                &mut builder,
                &name,
                &fs::read(storage::safe_path(payload, Path::new(&relative))?)?,
                0o755,
            )?;
        }
        let notices = storage::safe_path(payload, Path::new("usr/share/doc/among-terminal"))?;
        for name in ["LICENSE", "NOTICE", "THIRD_PARTY"] {
            attribution(&mut builder, &notices, Path::new(name))?;
        }
        let instructions = instructions(tag, target, architecture, dependencies);
        add(&mut builder, "INSTALL.txt", instructions.as_bytes(), 0o644)?;
        builder.finish()?;
        drop(builder);
        let environment = BTreeMap::from([
            ("PATH".into(), "/usr/bin:/bin".into()),
            ("LC_ALL".into(), "C".into()),
        ]);
        let mut gzip = OwnedProcess::spawn(
            Path::new("/usr/bin/gzip"),
            &["-n", tar_path.to_str().ok_or("archive path is not UTF-8")?],
            &environment,
            &scratch,
            &File::create(scratch.join("binary-archive.log"))?,
        )?;
        if !gzip.wait(Duration::from_secs(60))?.success() {
            return Err("binary archive compression failed".into());
        }
        let bytes = fs::read(scratch.join(&filename))?;
        let digest = storage::sha256(bytes.as_slice())?;
        storage::atomic_write(&output, Path::new(&filename), &bytes, 0o644)?;
        Ok((filename, digest))
    })();
    fs::remove_dir_all(&scratch)?;
    result
}

pub fn validate(
    record: &serde_json::Value,
    directory: &Path,
    tag: &str,
    target: &str,
    architecture: &str,
) -> Result<()> {
    deb::release_version(tag)?;
    if !matches!(target, "debian13" | "ubuntu24.04") || !matches!(architecture, "amd64" | "arm64") {
        return Err("unsupported binary archive target".into());
    }
    let expected = format!(
        "among-{}-{target}-{architecture}.tar.gz",
        tag.strip_prefix('v').ok_or("archive tag lacks prefix")?
    );
    let archive = &record["binary_archive"];
    if archive["filename"] != expected {
        return Err("binary archive identity differs from release job".into());
    }
    let path = storage::safe_path(directory, Path::new(&expected))?;
    if !path.symlink_metadata()?.is_file()
        || archive["sha256"] != storage::sha256(File::open(path)?)?
    {
        return Err("binary archive is missing, linked or changed".into());
    }
    Ok(())
}

fn expected_members(payload: &Path) -> Result<BTreeMap<String, (String, u32)>> {
    fn notices(
        root: &Path,
        relative: &Path,
        members: &mut BTreeMap<String, (String, u32)>,
    ) -> Result<()> {
        let path = storage::safe_path(root, relative)?;
        let kind = path.symlink_metadata()?.file_type();
        if kind.is_dir() {
            for entry in fs::read_dir(path)? {
                notices(root, &relative.join(entry?.file_name()), members)?;
            }
        } else if kind.is_file() {
            members.insert(
                relative.to_str().ok_or("notice path is not UTF-8")?.into(),
                (storage::sha256(File::open(path)?)?, 0o644),
            );
        } else {
            return Err("reference archive notice is linked or special".into());
        }
        Ok(())
    }
    let mut members = BTreeMap::new();
    for name in deb::EXECUTABLES {
        let (archive, original) = if name == "among" {
            ("bin/among".into(), "usr/bin/among".into())
        } else {
            (
                format!("plugins/{name}"),
                format!("usr/share/among/plugins/{name}"),
            )
        };
        let original = storage::safe_path(payload, Path::new(&original))?;
        if !original.symlink_metadata()?.is_file() {
            return Err("reference executable is not regular".into());
        }
        members.insert(archive, (storage::sha256(File::open(original)?)?, 0o755));
    }
    let notices_root = storage::safe_path(payload, Path::new("usr/share/doc/among-terminal"))?;
    for name in ["LICENSE", "NOTICE", "THIRD_PARTY"] {
        notices(&notices_root, Path::new(name), &mut members)?;
    }
    Ok(members)
}

fn extract_tar(
    stream: impl Read,
    payload: &Path,
    destination: &Path,
    architecture: &str,
    expected_instructions: &str,
) -> Result<()> {
    let expected = expected_members(payload)?;
    let mut observed = BTreeSet::new();
    let mut archive = tar::Archive::new(stream);
    for entry in archive.entries()? {
        let mut entry = entry?;
        let path = entry.path()?.into_owned();
        storage::relative_path(&path)?;
        let name = path.to_str().ok_or("archive member path is not UTF-8")?;
        if !entry.header().entry_type().is_file() || !observed.insert(name.to_string()) {
            return Err("archive contains duplicate, linked or special members".into());
        }
        let mode = entry.header().mode()?;
        let mut bytes = Vec::new();
        entry.read_to_end(&mut bytes)?;
        if name == "INSTALL.txt" {
            if mode != 0o644 || bytes != expected_instructions.as_bytes() {
                return Err("invalid archive instructions".into());
            }
        } else {
            let (digest, expected_mode) = expected
                .get(name)
                .ok_or("unexpected binary archive member")?;
            if mode != *expected_mode || storage::sha256(bytes.as_slice())? != *digest {
                return Err("archive member differs from exact package payload".into());
            }
        }
        let output = storage::safe_path(destination, &path)?;
        fs::create_dir_all(output.parent().ok_or("archive member parent absent")?)?;
        storage::atomic_write(destination, &path, &bytes, mode)?;
    }
    if observed.len() != expected.len() + 1
        || !observed.contains("INSTALL.txt")
        || expected.keys().any(|name| !observed.contains(name))
    {
        return Err("archive is missing exact package executables or attribution".into());
    }
    for name in deb::EXECUTABLES {
        let relative = if name == "among" {
            "bin/among".into()
        } else {
            format!("plugins/{name}")
        };
        deb::elf_architecture(&destination.join(relative), architecture)?;
    }
    Ok(())
}

pub fn extract_checked(
    record: &serde_json::Value,
    directory: &Path,
    tag: &str,
    target: &str,
    architecture: &str,
    reference_payload: &Path,
    destination: &Path,
) -> Result<()> {
    validate(record, directory, tag, target, architecture)?;
    if destination.symlink_metadata().is_ok() {
        return Err("archive extraction destination must be new".into());
    }
    let parent = destination
        .parent()
        .ok_or("archive extraction parent absent")?
        .canonicalize()?;
    let scratch = storage::private_directory(&parent, "among-archive-extraction")?;
    let result = (|| -> Result<()> {
        let tar_path = scratch.join("payload.tar");
        let environment = BTreeMap::from([
            ("PATH".into(), "/usr/bin:/bin".into()),
            ("LC_ALL".into(), "C".into()),
        ]);
        let archive = storage::safe_path(
            directory,
            Path::new(
                record["binary_archive"]["filename"]
                    .as_str()
                    .ok_or("archive filename absent")?,
            ),
        )?;
        let compressed = fs::read(&archive)?;
        if record["binary_archive"]["sha256"] != storage::sha256(compressed.as_slice())? {
            return Err("binary archive changed before private extraction".into());
        }
        storage::atomic_write(&scratch, Path::new("verified.tar.gz"), &compressed, 0o600)?;
        let verified = scratch.join("verified.tar.gz");
        let mut gzip = OwnedProcess::spawn_split(
            Path::new("/usr/bin/gzip"),
            &["-dc", verified.to_str().ok_or("archive path is not UTF-8")?],
            &environment,
            &scratch,
            &File::create(&tar_path)?,
            &File::create(scratch.join("gzip.log"))?,
        )?;
        if !gzip.wait(Duration::from_secs(60))?.success() {
            return Err("binary archive decompression failed".into());
        }
        let output = scratch.join("bundle");
        fs::create_dir(&output)?;
        let dependencies = record["depends"]
            .as_str()
            .filter(|value| !value.is_empty())
            .ok_or("archive runtime dependency record absent")?;
        extract_tar(
            File::open(tar_path)?,
            reference_payload,
            &output,
            architecture,
            &instructions(tag, target, architecture, dependencies),
        )?;
        fs::set_permissions(&output, fs::Permissions::from_mode(0o755))?;
        storage::publish_directory(&output, destination)?;
        Ok(())
    })();
    fs::remove_dir_all(&scratch)?;
    result
}
