//! Package lifecycle for the RPM candidate: an older package, an upgrade to
//! the exact candidate, remove, reinstall and erase, with seeded user state and
//! a running user process that none of them may touch.
//!
//! The older package is the candidate's own payload repacked under a lower
//! version with one extra older-only file, the same construction the Debian
//! lifecycle uses. Repacking needs `rpmbuild`, which the clean installed worker
//! must not contain, so the fixture is made in the build job by
//! [`make_older_fixture`] and travels to the installed job bound to its digest.

use crate::{deb, process::OwnedProcess, rpm_installed::Runner, storage, Result};
use serde_json::json;
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Appended to the candidate version. In RPM ordering `~` sorts before
/// everything, including the end of the string, so `0.1.0~rc.10~ciolder` is
/// older than `0.1.0~rc.10` and `0.1.0~ciolder` older than `0.1.0`.
pub const OLDER_SUFFIX: &str = "~ciolder";

/// The file only the older package owns. An upgrade that leaves it behind did
/// not replace the older package's file set.
pub const OLDER_MARKER: &str = "/usr/share/among/ci-older-fixture";

/// The fixture lives in its own directory so it can never be mistaken for the
/// single binary RPM of the candidate or attached to a release.
pub const FIXTURE_DIRECTORY: &str = "ci-lifecycle-fixture-not-for-release";

fn query(args: &[&str], package: &Path) -> Result<String> {
    let output = Command::new("/usr/bin/rpm")
        .args(args)
        .arg(package)
        .output()?;
    if !output.status.success() {
        return Err(format!(
            "rpm {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        )
        .into());
    }
    Ok(String::from_utf8(output.stdout)?)
}

/// Text interpolated into the generated spec must not open a macro, a new
/// line or a quoted path.
fn plain(value: &str, what: &str) -> Result<()> {
    if value.is_empty()
        || value
            .chars()
            .any(|c| c.is_control() || matches!(c, '%' | '"' | '\\' | '$' | '`'))
    {
        return Err(format!("unexpected characters in candidate {what}").into());
    }
    Ok(())
}

/// RPM's own version comparison, so the ordering claim is the package
/// manager's and not this crate's.
fn vercmp(left: &str, right: &str) -> Result<i32> {
    for version in [left, right] {
        if !version
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'~' | b'+' | b'_'))
        {
            return Err("version contains characters rpm.vercmp is not given".into());
        }
    }
    let output = Command::new("/usr/bin/rpm")
        .arg("--eval")
        .arg(format!(
            "%{{lua: print(rpm.vercmp(\"{left}\", \"{right}\"))}}"
        ))
        .output()?;
    if !output.status.success() {
        return Err("rpm.vercmp evaluation failed".into());
    }
    Ok(String::from_utf8(output.stdout)?.trim().parse()?)
}

