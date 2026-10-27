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
    published_version: &str,
    documented: &DocumentedCommands,
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
    let mut life = Lifecycle {
        paths: &paths,
        original: &original,
        survivor: &mut survivor,
        operations: Vec::new(),
    };
    life.apt_get(
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
    life.apt_get(runner, "candidate-upgrade", &["install", "-y", package])?;
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
    life.apt_get(runner, "remove", &["remove", "-y", "among-terminal"])?;
    if owned.iter().any(|path| path.exists() || path.is_symlink()) {
        return Err("remove left package-owned files".into());
    }
    // No conffiles are shipped: after remove, apt may forget this local-only
    // package entirely. Purging that absent name tests lookup, not removal.
    // Reinstall exact candidate bytes to test a real purge independently.
    life.apt_get(runner, "reinstall-for-purge", &["install", "-y", package])?;
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
    life.apt_get(runner, "purge", &["purge", "-y", "among-terminal"])?;
    if owned.iter().any(|path| path.exists() || path.is_symlink()) {
        return Err("purge left package-owned files".into());
    }
    // The upgrade users actually perform: the published release, installed
    // on a system without Among, replaced by the candidate.
    let published = published
        .to_str()
        .ok_or("published package path is not UTF-8")?;
    life.apt_get(
        runner,
        "published-release-installed",
        &["install", "-y", published],
    )?;
    let published_owned = owned_files(runner)?;
    life.apt_get(
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
    life.apt_get(
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
    life.replay_documented(
        runner,
        documented,
        published,
        package,
        current,
        published_version,
    )?;
    Ok(life.operations)
}

/// Package operations that keep user state and a user's running process
/// intact; every step re-checks both and records its progress.
struct Lifecycle<'a> {
    paths: &'a [PathBuf],
    original: &'a BTreeMap<PathBuf, String>,
    survivor: &'a mut OwnedProcess,
    operations: Vec<String>,
}

impl Lifecycle<'_> {
    fn apt_get(&mut self, runner: &mut Runner, label: &str, args: &[&str]) -> Result<()> {
        let base = runner.base.clone();
        self.step(runner, &base, "/usr/bin/apt-get", label, args)
    }

    fn step(
        &mut self,
        runner: &mut Runner,
        directory: &Path,
        program: &str,
        label: &str,
        args: &[&str],
    ) -> Result<()> {
        runner.command_in(directory, program, args, 180)?;
        deb::require_unchanged(self.paths, self.original)?;
        if !self.survivor.is_running()? {
            return Err(format!("synthetic user shell killed during {label}").into());
        }
        self.operations.push(label.to_string());
        storage::atomic_write(
            &runner.base,
            Path::new("lifecycle-progress.json"),
            &serde_json::to_vec_pretty(
                &serde_json::json!({"schema":1,"operations":self.operations,"scope":"completed package operations with user-state and survivor assertions only","release_acceptance":false}),
            )?,
            0o644,
        )?;
        Ok(())
    }

    /// Runs the user commands of the Debian guide word for word, in the order
    /// a user meets them: install a release, upgrade to a newer one, roll back
    /// with the documented downgrade, upgrade again, remove and purge. Each
    /// command runs in a directory that holds this target's package under its
    /// release name, as a user who downloaded it has it.
    fn replay_documented(
        &mut self,
        runner: &mut Runner,
        documented: &DocumentedCommands,
        published: &str,
        candidate: &str,
        candidate_version: &str,
        published_version: &str,
    ) -> Result<()> {
        let documented_file = documented.file()?;
        let release_names = crate::release_assets::DEB_TARGETS
            .iter()
            .flat_map(|target| {
                crate::release_assets::ARCHITECTURES
                    .iter()
                    .map(move |architecture| {
                        crate::release_assets::package_asset(target, architecture, "deb")
                    })
            })
            .collect::<Vec<_>>();
        if !release_names.iter().any(|name| name == documented_file) {
            return Err("documented package file is not a release file name".into());
        }
        // The guide's example names one target; a user on this target has
        // this target's file, so the replay uses that name.
        let name = Path::new(published)
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or("published package has no release name")?;
        let local = format!("./{name}");
        let mut directories = BTreeMap::new();
        for (role, source) in [("published", published), ("candidate", candidate)] {
            let directory = runner.base.join("documented").join(role);
            fs::create_dir_all(&directory)?;
            fs::copy(source, directory.join(name))?;
            directories.insert(role, directory);
        }
        let with_file = |words: &[String]| {
            words
                .iter()
                .map(|word| {
                    if word.starts_with("./") {
                        local.clone()
                    } else {
                        word.clone()
                    }
                })
                .collect::<Vec<_>>()
        };
        let steps = [
            (
                "documented-install",
                "published",
                with_file(&documented.install),
                Some(published_version),
            ),
            (
                "documented-upgrade",
                "candidate",
                with_file(&documented.install),
                Some(candidate_version),
            ),
            (
                "documented-rollback",
                "published",
                with_file(&documented.downgrade),
                Some(published_version),
            ),
            (
                "documented-upgrade-again",
                "candidate",
                with_file(&documented.install),
                Some(candidate_version),
            ),
            (
                "documented-remove",
                "candidate",
                documented.remove.clone(),
                None,
            ),
            (
                "documented-purge",
                "candidate",
                documented.purge.clone(),
                None,
            ),
        ];
        // apt asks for confirmation, and nobody types into the replay. The
        // configuration answers yes, as the user does at the prompt, without
        // changing the documented words.
        let answer = runner.base.join("documented").join("assume-yes.conf");
        fs::write(&answer, "APT::Get::Assume-Yes \"true\";\n")?;
        let key = std::ffi::OsString::from("APT_CONFIG");
        runner.env.insert(key.clone(), answer.into_os_string());
        let result = (|| -> Result<()> {
            for (label, role, words, version) in steps {
                self.documented_step(runner, &directories[role], label, &words, version)?;
            }
            Ok(())
        })();
        runner.env.remove(&key);
        result
    }

    fn documented_step(
        &mut self,
        runner: &mut Runner,
        directory: &Path,
        label: &str,
        words: &[String],
        version: Option<&str>,
    ) -> Result<()> {
        let (program, args) = words.split_first().ok_or("documented command is empty")?;
        if program != "apt" {
            return Err("documented command is not apt".into());
        }
        let args = args.iter().map(String::as_str).collect::<Vec<_>>();
        self.step(runner, directory, "/usr/bin/apt", label, &args)?;
        let (installed, status) = runner.execute(
            "/usr/bin/dpkg-query",
            &["-W", "-f=${Status}\t${Version}", "among-terminal"],
            30,
        )?;
        match version {
            Some(version) if !installed || status != format!("install ok installed\t{version}") => {
                return Err(format!("{label} did not leave version {version} installed").into());
            }
            None if Path::new("/usr/bin/among").symlink_metadata().is_ok() => {
                return Err(format!("{label} left /usr/bin/among installed").into());
            }
            _ => {}
        }
        Ok(())
    }
}

