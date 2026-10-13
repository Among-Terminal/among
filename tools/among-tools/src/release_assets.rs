//! Turns the workflow artifacts of one tag run into the exact file set that is
//! attached to the tag's GitHub Release draft.
//!
//! Every file is bound to the tag and commit by the record of the job that
//! built and installed it, its name is derived from the tag rather than taken
//! from that record, and it is renamed to a form GitHub keeps unchanged so the
//! written `SHA256SUMS` still verifies the downloaded assets.

use crate::{binary_archive, deb, storage, Result};
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File},
    os::unix::fs::DirBuilderExt,
    path::{Path, PathBuf},
};

pub const ARCHITECTURES: [&str; 2] = ["amd64", "arm64"];
pub const DEB_TARGETS: [&str; 2] = ["debian13", "ubuntu24.04"];
pub const RPM_TARGETS: [&str; 2] = ["fedora43", "fedora44"];
/// Both architectures build an SRPM with the same name but different bytes,
/// and a release cannot hold two assets with one name; the amd64 build's SRPM
/// is the one attached.
pub const SRPM_ARCHITECTURE: &str = "amd64";
/// Sixteen release files; `SHA256SUMS` is written next to them.
pub const FILE_COUNT: usize = 16;

/// Exact names of the workflow artifacts the assembly reads. Name patterns are
/// not used because `deb-*` would also match runtime evidence and failure logs.
pub fn input_artifacts() -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    for architecture in ARCHITECTURES {
        for target in DEB_TARGETS {
            names.insert(format!("deb-{target}-{architecture}"));
        }
        for target in RPM_TARGETS {
            names.insert(format!("rpm-{target}-{architecture}"));
        }
        names.insert(format!("appimage-{architecture}"));
    }
    for target in RPM_TARGETS {
        names.insert(format!("rpm-checked-{target}-{SRPM_ARCHITECTURE}"));
    }
    names
}

/// GitHub renames asset names with special characters; its documentation does
/// not give the exact rule. Release names here map the one character package
/// versions need (`~`, observed to become `.`) and refuse anything else that is
/// not plainly safe, so the uploaded name is predictable.
pub fn asset_name(name: &str) -> Result<String> {
    let mapped = name.replace('~', ".");
    if mapped.is_empty()
        || mapped.starts_with('.')
        || mapped.ends_with('.')
        || !mapped
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    {
        return Err(format!("{name:?} has no release asset name GitHub keeps unchanged").into());
    }
    Ok(mapped)
}

fn rpm_machine(architecture: &str) -> Result<&'static str> {
    match architecture {
        "amd64" => Ok("x86_64"),
        "arm64" => Ok("aarch64"),
        _ => Err("unsupported RPM architecture".into()),
    }
}

fn rpm_filename(tag: &str, target: &str, machine: &str) -> Result<String> {
    let (_, version) = deb::release_version(tag)?;
    let release = target
        .strip_prefix("fedora")
        .ok_or("RPM target is not a Fedora release")?;
    Ok(format!("among-{version}-1.fc{release}.{machine}.rpm"))
}

fn digest(path: &Path) -> Result<String> {
    storage::sha256(File::open(path)?)
}

fn is_digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
}

/// Reads an artifact's own checksum list and verifies every file it names.
fn checked_sums(directory: &Path) -> Result<BTreeMap<String, String>> {
    let text = fs::read_to_string(storage::safe_path(directory, Path::new("SHA256SUMS"))?)
        .map_err(|error| format!("{}: SHA256SUMS unreadable: {error}", directory.display()))?;
    let mut sums = BTreeMap::new();
    for line in text.lines() {
        let (sum, name) = line
            .split_once("  ")
            .or_else(|| line.split_once(" *"))
            .ok_or_else(|| format!("{}: malformed SHA256SUMS line", directory.display()))?;
        storage::relative_path(Path::new(name))?;
        if !is_digest(sum) || sums.insert(name.to_string(), sum.to_string()).is_some() {
            return Err(format!(
                "{}: invalid or repeated SHA256SUMS entry",
                directory.display()
            )
            .into());
        }
        if digest(&storage::safe_path(directory, Path::new(name))?)? != sum {
            return Err(format!(
                "{}: {name} does not match its SHA256SUMS entry",
                directory.display()
            )
            .into());
        }
    }
    if sums.is_empty() {
        return Err(format!("{}: SHA256SUMS lists nothing", directory.display()).into());
    }
    Ok(sums)
}

fn record(directory: &Path) -> Result<Value> {
    storage::parse_json(&fs::read(storage::safe_path(
        directory,
        Path::new("build-record.json"),
    )?)?)
}

