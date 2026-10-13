//! Turns the workflow artifacts of one tag run into the exact file set that is
//! attached to the tag's GitHub Release draft.
//!
//! Every file is bound to the tag and commit by the record of the job that
//! built and installed it, and its build name is derived from the tag rather
//! than taken from that record. It is attached under a version-free name, so
//! `releases/latest/download/<name>` always serves the newest stable file, and
//! that name uses only characters GitHub keeps unchanged, so the written
//! `SHA256SUMS` still verifies the downloaded assets.

use crate::{appimage_build, appimage_sources, binary_archive, deb, storage, Result};
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
/// Four DEB, four binary archives, four RPM, two SRPM, two AppImages and the
/// two AppImage source archives; `SHA256SUMS` is written next to them.
pub const FILE_COUNT: usize = 18;

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
        names.insert(format!("appimage-sources-{architecture}"));
    }
    for target in RPM_TARGETS {
        names.insert(format!("rpm-checked-{target}-{SRPM_ARCHITECTURE}"));
    }
    names
}

/// Every attached name starts with the repository's and the DEB package's name.
const PREFIX: &str = "among-terminal";

/// DEB (`deb`), binary archive (`tar.gz`) or RPM (`rpm`) of one distribution
/// and architecture. RPMs also use `amd64`/`arm64`, so one download table
/// column serves every format; `dnf` reads the architecture from the package.
pub fn package_asset(target: &str, architecture: &str, extension: &str) -> String {
    format!("{PREFIX}-{target}-{architecture}.{extension}")
}

pub fn srpm_asset(target: &str) -> String {
    format!("{PREFIX}-{target}.src.rpm")
}

pub fn appimage_asset(architecture: &str) -> String {
    format!("{PREFIX}-{architecture}.AppImage")
}

pub fn source_archive_name(architecture: &str) -> String {
    format!("{PREFIX}-{architecture}-appimage-sources.tar")
}

/// The complete set of attached names apart from `SHA256SUMS`. It is the same
/// for every tag, which is what lets the README link the latest release.
pub fn attached_names() -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    for architecture in ARCHITECTURES {
        for target in DEB_TARGETS {
            names.insert(package_asset(target, architecture, "deb"));
            names.insert(package_asset(target, architecture, "tar.gz"));
        }
        for target in RPM_TARGETS {
            names.insert(package_asset(target, architecture, "rpm"));
        }
        names.insert(appimage_asset(architecture));
        names.insert(source_archive_name(architecture));
    }
    for target in RPM_TARGETS {
        names.insert(srpm_asset(target));
    }
    names
}

/// GitHub renames asset names with special characters and its documentation
/// does not give the exact rule; a name made only of these characters, not
/// starting or ending with a dot, has been observed to stay unchanged.
fn kept_by_github(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('.')
        && !name.ends_with('.')
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
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

/// One verified file: where it is, the name it is attached under, and its digest.
struct Selected {
    source: PathBuf,
    name: String,
    sha256: String,
}

fn select(
    directory: &Path,
    name: &str,
    attached: String,
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
        name: attached,
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
                package_asset(target, architecture, "deb"),
                &record["sha256"],
                Some(&sums),
            )?);
            selected.push(select(
                &directory,
                &archive,
                package_asset(target, architecture, "tar.gz"),
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
            let file = select(
                &directory,
                &package,
                package_asset(target, architecture, "rpm"),
                &record["sha256"],
                Some(&sums),
            )?;
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
        selected.push(select(
            &sources,
            &source,
            srpm_asset(target),
            &recorded,
            None,
        )?);
    }
    Ok(selected)
}

/// The corresponding source of one AppImage's bundled libraries, verified file
/// by file and attached as one archive. GitHub would accept every file as its
/// own asset (up to 1000 per release, each under 2 GiB), but Debian source file
/// names carry characters such as `+` that GitHub renames on upload, and one
/// archive per AppImage keeps the association between image and source plain.
struct SourceBundle {
    directory: PathBuf,
    files: Vec<String>,
    prefix: String,
    name: String,
}

fn appimages(inputs: &Path, tag: &str) -> Result<(Vec<Selected>, Vec<SourceBundle>)> {
    let version = tag
        .strip_prefix('v')
        .ok_or("release tag lacks its v prefix")?;
    let mut selected = Vec::new();
    let mut bundles = Vec::new();
    for architecture in ARCHITECTURES {
        let directory = storage::safe_path(inputs, Path::new(&format!("appimage-{architecture}")))?;
        let sums = checked_sums(&directory)?;
        let name = appimage_build::file_name(version, architecture)?;
        only(&directory, ".AppImage", &name)?;
        let recorded = Value::from(sums.get(&name).cloned());
        let file = select(
            &directory,
            &name,
            appimage_asset(architecture),
            &recorded,
            None,
        )?;
        deb::elf_architecture(&file.source, architecture).map_err(|error| {
            format!(
                "{}: {name} is not an {architecture} executable: {error}",
                directory.display()
            )
        })?;
        let audit = storage::parse_json(&fs::read(storage::safe_path(
            &directory,
            Path::new(&format!("among-{version}-{architecture}.audit.json")),
        )?)?)?;
        if audit["appimage"]["filename"] != name.as_str()
            || audit["appimage"]["sha256"] != file.sha256.as_str()
            || audit["architecture"] != architecture
        {
            return Err(format!(
                "{}: the audit record describes another AppImage",
                directory.display()
            )
            .into());
        }
        bundles.push(appimage_sources_bundle(inputs, architecture, &audit)?);
        selected.push(file);
    }
    Ok((selected, bundles))
}