/// The user commands of the "Installing a built package" section of
/// `docs/debian-packaging.md`, before "Coexisting with other copies", as
/// argument vectors without `sudo`. Installed acceptance runs exactly these, so
/// the documented instructions cannot drift from what was tested. Any command
/// other than the four known forms is refused rather than executed.
pub fn documented_commands(guide: &str) -> Result<DocumentedCommands> {
    let section = guide
        .split_once("\n## Installing a built package\n")
        .and_then(|(_, rest)| rest.split_once("\n### Coexisting with other copies\n"))
        .map(|(section, _)| section)
        .ok_or("Debian guide lacks its installation section")?;
    let (mut install, mut downgrade, mut remove, mut purge) = (None, None, None, None);
    let mut block = false;
    for line in section.lines() {
        match line.trim_end() {
            "```sh" if !block => block = true,
            "```" if block => block = false,
            command if block => {
                let command = command.split('#').next().unwrap_or("").trim();
                if command.is_empty() {
                    continue;
                }
                let words = command
                    .strip_prefix("sudo ")
                    .ok_or(format!(
                        "documented command is not run with sudo: {command}"
                    ))?
                    .split_whitespace()
                    .map(str::to_string)
                    .collect::<Vec<_>>();
                let slot = match words.iter().map(String::as_str).collect::<Vec<_>>()[..] {
                    ["apt", "install", file] if file.starts_with("./") => &mut install,
                    ["apt", "install", "--allow-downgrades", file] if file.starts_with("./") => {
                        &mut downgrade
                    }
                    ["apt", "remove", "among-terminal"] => &mut remove,
                    ["apt", "purge", "among-terminal"] => &mut purge,
                    _ => return Err(format!("unknown documented command: {command}").into()),
                };
                if slot.as_ref().is_some_and(|known| known != &words) {
                    return Err(format!("documented command varies: {command}").into());
                }
                *slot = Some(words);
            }
            _ => {}
        }
    }
    let missing = |name: &str| format!("Debian guide lacks the {name} command");
    Ok(DocumentedCommands {
        install: install.ok_or_else(|| missing("install"))?,
        downgrade: downgrade.ok_or_else(|| missing("rollback"))?,
        remove: remove.ok_or_else(|| missing("remove"))?,
        purge: purge.ok_or_else(|| missing("purge"))?,
    })
}

#[derive(Debug, PartialEq, Eq)]
pub struct DocumentedCommands {
    pub install: Vec<String>,
    pub downgrade: Vec<String>,
    pub remove: Vec<String>,
    pub purge: Vec<String>,
}

impl DocumentedCommands {
    /// The file the documented install commands name, which the replay
    /// provides under that exact name in each package's directory.
    pub fn file(&self) -> Result<&str> {
        let file = self.install.last().ok_or("install command is empty")?;
        if self.downgrade.last() != Some(file) {
            return Err("documented install and rollback name different files".into());
        }
        file.strip_prefix("./")
            .ok_or_else(|| "documented package is not a relative file".into())
    }
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