fn require_identity(
    record: &Value,
    directory: &Path,
    tag: &str,
    commit: &str,
    target: &str,
    architecture: &str,
) -> Result<()> {
    for (key, expected) in [
        ("tag", tag),
        ("source_sha", commit),
        ("target", target),
        ("architecture", architecture),
    ] {
        if record[key].as_str() != Some(expected) {
            return Err(format!(
                "{}: build record {key} is {}, expected {expected}",
                directory.display(),
                record[key]
            )
            .into());
        }
    }
    Ok(())
}

/// Top-level regular files of a directory with the given suffix.
fn files_with_suffix(directory: &Path, suffix: &str) -> Result<Vec<String>> {
    let mut names = Vec::new();
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| "artifact file name is not UTF-8")?;
        if name.ends_with(suffix) && entry.file_type()?.is_file() {
            names.push(name);
        }
    }
    names.sort();
    Ok(names)
}

/// One verified file: where it is, the name it gets, and its digest.
struct Selected {
    source: PathBuf,
    name: String,
    sha256: String,
}

fn select(
    directory: &Path,
    name: &str,
    recorded: &Value,
    sums: Option<&BTreeMap<String, String>>,
) -> Result<Selected> {
    let source = storage::safe_path(directory, Path::new(name))?;
    if !source.symlink_metadata()?.file_type().is_file() {
        return Err(format!("{}: {name} is not a regular file", directory.display()).into());
    }
    let sha256 = digest(&source)?;
    if recorded.as_str() != Some(sha256.as_str()) {
        return Err(format!(
            "{}: {name} differs from the digest its build recorded",
            directory.display()
        )
        .into());
    }
    if let Some(sums) = sums {
        if sums.get(name) != Some(&sha256) {
            return Err(format!(
                "{}: {name} is not listed with its digest in SHA256SUMS",
                directory.display()
            )
            .into());
        }
    }
    Ok(Selected {
        source,
        name: asset_name(name)?,
        sha256,
    })
}

fn only(directory: &Path, suffix: &str, expected: &str) -> Result<()> {
    let found = files_with_suffix(directory, suffix)?;
    if found != [expected] {
        return Err(format!(
            "{}: expected exactly {expected}, found {found:?}",
            directory.display()
        )
        .into());
    }
    Ok(())
}

fn debian(inputs: &Path, tag: &str, commit: &str) -> Result<Vec<Selected>> {
    let mut selected = Vec::new();
    for target in DEB_TARGETS {
        for architecture in ARCHITECTURES {
            let directory =
                storage::safe_path(inputs, Path::new(&format!("deb-{target}-{architecture}")))?;
            let sums = checked_sums(&directory)?;
            let record = record(&directory)?;
            require_identity(&record, &directory, tag, commit, target, architecture)?;
            let package = deb::package_filename(&deb::package_version(tag, target)?, architecture);
            let archive = binary_archive::filename(tag, target, architecture)?;
            if record["filename"].as_str() != Some(package.as_str())
                || record["binary_archive"]["filename"].as_str() != Some(archive.as_str())
            {
                return Err(format!(
                    "{}: build record names unexpected files",
                    directory.display()
                )
                .into());
            }
            only(&directory, ".deb", &package)?;
            only(&directory, ".tar.gz", &archive)?;
            selected.push(select(
                &directory,
                &package,
                &record["sha256"],
                Some(&sums),
            )?);
            selected.push(select(
                &directory,
                &archive,
                &record["binary_archive"]["sha256"],
                Some(&sums),
            )?);
        }
    }
    Ok(selected)
}

/// `rpm-digests.txt` lines are `<sha256>  <absolute build path>`; the file
/// name is what binds them to the artifact's files.
fn build_digests(directory: &Path) -> Result<BTreeMap<String, String>> {
    let text = fs::read_to_string(storage::safe_path(directory, Path::new("rpm-digests.txt"))?)?;
    let mut digests = BTreeMap::new();
    for line in text.lines() {
        let (sum, path) = line
            .split_once("  ")
            .ok_or("malformed rpm-digests.txt line")?;
        let name = Path::new(path)
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or("rpm-digests.txt names no file")?;
        if !is_digest(sum) || digests.insert(name.to_string(), sum.to_string()).is_some() {
            return Err("invalid or repeated rpm-digests.txt entry".into());
        }
    }
    Ok(digests)
}

