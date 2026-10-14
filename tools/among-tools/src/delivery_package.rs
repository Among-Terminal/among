use crate::{
    apt_provenance, native_notices, native_sources, process::OwnedProcess, source_snapshot,
    storage, Result,
};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::path::{Path, PathBuf};
use std::time::Duration;

pub struct Inputs<'a> {
    pub snap: &'a Path,
    pub snapshot: &'a Path,
    pub native: &'a Path,
    pub sources: &'a Path,
    pub metadata: &'a Path,
    pub prime: &'a Path,
    pub builder: &'a Path,
    pub instructions: &'a Path,
}
fn regular(path: &Path) -> Result<File> {
    if !path.symlink_metadata()?.file_type().is_file() {
        return Err("delivery input must be a regular non-symlink file".into());
    }
    Ok(File::open(path)?)
}
fn collect(
    root: &Path,
    relative: &Path,
    prefix: &str,
    files: &mut BTreeMap<String, PathBuf>,
) -> Result<()> {
    let directory = if relative.as_os_str().is_empty() {
        root.to_path_buf()
    } else {
        storage::safe_path(root, relative)?
    };
    if directory.symlink_metadata()?.file_type().is_symlink() || !directory.is_dir() {
        return Err("delivery directory must not be a symlink".into());
    }
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let relative = relative.join(entry.file_name());
        let path = storage::safe_path(root, &relative)?;
        if path.is_dir() {
            collect(root, &relative, prefix, files)?;
        } else {
            regular(&path)?;
            let name = format!(
                "{prefix}/{}",
                relative.to_str().ok_or("delivery path is not UTF-8")?
            );
            if files.insert(name, path).is_some() {
                return Err("duplicate delivery member".into());
            }
        }
    }
    Ok(())
}
fn append(
    archive: &mut tar::Builder<File>,
    name: &str,
    mut input: File,
    hashes: &mut BTreeMap<String, String>,
) -> Result<()> {
    storage::relative_path(Path::new(name))?;
    let digest = storage::sha256(input.try_clone()?)?;
    // try_clone shares the descriptor offset. Reset before streaming into tar.
    use std::io::{Seek, SeekFrom};
    input.seek(SeekFrom::Start(0))?;
    let mut header = tar::Header::new_gnu();
    header.set_size(input.metadata()?.len());
    header.set_mode(0o644);
    header.set_uid(0);
    header.set_gid(0);
    header.set_mtime(0);
    header.set_cksum();
    archive.append_data(&mut header, name, input)?;
    hashes.insert(name.into(), digest);
    Ok(())
}
fn append_bytes(
    archive: &mut tar::Builder<File>,
    name: &str,
    bytes: &[u8],
    hashes: &mut BTreeMap<String, String>,
) -> Result<()> {
    let mut header = tar::Header::new_gnu();
    header.set_size(bytes.len() as u64);
    header.set_mode(0o644);
    header.set_uid(0);
    header.set_gid(0);
    header.set_mtime(0);
    header.set_cksum();
    archive.append_data(&mut header, name, bytes)?;
    hashes.insert(name.into(), storage::sha256(bytes)?);
    Ok(())
}

