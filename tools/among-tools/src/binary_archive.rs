use crate::{deb, process::OwnedProcess, storage, Result};
use std::{
    collections::BTreeMap,
    fs::{self, File},
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
    let filename = format!(
        "among-{}-{target}-{architecture}.tar.gz",
        tag.strip_prefix('v').ok_or("archive tag lacks prefix")?
    );
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
        let instructions = format!("Among Terminal {tag}\nTarget: {target}, {architecture}; not a universal or self-contained Linux binary.\nRequired distribution packages: {dependencies}\n\nExtract this archive and run ./bin/among. No compiler or Rust toolchain is needed.\nLibraries are NOT bundled: install the distribution runtime dependencies above.\nPrefer the DEB if you want APT to install dependencies and a desktop launcher.\nOptional plugin executables are in plugins/ and remain disabled by default.\nMatching source and licence information accompanies the GitHub release.\n");
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