fn fedora(inputs: &Path, tag: &str, commit: &str) -> Result<Vec<Selected>> {
    let mut selected = Vec::new();
    let mut checked = BTreeMap::new();
    for target in RPM_TARGETS {
        for architecture in ARCHITECTURES {
            let directory =
                storage::safe_path(inputs, Path::new(&format!("rpm-{target}-{architecture}")))?;
            let sums = checked_sums(&directory)?;
            let record = record(&directory)?;
            require_identity(&record, &directory, tag, commit, target, architecture)?;
            let package = rpm_filename(tag, target, rpm_machine(architecture)?)?;
            if record["filename"].as_str() != Some(package.as_str()) {
                return Err(format!(
                    "{}: build record names an unexpected RPM",
                    directory.display()
                )
                .into());
            }
            // Only top-level files are candidates: the older package in
            // `ci-lifecycle-fixture-not-for-release/` is upgrade-test input.
            only(&directory, ".rpm", &package)?;
            let file = select(&directory, &package, &record["sha256"], Some(&sums))?;
            checked.insert((target, architecture), (package, file.sha256.clone()));
            selected.push(file);
        }
        // The SRPM is not installed or recorded by the checked build; it is
        // accepted only from the build whose binary RPM passed those checks.
        let directory = storage::safe_path(
            inputs,
            Path::new(&format!("rpm-checked-{target}-{SRPM_ARCHITECTURE}")),
        )?;
        let digests = build_digests(&directory)?;
        let (package, package_sha256) = checked
            .get(&(target, SRPM_ARCHITECTURE))
            .ok_or("checked binary RPM absent")?;
        if digests.get(package) != Some(package_sha256) {
            return Err(format!(
                "{}: SRPM build did not produce the checked binary RPM",
                directory.display()
            )
            .into());
        }
        let source = rpm_filename(tag, target, "src")?;
        let sources = storage::safe_path(&directory, Path::new("rpmbuild/SRPMS"))?;
        only(&sources, ".rpm", &source)?;
        if digests.len() != 2 {
            return Err(format!(
                "{}: rpm-digests.txt must list one RPM and one SRPM",
                directory.display()
            )
            .into());
        }
        let recorded = Value::from(digests.get(&source).cloned());
        selected.push(select(&sources, &source, &recorded, None)?);
    }
    Ok(selected)
}

fn appimages(inputs: &Path) -> Result<Vec<Selected>> {
    let mut selected = Vec::new();
    for architecture in ARCHITECTURES {
        let directory = storage::safe_path(inputs, Path::new(&format!("appimage-{architecture}")))?;
        let sums = checked_sums(&directory)?;
        let found = files_with_suffix(&directory, ".AppImage")?;
        let [name] = found.as_slice() else {
            return Err(format!(
                "{}: expected exactly one AppImage, found {found:?}",
                directory.display()
            )
            .into());
        };
        let recorded = Value::from(sums.get(name).cloned());
        let file = select(&directory, name, &recorded, None)?;
        // The name is the producer's choice; the bytes decide the architecture.
        deb::elf_architecture(&file.source, architecture).map_err(|error| {
            format!(
                "{}: {name} is not an {architecture} executable: {error}",
                directory.display()
            )
        })?;
        selected.push(file);
    }
    Ok(selected)
}

/// Verifies the inputs, then writes the release files and `SHA256SUMS` into
/// `output`, which must not exist yet. Returns the asset names in order.
pub fn stage(inputs: &Path, tag: &str, commit: &str, output: &Path) -> Result<Vec<String>> {
    deb::release_version(tag)?;
    if commit.len() != 40
        || !commit
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
    {
        return Err("commit must be a full lowercase hexadecimal SHA-1".into());
    }
    if output.symlink_metadata().is_ok() {
        return Err("release asset output must be new".into());
    }
    let mut selected = debian(inputs, tag, commit)?;
    selected.extend(fedora(inputs, tag, commit)?);
    selected.extend(appimages(inputs)?);
    let mut names = BTreeSet::new();
    for file in &selected {
        if file.name == "SHA256SUMS" || !names.insert(file.name.clone()) {
            return Err(format!("release asset name {} is not unique", file.name).into());
        }
    }
    if selected.len() != FILE_COUNT {
        return Err(format!(
            "expected {FILE_COUNT} release files, selected {}",
            selected.len()
        )
        .into());
    }
    selected.sort_by(|left, right| left.name.cmp(&right.name));
    fs::DirBuilder::new().mode(0o755).create(output)?;
    let mut sums = String::new();
    for file in &selected {
        let destination = output.join(&file.name);
        fs::copy(&file.source, &destination)?;
        if digest(&destination)? != file.sha256 {
            return Err(format!("{} changed while it was copied", file.name).into());
        }
        sums.push_str(&format!("{}  {}\n", file.sha256, file.name));
    }
    storage::atomic_write(output, Path::new("SHA256SUMS"), sums.as_bytes(), 0o644)?;
    Ok(selected.into_iter().map(|file| file.name).collect())
}