/// Accepts the source directory only if `sources.json` belongs to the attached
/// AppImage, names exactly the source packages its audit says are bundled,
/// every descriptor and listed file verifies, and nothing else is present.
fn appimage_sources_bundle(
    inputs: &Path,
    architecture: &str,
    audit: &Value,
) -> Result<SourceBundle> {
    let directory = storage::safe_path(
        inputs,
        Path::new(&format!("appimage-sources-{architecture}")),
    )?;
    let fail = |what: &str| -> crate::Result<()> {
        Err(format!("{}: {what}", directory.display()).into())
    };
    let index = storage::parse_json(&fs::read(storage::safe_path(
        &directory,
        Path::new("sources.json"),
    )?)?)?;
    if index["appimage"] != audit["appimage"] || index["architecture"] != architecture {
        fail("sources.json belongs to another AppImage")?;
    }
    let records = index["sources"]
        .as_array()
        .ok_or("sources.json lists no sources")?;
    let mut named = BTreeSet::new();
    let mut files = BTreeSet::from(["sources.json".to_string()]);
    for record in records {
        let source = record["source_package"]
            .as_str()
            .ok_or("source record lacks its package")?;
        let version = record["source_version"]
            .as_str()
            .ok_or("source record lacks its version")?;
        let verified = appimage_sources::verify_descriptor(&directory, source, version)?;
        if verified["files"] != record["files"] {
            fail(&format!("{source} {version} differs from its descriptor"))?;
        }
        named.insert((source.to_string(), version.to_string()));
        for file in verified["files"].as_array().ok_or("descriptor files")? {
            files.insert(
                file["name"]
                    .as_str()
                    .ok_or("descriptor file name")?
                    .to_string(),
            );
        }
    }
    if named != appimage_sources::source_set(audit)? || named.len() != records.len() {
        fail("sources.json does not name exactly the bundled source packages")?;
    }
    let mut present = BTreeSet::new();
    for entry in fs::read_dir(&directory)? {
        let entry = entry?;
        if !entry.file_type()?.is_file() {
            fail("holds something other than regular files")?;
        }
        present.insert(
            entry
                .file_name()
                .into_string()
                .map_err(|_| "source file name is not UTF-8")?,
        );
    }
    if present != files {
        fail("holds files its descriptors do not list")?;
    }
    let name = source_archive_name(architecture);
    Ok(SourceBundle {
        directory,
        files: files.into_iter().collect(),
        prefix: name.trim_end_matches(".tar").to_string(),
        name,
    })
}

/// A reproducible tar: sorted members, fixed owner, mode and time.
fn write_bundle(bundle: &SourceBundle, destination: &Path) -> Result<()> {
    let file = File::options()
        .write(true)
        .create_new(true)
        .open(destination)?;
    let mut builder = tar::Builder::new(file);
    for name in &bundle.files {
        let path = storage::safe_path(&bundle.directory, Path::new(name))?;
        let mut header = tar::Header::new_gnu();
        header.set_size(path.metadata()?.len());
        header.set_mode(0o644);
        header.set_uid(0);
        header.set_gid(0);
        header.set_mtime(0);
        header.set_entry_type(tar::EntryType::Regular);
        header.set_cksum();
        builder.append_data(
            &mut header,
            format!("{}/{name}", bundle.prefix),
            File::open(&path)?,
        )?;
    }
    builder.into_inner()?.sync_all()?;
    Ok(())
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
    let (images, bundles) = appimages(inputs, tag)?;
    selected.extend(images);
    let mut names = BTreeSet::new();
    for name in selected
        .iter()
        .map(|file| &file.name)
        .chain(bundles.iter().map(|bundle| &bundle.name))
    {
        if !kept_by_github(name) || !names.insert(name.clone()) {
            return Err(format!("release asset name {name} is not unique or not kept").into());
        }
    }
    if names != attached_names() || names.len() != FILE_COUNT {
        return Err(format!("expected the {FILE_COUNT} attached names, selected {names:?}").into());
    }
    fs::DirBuilder::new().mode(0o755).create(output)?;
    let mut digests = BTreeMap::new();
    for file in &selected {
        let destination = output.join(&file.name);
        fs::copy(&file.source, &destination)?;
        if digest(&destination)? != file.sha256 {
            return Err(format!("{} changed while it was copied", file.name).into());
        }
        digests.insert(file.name.clone(), file.sha256.clone());
    }
    for bundle in &bundles {
        let destination = output.join(&bundle.name);
        write_bundle(bundle, &destination)?;
        digests.insert(bundle.name.clone(), digest(&destination)?);
    }
    let sums = digests
        .iter()
        .map(|(name, sha256)| format!("{sha256}  {name}\n"))
        .collect::<String>();
    storage::atomic_write(output, Path::new("SHA256SUMS"), sums.as_bytes(), 0o644)?;
    Ok(digests.into_keys().collect())
}