/// Repack `candidate` as `among-<version>~ciolder` with an extra older-only
/// file, write it into `output/ci-lifecycle-fixture-not-for-release/` and
/// return its path. `work` must be an empty directory owned by the caller.
pub fn make_older_fixture(candidate: &Path, work: &Path, output: &Path) -> Result<PathBuf> {
    let candidate = candidate.canonicalize()?;
    let work = work.canonicalize()?;
    if fs::read_dir(&work)?.next().is_some() {
        return Err("fixture work directory must be empty".into());
    }
    let identity = query(
        &[
            "-qp",
            "--qf",
            "%{NAME}|%{VERSION}|%{RELEASE}|%{ARCH}|%{SUMMARY}|%{LICENSE}|%{URL}",
        ],
        &candidate,
    )?;
    let fields = identity.split('|').collect::<Vec<_>>();
    let [name, version, release, arch, summary, license, url] = fields.as_slice() else {
        return Err("unexpected candidate identity".into());
    };
    if *name != "among" {
        return Err("lifecycle fixture is only made from package among".into());
    }
    for (value, what) in [
        (version, "version"),
        (release, "release"),
        (arch, "architecture"),
        (summary, "summary"),
        (license, "license"),
        (url, "URL"),
    ] {
        plain(value, what)?;
    }
    let older = format!("{version}{OLDER_SUFFIX}");
    if vercmp(&older, version)? != -1 {
        return Err("fixture version does not sort before the candidate".into());
    }

    let tree = work.join("payload");
    fs::create_dir(&tree)?;
    let archive = work.join("payload.cpio");
    let status = Command::new("/usr/bin/rpm2cpio")
        .arg(&candidate)
        .stdout(File::create(&archive)?)
        .status()?;
    if !status.success() {
        return Err("rpm2cpio could not read the candidate".into());
    }
    let status = Command::new("/usr/bin/cpio")
        .args(["-idm", "--quiet", "--no-absolute-filenames"])
        .current_dir(&tree)
        .stdin(File::open(&archive)?)
        .stdout(Stdio::null())
        .status()?;
    if !status.success() {
        return Err("cpio could not unpack the candidate payload".into());
    }
    fs::remove_file(&archive)?;

    // Same owned paths as the candidate, directories marked as such, so the
    // upgrade replaces a like-for-like file set plus the one marker.
    let mut files = String::new();
    for line in query(
        &["-qp", "--qf", "[%{FILEMODES:perms} %{FILENAMES}\n]"],
        &candidate,
    )?
    .lines()
    {
        let (perms, path) = line
            .split_once(' ')
            .ok_or("unexpected candidate file line")?;
        plain(path, "file path")?;
        if path.contains(char::is_whitespace) || !path.starts_with('/') {
            return Err("unexpected candidate file path".into());
        }
        if perms.starts_with('d') {
            files.push_str(&format!("%dir \"{path}\"\n"));
        } else {
            files.push_str(&format!("\"{path}\"\n"));
        }
    }
    files.push_str(&format!("\"{OLDER_MARKER}\"\n"));
    let mut requires = String::new();
    for line in query(&["-qp", "--requires"], &candidate)?.lines() {
        if line.starts_with("rpmlib(") {
            continue;
        }
        plain(line, "requirement")?;
        requires.push_str(&format!("Requires: {line}\n"));
    }
    let tree_text = tree.to_str().ok_or("fixture tree path is not UTF-8")?;
    plain(tree_text, "work directory")?;
    let filename = candidate
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or("candidate name is not UTF-8")?;
    let marker_directory = Path::new(OLDER_MARKER)
        .parent()
        .and_then(Path::to_str)
        .ok_or("marker directory absent")?;
    // debuginfo extraction, build-id links and the post-install strip/compress
    // scripts would change the repacked bytes; the payload is taken as built.
    let spec = format!(
        "%global debug_package %{{nil}}\n%global _build_id_links none\n%global __os_install_post %{{nil}}\n\
Name: among\nVersion: {older}\nRelease: {release}\nSummary: {summary}\nLicense: {license}\nURL: {url}\n\
AutoReqProv: no\n{requires}\n%description\nOlder package fixture for upgrade and rollback checks, repacked from {filename}. Not a release asset.\n\n\
%install\ncp -a \"{tree_text}/.\" %{{buildroot}}/\nmkdir -p %{{buildroot}}{marker_directory}\n\
printf 'older package fixture, not a public release\\n' > %{{buildroot}}{OLDER_MARKER}\n\n%files\n{files}"
    );
    let spec_path = work.join("among-older-fixture.spec");
    fs::write(&spec_path, spec)?;
    let topdir = work.join("rpmbuild");
    let log = work.join("rpmbuild.log");
    let status = Command::new("/usr/bin/rpmbuild")
        .arg("--define")
        .arg(format!(
            "_topdir {}",
            topdir.to_str().ok_or("topdir is not UTF-8")?
        ))
        .arg("-bb")
        .arg(&spec_path)
        .stdout(File::create(&log)?)
        .stderr(File::create(work.join("rpmbuild.stderr"))?)
        .status()?;
    if !status.success() {
        return Err(format!(
            "rpmbuild of the lifecycle fixture failed; evidence: {}",
            work.display()
        )
        .into());
    }
    let built = topdir
        .join("RPMS")
        .join(arch)
        .join(format!("among-{older}-{release}.{arch}.rpm"));
    let observed = query(
        &["-qp", "--qf", "%{NAME}|%{VERSION}|%{RELEASE}|%{ARCH}"],
        &built,
    )?;
    if observed != format!("among|{older}|{release}|{arch}") {
        return Err("lifecycle fixture metadata differs from the requested identity".into());
    }
    let directory = output.join(FIXTURE_DIRECTORY);
    fs::create_dir(&directory)?;
    let destination = directory.join(built.file_name().ok_or("fixture name absent")?);
    fs::copy(&built, &destination)?;
    Ok(destination)
}

/// The same synthetic user state the Debian lifecycle seeds: preferences,
/// shortcuts, a session, an external plugin, both shell startup files, and
/// other channels' launchers and desktop entry.
const USER_STATE: [(&str, &str); 9] = [
    ("config/among/plugins.json", "{\"plugins\":[{\"name\":\"external-fixture\",\"enabled\":true}]}\n"),
    ("config/among/shortcuts.json", "{\"version\":1,\"overrides\":{\"split-pane\":\"F11\"}}\n"),
    ("data/among/session.json", "{\"version\":1,\"workspaces\":[]}\n"),
    ("data/among/plugins/external-fixture", "#!/bin/sh\nprintf external-fixture\n"),
    (".bashrc", "# synthetic shell startup, must not be edited\n"),
    (".zshrc", "# synthetic zsh startup, must not be edited\n"),
    (".cargo/bin/among", "#!/bin/sh\nprintf cargo-owned\n"),
    (".local/bin/among", "#!/bin/sh\nprintf source-owned\n"),
    (
        ".local/share/applications/com.github.among-terminal.desktop",
        "[Desktop Entry]\nName=User fixture\nType=Application\nExec=/home/among-check/.local/bin/among\n",
    ),
];

fn installed_version(runner: &mut Runner) -> Result<String> {
    runner.command("/usr/bin/rpm", &["-q", "--qf", "%{VERSION}", "among"], 30)
}

