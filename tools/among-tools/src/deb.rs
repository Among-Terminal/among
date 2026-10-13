use crate::{storage, Result};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::Path;

pub const APP_ID: &str = "com.github.among-terminal";
pub const EXECUTABLES: [&str; 4] = ["among", "clock", "system-status", "pane-pwd"];

pub fn stable_version(tag: &str) -> Result<&str> {
    let version = tag
        .strip_prefix('v')
        .ok_or("expected stable vMAJOR.MINOR.PATCH tag")?;
    let parts = version.split('.').collect::<Vec<_>>();
    if parts.len() != 3
        || !parts.iter().all(|part| {
            !part.is_empty()
                && part.bytes().all(|byte| byte.is_ascii_digit())
                && (*part == "0" || !part.starts_with('0'))
        })
    {
        return Err("expected stable vMAJOR.MINOR.PATCH tag".into());
    }
    Ok(version)
}

// RC tags build installable previews without granting stable promotion authority.
pub fn release_version(tag: &str) -> Result<(&str, String)> {
    if let Some((base, candidate)) = tag.split_once("-rc.") {
        let version = stable_version(base)?;
        if candidate.is_empty()
            || candidate.starts_with('0')
            || !candidate.bytes().all(|byte| byte.is_ascii_digit())
        {
            return Err("expected positive canonical rc.N suffix".into());
        }
        Ok((version, format!("{version}~rc.{candidate}")))
    } else {
        let version = stable_version(tag)?;
        Ok((version, version.into()))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Identity {
    pub target: String,
    pub architecture: String,
    pub upstream_version: String,
    pub package_version: String,
    pub filename: String,
}
pub fn identity(root: &Path, tag: &str, target: &str, architecture: &str) -> Result<Identity> {
    let (version, _) = release_version(tag)?;
    if !matches!(target, "debian13" | "ubuntu24.04") || !matches!(architecture, "amd64" | "arm64") {
        return Err("unsupported distribution or architecture".into());
    }
    for manifest in ["Cargo.toml", "examples/plugins/Cargo.toml"] {
        let document: toml::Value = toml::from_str(&fs::read_to_string(storage::safe_path(
            root,
            Path::new(manifest),
        )?)?)?;
        if document
            .get("package")
            .and_then(|package| package.get("version"))
            .and_then(toml::Value::as_str)
            != Some(version)
        {
            return Err(format!("tag version differs from {manifest}").into());
        }
    }
    let text = fs::read_to_string(storage::safe_path(
        root,
        Path::new(&format!("data/{APP_ID}.metainfo.xml")),
    )?)?;
    let document = roxmltree::Document::parse(&text)?;
    let release = document
        .root_element()
        .children()
        .find(|node| node.has_tag_name("releases"))
        .and_then(|node| node.children().find(|node| node.has_tag_name("release")));
    if release.and_then(|node| node.attribute("version")) != Some(version) {
        return Err("AppStream release version differs from tag".into());
    }
    let package_version = package_version(tag, target)?;
    Ok(Identity {
        target: target.into(),
        architecture: architecture.into(),
        upstream_version: version.into(),
        filename: package_filename(&package_version, architecture),
        package_version,
    })
}
/// The Debian package version a tag and distribution produce, such as
/// `0.1.0~rc.11-1~debian13`.
pub fn package_version(tag: &str, target: &str) -> Result<String> {
    let (_, debian_version) = release_version(tag)?;
    Ok(format!("{debian_version}-1~{target}"))
}

pub fn package_filename(package_version: &str, architecture: &str) -> String {
    format!("among-terminal_{package_version}_{architecture}.deb")
}

pub fn unique_inventory(records: &[Identity]) -> Result<()> {
    let mut filenames = BTreeSet::new();
    let mut targets = BTreeSet::new();
    for record in records {
        if !filenames.insert(&record.filename)
            || !targets.insert((&record.target, &record.architecture))
        {
            return Err("duplicate filename or native target identity".into());
        }
    }
    Ok(())
}

pub fn check_source_inputs(root: &Path) -> Result<()> {
    let rules = fs::read_to_string(storage::safe_path(
        root,
        Path::new("packaging/debian/rules"),
    )?)?;
    let control = fs::read_to_string(storage::safe_path(
        root,
        Path::new("packaging/debian/control"),
    )?)?;
    if !rules.contains("cargo +1.98.1 build --release --locked --bins\n")
        || !rules.contains("--locked --bins --manifest-path examples/plugins/Cargo.toml")
        || rules.contains("install-plugins.sh")
        || rules.contains("scripts/install.sh")
        || !control.contains("Maintainer: Among Terminal <hello+amongterminal@highload.zone>")
        || !control.contains("Rules-Requires-Root: no")
    {
        return Err(
            "Debian build inputs lost locked native builds or user-installer isolation".into(),
        );
    }
    let manifest: toml::Value = toml::from_str(&fs::read_to_string(storage::safe_path(
        root,
        Path::new("Cargo.toml"),
    )?)?)?;
    let package = manifest.get("package").ok_or("root package missing")?;
    let repository = package
        .get("repository")
        .and_then(toml::Value::as_str)
        .ok_or("root repository missing")?;
    let name = package
        .get("name")
        .and_then(toml::Value::as_str)
        .ok_or("root package name missing")?;
    // Keep executable install/remove instructions bound to this package's identity.
    let install_guide =
        fs::read_to_string(storage::safe_path(root, Path::new("docs/install.md"))?)?;
    if !install_guide.contains(&format!("cargo install --git {repository} --locked {name}"))
        || !install_guide.contains(&format!("cargo uninstall {name}"))
        || package.get("publish").and_then(toml::Value::as_bool) != Some(false)
    {
        return Err("Cargo installation guide/package identity differs".into());
    }
    Ok(())
}
pub fn elf_architecture(path: &Path, architecture: &str) -> Result<()> {
    use std::io::Read;
    let mut file = fs::File::open(path)?;
    let mut data = [0u8; 20];
    file.read_exact(&mut data)?;
    if &data[..4] != b"\x7fELF" || data[4..6] != [2, 1] {
        return Err("expected 64-bit little-endian ELF executable".into());
    }
    let expected = match architecture {
        "amd64" => 62,
        "arm64" => 183,
        _ => return Err("unsupported ELF target".into()),
    };
    if u16::from_le_bytes([data[18], data[19]]) != expected {
        return Err("ELF architecture differs from package target".into());
    }
    Ok(())
}

pub(crate) fn repository_directory(root: &Path) -> Result<std::path::PathBuf> {
    let root = root.canonicalize()?;
    for directory in root.ancestors() {
        if directory.join(".git").symlink_metadata().is_ok() {
            return Ok(directory.to_path_buf());
        }
    }
    Err("release checkout has no Git repository".into())
}

pub fn github_tag_source(
    root: &Path,
    environment: &BTreeMap<String, String>,
) -> Result<(String, String)> {
    if environment.get("GITHUB_ACTIONS").map(String::as_str) != Some("true")
        || environment.get("GITHUB_EVENT_NAME").map(String::as_str) != Some("push")
    {
        return Err("package construction is restricted to GitHub release-tag push CI".into());
    }
    let reference = environment
        .get("GITHUB_REF")
        .ok_or("package construction requires a tag ref")?;
    let tag = reference
        .strip_prefix("refs/tags/")
        .ok_or("package construction requires a tag ref")?;
    release_version(tag)?;
    let repository = repository_directory(root)?;
    let safe_directory = format!(
        "safe.directory={}",
        repository
            .to_str()
            .ok_or("Git repository path is not UTF-8")?
    );
    let scratch = std::env::temp_dir().join(format!(
        "among-deb-git-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos()
    ));
    fs::DirBuilder::new().mode(0o700).create(&scratch)?;
    let result = (|| -> Result<(String, String)> {
        let env = [
            ("PATH", "/usr/bin:/bin"),
            ("GIT_CONFIG_NOSYSTEM", "1"),
            ("GIT_CONFIG_GLOBAL", "/dev/null"),
            ("GIT_TERMINAL_PROMPT", "0"),
        ]
        .into_iter()
        .map(|(key, value)| (key.into(), value.into()))
        .collect();
        let git = |args: &[&str]| -> Result<String> {
            let file = fs::File::create(scratch.join("git.log"))?;
            // Hosted container checkout ownership can differ from root. Trust
            // only this resolved checkout, never global config or a wildcard.
            let mut configured = vec!["-c", safe_directory.as_str()];
            configured.extend_from_slice(args);
            let mut process = crate::process::OwnedProcess::spawn(
                Path::new("/usr/bin/git"),
                &configured,
                &env,
                root,
                &file,
            )?;
            if !process.wait(std::time::Duration::from_secs(30))?.success() {
                return Err("Git release identity/ancestry check failed".into());
            }
            Ok(fs::read_to_string(scratch.join("git.log"))?
                .trim()
                .to_string())
        };
        let source = git(&["rev-parse", &format!("{reference}^{{commit}}")])?;
        let head = git(&["rev-parse", "HEAD"])?;
        if source.len() != 40
            || !source
                .bytes()
                .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
            || head != source
            || environment.get("GITHUB_SHA") != Some(&source)
        {
            return Err("checkout, tag commit and GitHub source identity differ".into());
        }
        git(&[
            "merge-base",
            "--is-ancestor",
            &source,
            "refs/remotes/origin/main",
        ])?;
        Ok((tag.into(), source))
    })();
    // this directory contains only our local, credential-free Git command log.
    fs::remove_dir_all(&scratch)?;
    result
}
fn attribution(root: &Path, relative: &Path, files: &mut BTreeMap<String, Vec<u8>>) -> Result<()> {
    let path = storage::safe_path(root, relative)?;
    if path.is_dir() {
        for entry in fs::read_dir(path)? {
            let entry = entry?;
            attribution(root, &relative.join(entry.file_name()), files)?;
        }
    } else {
        if !path.is_file() {
            return Err("unsafe third-party package input".into());
        }
        let bytes = fs::read(path)?;
        if bytes.is_empty() {
            return Err("empty third-party package input".into());
        }
        files.insert(
            format!(
                "usr/share/doc/among-terminal/THIRD_PARTY/{}",
                relative.to_str().ok_or("attribution path is not UTF-8")?
            ),
            bytes,
        );
    }
    Ok(())
}
pub fn stage(root: &Path, binaries: &Path, destination: &Path) -> Result<usize> {
    let mut files = BTreeMap::new();
    for name in EXECUTABLES {
        let path = storage::safe_path(binaries, Path::new(name))?;
        if !path.is_file() || fs::metadata(&path)?.permissions().mode() & 0o111 == 0 {
            return Err("missing or non-executable package input".into());
        }
        let bytes = fs::read(&path)?;
        if bytes.is_empty() {
            return Err("empty package executable".into());
        }
        files.insert(
            if name == "among" {
                "usr/bin/among".into()
            } else {
                format!("usr/share/among/plugins/{name}")
            },
            bytes,
        );
    }
    for (target, source) in [
        (
            format!("usr/share/applications/{APP_ID}.desktop"),
            format!("data/{APP_ID}.desktop"),
        ),
        (
            format!("usr/share/icons/hicolor/scalable/apps/{APP_ID}.svg"),
            format!("data/{APP_ID}.svg"),
        ),
        (
            format!("usr/share/metainfo/{APP_ID}.metainfo.xml"),
            format!("data/{APP_ID}.metainfo.xml"),
        ),
        (
            "usr/share/doc/among-terminal/LICENSE".into(),
            "LICENSE".into(),
        ),
        (
            "usr/share/doc/among-terminal/NOTICE".into(),
            "NOTICE".into(),
        ),
    ] {
        let bytes = fs::read(storage::safe_path(root, Path::new(&source))?)?;
        if bytes.is_empty() {
            return Err("empty package metadata/license".into());
        }
        files.insert(target, bytes);
    }
    let third_party = storage::safe_path(root, Path::new("THIRD_PARTY"))?;
    for entry in fs::read_dir(&third_party)? {
        attribution(&third_party, Path::new(&entry?.file_name()), &mut files)?;
    }
    if !files.contains_key("usr/share/doc/among-terminal/THIRD_PARTY/inventory.json") {
        return Err("third-party inventory is required".into());
    }
    let desktop = files
        .get_mut(&format!("usr/share/applications/{APP_ID}.desktop"))
        .ok_or("desktop entry missing")?;
    let entry = std::str::from_utf8(desktop)?;
    if entry.lines().filter(|line| *line == "Exec=among").count() != 1 {
        return Err("expected exactly one source desktop Exec=among".into());
    }
    *desktop = entry
        .split_inclusive('\n')
        .map(|line| {
            if line.trim_end_matches(['\r', '\n']) == "Exec=among" {
                format!("Exec=/usr/bin/among{}", &line["Exec=among".len()..])
            } else {
                line.to_string()
            }
        })
        .collect::<String>()
        .into_bytes();
    // all inputs are checked before the destination exists, preserving failure isolation.
    fs::DirBuilder::new().mode(0o755).create(destination)?;
    for (name, bytes) in &files {
        let path = storage::safe_path(destination, Path::new(name))?;
        fs::create_dir_all(path.parent().ok_or("staged file has no parent")?)?;
        storage::atomic_write(
            destination,
            Path::new(name),
            bytes,
            if name == "usr/bin/among" || name.starts_with("usr/share/among/plugins/") {
                0o755
            } else {
                0o644
            },
        )?;
    }
    Ok(files.len())
}

pub fn validate_candidate(
    root: &Path,
    directory: &Path,
    tag: &str,
    source: &str,
    target: &str,
    architecture: &str,
) -> Result<(std::path::PathBuf, serde_json::Value)> {
    if source.len() != 40
        || !source
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
    {
        return Err("invalid candidate source identity".into());
    }
    let record = storage::parse_json(&fs::read(storage::safe_path(
        directory,
        Path::new("build-record.json"),
    )?)?)?;
    let expected = identity(root, tag, target, architecture)?;
    let mut fields = serde_json::to_value(&expected)?
        .as_object()
        .ok_or("invalid expected identity")?
        .clone();
    fields.insert("tag".into(), tag.into());
    fields.insert("source_sha".into(), source.into());
    for (key, value) in fields {
        if record.get(&key) != Some(&value) {
            return Err(format!("candidate {key} does not match this release job").into());
        }
    }
    let path = storage::safe_path(directory, Path::new(&expected.filename))?;
    if !path.is_file() {
        return Err("candidate must be a regular package file".into());
    }
    let digest = storage::sha256(fs::File::open(&path)?)?;
    if record.get("sha256").and_then(serde_json::Value::as_str) != Some(&digest) {
        return Err("candidate package digest changed".into());
    }
    let mut packages = Vec::new();
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        if entry.path().extension() == Some(std::ffi::OsStr::new("deb")) {
            packages.push(entry.file_name());
        }
    }
    if packages != [std::ffi::OsString::from(&expected.filename)] {
        return Err("unexpected package inventory".into());
    }
    Ok((path, record))
}

pub fn runtime_inventory_only(packages: &str, visible_commands: &[&str]) -> Result<()> {
    for line in packages.lines() {
        let (name, status) = line
            .split_once('\t')
            .ok_or("malformed installed package status")?;
        let name = name.split(':').next().ok_or("missing package name")?;
        if status == "installed"
            && (name.ends_with("-dev")
                || matches!(
                    name,
                    "cargo"
                        | "rustc"
                        | "rustup"
                        | "build-essential"
                        | "python3-gi"
                        | "python3-gi-cairo"
                        | "python3-cairo"
                ))
        {
            return Err(format!(
                "build or Python GUI test dependency present in runtime container: {name}"
            )
            .into());
        }
    }
    if visible_commands
        .iter()
        .any(|command| matches!(*command, "cargo" | "rustc" | "rustup"))
    {
        return Err("build tool visible in runtime PATH".into());
    }
    Ok(())
}

pub fn state_hashes(paths: &[std::path::PathBuf]) -> Result<BTreeMap<std::path::PathBuf, String>> {
    paths
        .iter()
        .map(|path| Ok((path.clone(), storage::sha256(fs::File::open(path)?)?)))
        .collect()
}
pub fn require_unchanged(
    paths: &[std::path::PathBuf],
    original: &BTreeMap<std::path::PathBuf, String>,
) -> Result<()> {
    if &state_hashes(paths)? != original {
        return Err("package lifecycle changed user-owned state".into());
    }
    Ok(())
}
