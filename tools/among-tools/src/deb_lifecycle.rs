use crate::{deb, deb_installed::Runner, deb_previous, process::OwnedProcess, storage, Result};
use serde_json::Value;
use std::collections::BTreeMap;
use std::fs::{self, File};
use std::path::{Path, PathBuf};

fn payload(root: &Path, relative: &Path, files: &mut BTreeMap<PathBuf, String>) -> Result<()> {
    for entry in fs::read_dir(root.join(relative))? {
        let entry = entry?;
        if relative.as_os_str().is_empty() && entry.file_name() == "DEBIAN" {
            continue;
        }
        let relative = relative.join(entry.file_name());
        let path = storage::safe_path(root, &relative)?;
        if path.is_dir() {
            payload(root, &relative, files)?;
        } else if path.is_file() {
            files.insert(relative, storage::sha256(File::open(path)?)?);
        } else {
            return Err("unexpected lifecycle payload type".into());
        }
    }
    Ok(())
}

pub(crate) fn run(
    runner: &mut Runner,
    package: &Path,
    record: &Value,
    home: &Path,
    published: &Path,
) -> Result<Vec<String>> {
    if home != Path::new("/home/among-check") || !nix::unistd::getuid().is_root() {
        return Err("lifecycle requires disposable root CI container and synthetic home".into());
    }
    let state=[
        ("config/among/plugins.json","{\"plugins\":[{\"name\":\"external-fixture\",\"enabled\":true}]}\n"),
        ("config/among/shortcuts.json","{\"version\":1,\"overrides\":{\"split-pane\":\"F11\"}}\n"),
        ("data/among/session.json","{\"version\":1,\"workspaces\":[]}\n"),
        ("data/among/plugins/external-fixture","#!/bin/sh\nprintf external-fixture\n"),
        (".bashrc","# synthetic shell startup, must not be edited\n"),(".zshrc","# synthetic zsh startup, must not be edited\n"),
        (".cargo/bin/among","#!/bin/sh\nprintf cargo-owned\n"),(".local/bin/among","#!/bin/sh\nprintf source-owned\n"),
        (".local/share/applications/com.github.among-terminal.desktop","[Desktop Entry]\nName=User fixture\nType=Application\nExec=/home/among-check/.local/bin/among\n"),
    ];
    let mut paths = Vec::new();
    for (relative, text) in state {
        let path = storage::safe_path(home, Path::new(relative))?;
        fs::create_dir_all(path.parent().ok_or("state parent absent")?)?;
        storage::atomic_write(home, Path::new(relative), text.as_bytes(), 0o644)?;
        paths.push(path);
    }
    runner.command(
        "/usr/bin/chown",
        &["-R", "among-check:among-check", "/home/among-check"],
        30,
    )?;
    let original = deb::state_hashes(&paths)?;
    storage::atomic_write(
        &runner.base,
        Path::new("user-state-sha256.json"),
        &serde_json::to_vec_pretty(&original)?,
        0o644,
    )?;
    let log = File::create(runner.base.join("survivor.log"))?;
    let mut survivor = OwnedProcess::spawn(
        Path::new("/usr/sbin/runuser"),
        &[
            "-u",
            "among-check",
            "--",
            "/bin/bash",
            "--noprofile",
            "--norc",
            "-c",
            "exec sleep 600",
        ],
        &runner.env,
        &runner.base,
        &log,
    )?;
    let tree = runner.base.join("older-package");
    let package = package.to_str().ok_or("candidate path is not UTF-8")?;
    runner.command(
        "/usr/bin/dpkg-deb",
        &[
            "--raw-extract",
            package,
            tree.to_str().ok_or("older tree path is not UTF-8")?,
        ],
        30,
    )?;
    let mut candidate = BTreeMap::new();
    payload(&tree, Path::new(""), &mut candidate)?;
    let current = record["package_version"]
        .as_str()
        .ok_or("candidate version absent")?;
    let older = format!("{current}~ciolder");
    runner.command(
        "/usr/bin/dpkg",
        &["--compare-versions", &older, "lt", current],
        30,
    )?;
    let control = fs::read_to_string(tree.join("DEBIAN/control"))?;
    let needle = format!("Version: {current}\n");
    if control.matches(&needle).count() != 1 {
        return Err("unexpected candidate version metadata".into());
    }
    fs::write(
        tree.join("DEBIAN/control"),
        control.replace(&needle, &format!("Version: {older}\n")),
    )?;
    let marker = Path::new("usr/share/doc/among-terminal/ci-older-fixture");
    fs::write(
        tree.join(marker),
        "older package fixture, not a public release\n",
    )?;
    let mut older_files = BTreeMap::new();
    payload(&tree, Path::new(""), &mut older_files)?;
    let mut sums = String::new();
    for name in older_files.keys() {
        let text = runner.command(
            "/usr/bin/md5sum",
            &[tree
                .join(name)
                .to_str()
                .ok_or("fixture member path is not UTF-8")?],
            10,
        )?;
        let digest = text
            .split_whitespace()
            .next()
            .ok_or("md5sum output absent")?;
        if digest.len() != 32 || !digest.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err("invalid md5sum output".into());
        }
        sums.push_str(&format!("{digest}  {}\n", name.display()));
    }
    fs::write(tree.join("DEBIAN/md5sums"), sums)?;
    let fixture = runner.base.join("older.deb");
    runner.command(
        "/usr/bin/dpkg-deb",
        &[
            "--build",
            tree.to_str().ok_or("fixture tree path is not UTF-8")?,
            fixture
                .to_str()
                .ok_or("fixture package path is not UTF-8")?,
        ],
        30,
    )?;
    let mut operations = Vec::new();
    let mut operation = |runner: &mut Runner, label: &str, args: &[&str]| -> Result<()> {
        runner.command("/usr/bin/apt-get", args, 180)?;
        deb::require_unchanged(&paths, &original)?;
        if !survivor.is_running()? {
            return Err(format!("synthetic user shell killed during {label}").into());
        }
        operations.push(label.to_string());
        storage::atomic_write(
            &runner.base,
            Path::new("lifecycle-progress.json"),
            &serde_json::to_vec_pretty(
                &serde_json::json!({"schema":1,"operations":operations,"scope":"completed package operations with user-state and survivor assertions only","release_acceptance":false}),
            )?,
            0o644,
        )?;
        Ok(())
    };
    operation(
        runner,
        "older-fixture-installed",
        &[
            "install",
            "-y",
            "--allow-downgrades",
            fixture.to_str().ok_or("fixture path is not UTF-8")?,
        ],
    )?;
    if !Path::new("/").join(marker).is_file() {
        return Err("older-only payload not installed".into());
    }
    operation(runner, "candidate-upgrade", &["install", "-y", package])?;
    if Path::new("/").join(marker).exists() {
        return Err("upgrade retained older-only package file".into());
    }
    if runner.command(
        "/usr/bin/dpkg-query",
        &["-W", "-f=${Status}\t${Version}", "among-terminal"],
        30,
    )? != format!("install ok installed\t{current}")
    {
        return Err("upgrade did not configure exact candidate version".into());
    }
    for (name, digest) in &candidate {
        if storage::sha256(File::open(Path::new("/").join(name))?)? != *digest {
            return Err("upgraded payload differs from candidate bytes".into());
        }
    }
    let owned = runner
        .command("/usr/bin/dpkg-query", &["-L", "among-terminal"], 30)?
        .lines()
        .map(PathBuf::from)
        .filter(|path| path.is_file() || path.is_symlink())
        .collect::<Vec<_>>();
    operation(runner, "remove", &["remove", "-y", "among-terminal"])?;
    if owned.iter().any(|path| path.exists() || path.is_symlink()) {
        return Err("remove left package-owned files".into());
    }
    // No conffiles are shipped: after remove, apt may forget this local-only
    // package entirely. Purging that absent name tests lookup, not removal.
    // Reinstall exact candidate bytes to test a real purge independently.
    operation(runner, "reinstall-for-purge", &["install", "-y", package])?;
    if runner.command(
        "/usr/bin/dpkg-query",
        &["-W", "-f=${Status}\t${Version}", "among-terminal"],
        30,
    )? != format!("install ok installed\t{current}")
    {
        return Err("purge fixture did not reinstall exact candidate version".into());
    }
    for (name, digest) in &candidate {
        if storage::sha256(File::open(Path::new("/").join(name))?)? != *digest {
            return Err("purge fixture payload differs from checked candidate".into());
        }
    }
    operation(runner, "purge", &["purge", "-y", "among-terminal"])?;
    if owned.iter().any(|path| path.exists() || path.is_symlink()) {
        return Err("purge left package-owned files".into());
    }
    // The upgrade users actually perform: the published release, installed
    // on a system without Among, replaced by the candidate.
    let published = published
        .to_str()
        .ok_or("published package path is not UTF-8")?;
    operation(
        runner,
        "published-release-installed",
        &["install", "-y", published],
    )?;
    let published_owned = owned_files(runner)?;
    operation(
        runner,
        "published-release-upgrade",
        &["install", "-y", package],
    )?;
    if runner.command(
        "/usr/bin/dpkg-query",
        &["-W", "-f=${Status}\t${Version}", "among-terminal"],
        30,
    )? != format!("install ok installed\t{current}")
    {
        return Err("upgrade from the published release did not configure the candidate".into());
    }
    for (name, digest) in &candidate {
        if storage::sha256(File::open(Path::new("/").join(name))?)? != *digest {
            return Err("payload after the published upgrade differs from candidate bytes".into());
        }
    }
    let kept = Path::new("/");
    if let Some(stale) = published_owned.iter().find(|path| {
        path.strip_prefix(kept)
            .map_or(true, |relative| !candidate.contains_key(relative))
            && (path.exists() || path.is_symlink())
    }) {
        return Err(format!(
            "upgrade kept {} that only the published release owned",
            stale.display()
        )
        .into());
    }
    let upgraded = owned_files(runner)?;
    operation(
        runner,
        "purge-after-published-upgrade",
        &["purge", "-y", "among-terminal"],
    )?;
    if upgraded
        .iter()
        .any(|path| path.exists() || path.is_symlink())
    {
        return Err("purge after the published upgrade left package-owned files".into());
    }
    Ok(operations)
}