/// The installed header is the candidate's header and every installed file
/// still matches the digests that header records: together, the installed
/// bytes are the candidate's bytes.
fn require_candidate_installed(runner: &mut Runner, candidate: &str, header: &str) -> Result<()> {
    if runner.command(
        "/usr/bin/rpm",
        &["-q", "--qf", "%{SHA256HEADER}", "among"],
        30,
    )? != header
    {
        return Err("installed package header is not the candidate's".into());
    }
    runner.command("/usr/bin/rpm", &["-V", "among"], 60)?;
    if !Path::new(candidate).is_file() {
        return Err("candidate disappeared during lifecycle".into());
    }
    Ok(())
}

fn require_absent(owned: &[PathBuf], operation: &str) -> Result<()> {
    if let Some(left) = owned.iter().find(|path| path.symlink_metadata().is_ok()) {
        return Err(format!("{operation} left package-owned {}", left.display()).into());
    }
    Ok(())
}

/// Run the lifecycle from an installed candidate. Returns the completed
/// operation names in order.
pub(crate) fn run(
    runner: &mut Runner,
    candidate: &Path,
    fixture: &Path,
    version: &str,
    home: &Path,
) -> Result<Vec<String>> {
    if home != Path::new("/home/among-check") || !nix::unistd::getuid().is_root() {
        return Err("lifecycle requires disposable root CI container and synthetic home".into());
    }
    let candidate = candidate.to_str().ok_or("candidate path is not UTF-8")?;
    let fixture = fixture.to_str().ok_or("fixture path is not UTF-8")?;
    let older = format!("{version}{OLDER_SUFFIX}");
    if vercmp(&older, version)? != -1 {
        return Err("fixture version does not sort before the candidate".into());
    }
    let header = runner.command(
        "/usr/bin/rpm",
        &["-qp", "--qf", "%{SHA256HEADER}", candidate],
        30,
    )?;
    // An absent tag prints "(none)" for both sides and would compare equal.
    if header.len() != 64 || !header.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("candidate header carries no SHA-256 digest".into());
    }
    let observed = runner.command(
        "/usr/bin/rpm",
        &["-qp", "--qf", "%{NAME}|%{VERSION}", fixture],
        30,
    )?;
    if observed != format!("among|{older}") {
        return Err("lifecycle fixture is not the older among package".into());
    }

    let mut paths = Vec::new();
    for (relative, text) in USER_STATE {
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
    // Stands in for a terminal the user keeps open: no package operation may
    // signal processes of other users.
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
        &runner.environment,
        &runner.base,
        &log,
    )?;

    let mut operations = Vec::new();
    let mut operation = |runner: &mut Runner,
                         label: &str,
                         program: &str,
                         args: &[&str]|
     -> Result<()> {
        runner.command(program, args, 300)?;
        deb::require_unchanged(&paths, &original)?;
        if !survivor.is_running()? {
            return Err(format!("synthetic user shell killed during {label}").into());
        }
        operations.push(label.to_string());
        storage::atomic_write(
            &runner.base,
            Path::new("lifecycle-progress.json"),
            &serde_json::to_vec_pretty(
                &json!({"schema":1,"operations":operations,"scope":"completed package operations with user-state and survivor assertions only","release_acceptance":false}),
            )?,
            0o644,
        )?;
        Ok(())
    };

    // Rolling back to an older local package is the documented downgrade.
    operation(
        runner,
        "older-fixture-installed",
        "/usr/bin/dnf",
        &["downgrade", "-y", fixture],
    )?;
    if installed_version(runner)? != older || !Path::new(OLDER_MARKER).is_file() {
        return Err("older fixture is not the installed version or lacks its own file".into());
    }
    operation(
        runner,
        "candidate-upgrade",
        "/usr/bin/dnf",
        &["upgrade", "-y", candidate],
    )?;
    if Path::new(OLDER_MARKER).symlink_metadata().is_ok() {
        return Err("upgrade retained older-only package file".into());
    }
    if installed_version(runner)? != version {
        return Err("upgrade did not install the exact candidate version".into());
    }
    require_candidate_installed(runner, candidate, &header)?;
    let owned = runner
        .command("/usr/bin/rpm", &["-ql", "among"], 30)?
        .lines()
        .map(PathBuf::from)
        .collect::<Vec<_>>();
    if owned.is_empty() || !owned.iter().all(|path| path.symlink_metadata().is_ok()) {
        return Err("installed candidate does not own its listed paths".into());
    }
    operation(runner, "remove", "/usr/bin/dnf", &["remove", "-y", "among"])?;
    require_absent(&owned, "remove")?;
    operation(
        runner,
        "reinstall-for-erase",
        "/usr/bin/dnf",
        &["install", "-y", candidate],
    )?;
    require_candidate_installed(runner, candidate, &header)?;
    // `rpm -e` is the erase that bypasses dnf's dependency solver; it must
    // remove the same files and still leave user state alone.
    operation(runner, "erase", "/usr/bin/rpm", &["-e", "among"])?;
    require_absent(&owned, "erase")?;
    let (still, _) = runner.execute("/usr/bin/rpm", &["-q", "among"], 30)?;
    if still {
        return Err("erase left package among registered".into());
    }
    Ok(operations)
}