pub fn write_bundle(inputs: &Inputs<'_>, output: &Path, trust: &Value) -> Result<Value> {
    regular(inputs.snap)?;
    regular(inputs.snapshot)?;
    regular(inputs.instructions)?;
    source_snapshot::check(inputs.snapshot)?;
    let inventory =
        crate::source_delivery::inventory(regular(inputs.snapshot)?, "source-inventory.json")?;
    if !inventory.contains_key("snap/snapcraft.yaml") {
        return Err("source delivery snapshot is missing the Snap build recipe".into());
    }
    let snapshot_record = storage::parse_json(&fs::read(native_sources::regular(
        inputs.builder,
        "source-snapshot.json",
    )?)?)?;
    let original = storage::sha256(regular(inputs.snapshot)?)?;
    if snapshot_record["archive_sha256"] != original {
        return Err("snapshot differs from preserved builder input".into());
    }
    let mut files = BTreeMap::from([
        ("original-source.tar".into(), inputs.snapshot.to_path_buf()),
        (
            "BUILD-AND-MODIFY.md".into(),
            inputs.instructions.to_path_buf(),
        ),
    ]);
    // builder-python.txt records the external Snapcraft environment, not a
    // project interpreter requirement. Preserve original provenance unchanged.
    for name in [
        "Dockerfile",
        "runner.sh",
        "builder-python.txt",
        "builder-packages.txt",
        "source-snapshot.json",
    ] {
        files.insert(
            format!("build-environment/{name}"),
            native_sources::regular(inputs.builder, name)?,
        );
    }
    for (prefix, root) in [
        ("native-sources", inputs.sources),
        ("native-audit", inputs.native),
        ("apt-authentication", inputs.metadata),
    ] {
        collect(root, Path::new(""), prefix, &mut files)?;
    }
    let common =
        storage::safe_path(inputs.prime, Path::new("usr/share/common-licenses"))?.canonicalize()?;
    for entry in fs::read_dir(&common)? {
        let entry = entry?;
        let path = entry.path().canonicalize()?;
        if path.parent() != Some(common.as_path()) || !path.is_file() {
            return Err("escaping or nonregular common-license alias".into());
        }
        files.insert(
            format!(
                "common-licenses/{}",
                entry
                    .file_name()
                    .to_str()
                    .ok_or("license alias is not UTF-8")?
            ),
            path,
        );
    }
    fs::create_dir(output)?;
    let bundle = output.join("among-source-materials.tar");
    let mut archive = tar::Builder::new(
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&bundle)?,
    );
    let mut hashes = BTreeMap::new();
    for (name, path) in files {
        append(&mut archive, &name, regular(&path)?, &mut hashes)?;
    }
    let mut authentication = serde_json::to_vec_pretty(trust)?;
    authentication.push(b'\n');
    append_bytes(
        &mut archive,
        "authentication-result.json",
        &authentication,
        &mut hashes,
    )?;
    let mut inventory = serde_json::to_vec_pretty(&json!({"schema":1,"files":hashes}))?;
    inventory.push(b'\n');
    append_bytes(
        &mut archive,
        "delivery-inventory.json",
        &inventory,
        &mut hashes,
    )?;
    archive.into_inner()?.sync_all()?;
    let record = json!({"schema":1,"snap":{"filename":inputs.snap.file_name().and_then(|name|name.to_str()).ok_or("Snap filename not UTF-8")?,"sha256":storage::sha256(regular(inputs.snap)?)?},"source_bundle":{"filename":"among-source-materials.tar","sha256":storage::sha256(regular(&bundle)?)?},"original_source_sha256":original,"authentication":trust});
    let mut bytes = serde_json::to_vec_pretty(&record)?;
    bytes.push(b'\n');
    storage::atomic_write(output, Path::new("source-association.json"), &bytes, 0o644)?;
    crate::source_delivery::check(
        &output.join("source-association.json"),
        &bundle,
        inputs.snap,
    )?;
    Ok(record)
}

pub fn package(inputs: &Inputs<'_>, output: &Path, scratch: &Path) -> Result<Value> {
    // Authentication, notice and immutable executable checks precede output.
    if output.symlink_metadata().is_ok() {
        return Err("source delivery output must be new".into());
    }
    regular(inputs.snap)?;
    regular(inputs.snapshot)?;
    let native = storage::parse_json(&fs::read(native_sources::regular(
        inputs.native,
        "inventory.json",
    )?)?)?;
    let trust = apt_provenance::authenticate(inputs.metadata, &native, inputs.sources, scratch)?;
    native_notices::check(&native, inputs.prime)?;
    compare_snap_payload(inputs.snap, inputs.prime, scratch)?;
    write_bundle(inputs, output, &trust)
}

/// Read the executable, plugin and metadata members out of the actual Snap
/// with `unsquashfs` and require the bytes the audit saw in prime.
///
/// The association published next to the Snap claims these sources build
/// this binary; a Snap repacked after the audit would make that claim false.
pub fn compare_snap_payload(snap: &Path, prime: &Path, scratch: &Path) -> Result<()> {
    let base = storage::private_directory(scratch, "among-delivery-prime")?;
    let env = BTreeMap::from([
        ("PATH".into(), "/usr/bin:/bin".into()),
        ("LC_ALL".into(), "C.UTF-8".into()),
    ]);
    for (index, name) in [
        "bin/among",
        "usr/share/among/plugins/clock",
        "usr/share/among/plugins/system-status",
        "usr/share/among/plugins/pane-pwd",
        "meta/snap.yaml",
    ]
    .iter()
    .enumerate()
    {
        let actual = base.join(format!("member-{index}"));
        let errors = File::create(base.join(format!("member-{index}.stderr")))?;
        let mut process = OwnedProcess::spawn_split(
            Path::new("/usr/bin/unsquashfs"),
            &["-cat", snap.to_str().ok_or("Snap path not UTF-8")?, name],
            &env,
            &base,
            &File::create(&actual)?,
            &errors,
        )?;
        if !process.wait(Duration::from_secs(60))?.success()
            || storage::sha256(File::open(actual)?)?
                != storage::sha256(File::open(native_sources::regular(prime, name)?)?)?
        {
            return Err("Snap differs from audited prime executable/metadata bytes".into());
        }
    }
    Ok(())
}