fn owned_files(runner: &mut Runner) -> Result<Vec<PathBuf>> {
    Ok(runner
        .command("/usr/bin/dpkg-query", &["-L", "among-terminal"], 30)?
        .lines()
        .map(PathBuf::from)
        .filter(|path| path.is_file() || path.is_symlink())
        .collect())
}

/// Downloads the pinned published package and refuses it unless its bytes,
/// name, version and architecture are exactly the pinned ones and it sorts
/// before the candidate, so the later install really is an upgrade.
pub(crate) fn fetch_published(
    runner: &mut Runner,
    previous: &deb_previous::Package,
    architecture: &str,
    candidate_version: &str,
) -> Result<PathBuf> {
    let directory = runner.base.join("published-release");
    fs::create_dir(&directory)?;
    let path = directory.join(&previous.asset);
    let target = path.to_str().ok_or("published package path is not UTF-8")?;
    runner.command(
        "/usr/bin/curl",
        &[
            "--fail",
            "--silent",
            "--show-error",
            "--location",
            "--proto",
            "=https",
            "--proto-redir",
            "=https",
            "--max-filesize",
            "268435456",
            "--output",
            target,
            &previous.url(),
        ],
        300,
    )?;
    if storage::sha256(File::open(&path)?)? != previous.sha256 {
        return Err(format!("{} differs from the pinned published bytes", previous.asset).into());
    }
    for (field, expected) in [
        ("Package", "among-terminal"),
        ("Version", previous.version.as_str()),
        ("Architecture", architecture),
    ] {
        if runner
            .command("/usr/bin/dpkg-deb", &["-f", target, field], 30)?
            .trim()
            != expected
        {
            return Err(format!("published package {field} differs from the pin").into());
        }
    }
    runner.command(
        "/usr/bin/dpkg",
        &[
            "--compare-versions",
            &previous.version,
            "lt",
            candidate_version,
        ],
        30,
    )?;
    Ok(path)
}
