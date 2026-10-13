use crate::{deb, platform, process::OwnedProcess, storage, Result};
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    ffi::OsString,
    fs::{self, File},
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    time::Duration,
};

fn target(target: &str, architecture: &str) -> Result<()> {
    let version = match target {
        "fedora43" => "43",
        "fedora44" => "44",
        _ => return Err("unsupported RPM target".into()),
    };
    let observed = platform::discover()?;
    if observed.os != "fedora"
        || observed.version != version
        || observed.architecture != architecture
    {
        return Err("RPM worker OS/architecture differs from target".into());
    }
    Ok(())
}

pub(crate) struct Runner {
    pub(crate) base: PathBuf,
    pub(crate) environment: BTreeMap<OsString, OsString>,
    counter: usize,
}
impl Runner {
    /// Run a command with its combined output in a numbered evidence log and
    /// report success alongside the trimmed output, for checks that expect a
    /// failure.
    pub(crate) fn execute(
        &mut self,
        program: &str,
        args: &[&str],
        seconds: u64,
    ) -> Result<(bool, String)> {
        self.counter += 1;
        let log = self.base.join(format!("command-{}.log", self.counter));
        let mut process = OwnedProcess::spawn(
            Path::new(program),
            args,
            &self.environment,
            &self.base,
            &File::create(&log)?,
        )?;
        let success = process.wait(Duration::from_secs(seconds))?.success();
        Ok((success, fs::read_to_string(log)?.trim().into()))
    }
    pub(crate) fn command(&mut self, program: &str, args: &[&str], seconds: u64) -> Result<String> {
        let (success, text) = self.execute(program, args, seconds)?;
        if !success {
            return Err(format!(
                "RPM check failed; evidence: {}",
                self.base
                    .join(format!("command-{}.log", self.counter))
                    .display()
            )
            .into());
        }
        Ok(text)
    }
    fn runtime_only(&mut self) -> Result<()> {
        let packages = self.command("/usr/bin/rpm", &["-qa", "--qf", "%{NAME}\n"], 30)?;
        if packages.lines().any(|name| {
            name.ends_with("-devel")
                || matches!(name, "cargo" | "rust" | "gcc" | "gcc-c++" | "rpm-build")
        }) {
            return Err("runtime worker contains compilers/development packages".into());
        }
        Ok(())
    }
}

fn runner(base: &Path) -> Runner {
    Runner {
        base: base.into(),
        environment: BTreeMap::from([
            ("PATH".into(), "/usr/sbin:/usr/bin:/sbin:/bin".into()),
            ("LC_ALL".into(), "C.UTF-8".into()),
            ("HOME".into(), "/root".into()),
            ("GITHUB_ACTIONS".into(), "true".into()),
            ("RUNNER_ENVIRONMENT".into(), "github-hosted".into()),
        ]),
        counter: 0,
    }
}

/// The `dist.arch` tail of every RPM file name built for a supported target.
fn filename_suffix(target: &str, architecture: &str) -> Result<&'static str> {
    Ok(match (target, architecture) {
        ("fedora43", "amd64") => "fc43.x86_64",
        ("fedora43", "arm64") => "fc43.aarch64",
        ("fedora44", "amd64") => "fc44.x86_64",
        ("fedora44", "arm64") => "fc44.aarch64",
        _ => return Err("unsupported RPM identity".into()),
    })
}

pub fn validate_record(
    record: &Value,
    tag: &str,
    source: &str,
    target: &str,
    architecture: &str,
    package: &Path,
) -> Result<()> {
    let (_, version) = deb::release_version(tag)?;
    let suffix = filename_suffix(target, architecture)?;
    let filename = format!("among-{version}-1.{suffix}.rpm");
    if record["schema"] != 1
        || record["tag"] != tag
        || record["source_sha"] != source
        || record["target"] != target
        || record["architecture"] != architecture
        || record["filename"] != filename
        || package.file_name().and_then(|name| name.to_str()) != Some(&filename)
        || !package.symlink_metadata()?.is_file()
        || record["sha256"] != storage::sha256(File::open(package)?)?
    {
        return Err("RPM candidate source/target/bytes differ from receipt".into());
    }
    Ok(())
}

/// The lifecycle fixture named and hashed in the receipt, at its fixed place
/// beside the candidate. Its bytes are bound like the candidate's, so the
/// installed job cannot be handed a different "older" package.
pub fn validate_fixture(
    record: &Value,
    tag: &str,
    target: &str,
    architecture: &str,
    directory: &Path,
) -> Result<PathBuf> {
    let (_, version) = deb::release_version(tag)?;
    let suffix = filename_suffix(target, architecture)?;
    let filename = format!(
        "among-{version}{}-1.{suffix}.rpm",
        crate::rpm_lifecycle::OLDER_SUFFIX
    );
    let fixture = directory
        .join(crate::rpm_lifecycle::FIXTURE_DIRECTORY)
        .join(&filename);
    if record["older_fixture"]["filename"] != filename
        || !fixture.symlink_metadata()?.is_file()
        || record["older_fixture"]["sha256"] != storage::sha256(File::open(&fixture)?)?
    {
        return Err("RPM lifecycle fixture name/bytes differ from receipt".into());
    }
    Ok(fixture)
}

fn rpm_files(directory: &Path) -> Result<Vec<PathBuf>> {
    Ok(fs::read_dir(directory)?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<std::io::Result<Vec<_>>>()?
        .into_iter()
        .filter(|path| path.extension().is_some_and(|extension| extension == "rpm"))
        .collect())
}

fn observed_identity(package: &Path) -> Result<String> {
    let observed = std::process::Command::new("/usr/bin/rpm")
        .args(["-qp", "--qf", "%{NAME}|%{VERSION}|%{RELEASE}|%{ARCH}"])
        .arg(package)
        .output()?;
    if !observed.status.success() {
        return Err("rpm could not read package metadata".into());
    }
    Ok(String::from_utf8(observed.stdout)?)
}

pub fn record(root: &Path, target_name: &str, architecture: &str, directory: &Path) -> Result<()> {
    let (tag, source) = deb::github_tag_source(root, &std::env::vars().collect())?;
    target(target_name, architecture)?;
    let packages = rpm_files(directory)?;
    if packages.len() != 1 {
        return Err("expected exactly one binary RPM".into());
    }
    let package = &packages[0];
    let (_, version) = deb::release_version(&tag)?;
    let dist = target_name
        .strip_prefix("fedora")
        .ok_or("Fedora target absent")?;
    let machine = match architecture {
        "amd64" => "x86_64",
        "arm64" => "aarch64",
        _ => return Err("RPM architecture unsupported".into()),
    };
    if observed_identity(package)? != format!("among|{version}|1.fc{dist}|{machine}") {
        return Err("actual RPM metadata differs from release tag/target".into());
    }
    let fixtures = rpm_files(&directory.join(crate::rpm_lifecycle::FIXTURE_DIRECTORY))?;
    let [fixture] = fixtures.as_slice() else {
        return Err("expected exactly one lifecycle fixture RPM".into());
    };
    if observed_identity(fixture)?
        != format!(
            "among|{version}{}|1.fc{dist}|{machine}",
            crate::rpm_lifecycle::OLDER_SUFFIX
        )
    {
        return Err("lifecycle fixture metadata differs from the candidate identity".into());
    }
    let document = json!({"schema":1,"tag":tag,"source_sha":source,"target":target_name,"architecture":architecture,
        "filename":package.file_name().and_then(|name|name.to_str()).ok_or("non UTF-8 RPM")?,
        "sha256":storage::sha256(File::open(package)?)?,"checker_sha256":storage::sha256(File::open(directory.join("among-tools"))?)?,
        "older_fixture":{"filename":fixture.file_name().and_then(|name|name.to_str()).ok_or("non UTF-8 fixture")?,
            "sha256":storage::sha256(File::open(fixture)?)?,"release_asset":false}});
    validate_record(&document, &tag, &source, target_name, architecture, package)?;
    validate_fixture(&document, &tag, target_name, architecture, directory)?;
    storage::atomic_write(
        directory,
        Path::new("build-record.json"),
        &serde_json::to_vec_pretty(&document)?,
        0o644,
    )
}

pub fn run(
    root: &Path,
    target_name: &str,
    architecture: &str,
    directory: &Path,
    output: &Path,
) -> Result<()> {
    let (tag, source) = deb::github_tag_source(root, &std::env::vars().collect())?;
    if std::env::var("RUNNER_ENVIRONMENT").as_deref() != Ok("github-hosted")
        || !nix::unistd::getuid().is_root()
    {
        return Err("RPM install checks require a disposable hosted root container".into());
    }
    target(target_name, architecture)?;
    let root = root.canonicalize()?;
    let directory = directory.canonicalize()?;
    let record = storage::parse_json(&fs::read(directory.join("build-record.json"))?)?;
    let filename = record["filename"].as_str().ok_or("RPM filename absent")?;
    storage::relative_path(Path::new(filename))?;
    let package = directory.join(filename);
    validate_record(&record, &tag, &source, target_name, architecture, &package)?;
    let fixture = validate_fixture(&record, &tag, target_name, architecture, &directory)?;
    let checker = directory.join("among-tools");
    if checker.canonicalize()? != std::env::current_exe()?.canonicalize()?
        || record["checker_sha256"] != storage::sha256(File::open(&checker)?)?
    {
        return Err("RPM install driver is not the exact transferred checker".into());
    }
    fs::create_dir(output)?;
    let output = output.canonicalize()?;
    let mut runner = runner(&output);
    // Resolved once here as root; the unprivileged check user receives it
    // explicitly, because its own HOME would yield a different default.
    let checks = crate::scratch::root()?;
    let checks_text = checks.to_str().ok_or("check root is not UTF-8")?;
    let check_root = format!("{}={checks_text}", crate::scratch::VARIABLE);
    let mut evidence = json!({"schema":1,"tag":tag,"source_sha":source,"target":target_name,"architecture":architecture,
        "package_sha256":record["sha256"],"checker_sha256":record["checker_sha256"],"older_fixture":record["older_fixture"],
        "display":"X11/Xvfb/Openbox","status":"in-progress","checks":[],
        "release_acceptance":false,"scope":"native clean RPM installation; X11 desktop, focus, keyboard divider resize, plugin status with the packaged plugins as external plugins and consented Sessions; downgrade to a repacked older fixture, upgrade, remove, reinstall and erase with seeded user state and a running user process untouched; no installed preferences editing, historical-release upgrade, application-state migration or GNOME/Wayland certification"});
    let result = (|| -> Result<()> {
        runner.runtime_only()?;
        let package = package.to_str().ok_or("RPM path is not UTF-8")?;
        runner.command("/usr/bin/dnf", &["install", "-y", package], 300)?;
        runner.runtime_only()?;
        let (_, version) = deb::release_version(&tag)?;
        let installed =
            runner.command("/usr/bin/rpm", &["-q", "--qf", "%{VERSION}", "among"], 30)?;
        if installed != version {
            return Err("installed RPM version differs from candidate".into());
        }
        if runner
            .command("/usr/bin/ldd", &["/usr/bin/among"], 30)?
            .contains("not found")
        {
            return Err("unresolved RPM runtime library".into());
        }
        evidence["checks"]
            .as_array_mut()
            .ok_or("RPM checks array absent")?
            .push(json!("dnf-configured-and-linked"));
        runner.command(
            "/usr/sbin/useradd",
            &["--create-home", "--shell", "/bin/bash", "among-check"],
            30,
        )?;
        runner.command(
            "/usr/bin/chown",
            &["among-check:among-check", checks_text],
            30,
        )?;
        fs::set_permissions(&checker, fs::Permissions::from_mode(0o755))?;
        let checker = checker.to_str().ok_or("checker path is not UTF-8")?;
        for (check, args) in [
            (
                "installed-desktop-identity-icon-input",
                vec!["check-deb-desktop"],
            ),
            (
                "installed-live-focus",
                vec!["check-live-focus", "/usr/bin/among"],
            ),
            (
                "installed-live-status",
                vec![
                    "check-live-status",
                    root.to_str().ok_or("root path is not UTF-8")?,
                    "/usr/bin/among",
                    "/usr/share/among/plugins",
                ],
            ),
            // Keyboard divider movement in all four directions on the
            // installed executable, not only on a development build.
            (
                "installed-live-resize",
                vec!["check-live-resize", "/usr/bin/among"],
            ),
            // Restored split layout, a pane command that keeps running while
            // its workspace is hidden, a pointer pane move and consent.
            (
                "installed-sessions",
                vec!["check-sessions", "/usr/bin/among"],
            ),
        ] {
            let mut command = vec![
                "--user",
                "among-check",
                "--",
                "/usr/bin/env",
                &check_root,
                checker,
                "ci-x11",
                checker,
            ];
            command.extend(args);
            runner.command("/usr/sbin/runuser", &command, 300)?;
            evidence["checks"]
                .as_array_mut()
                .ok_or("RPM checks array absent")?
                .push(json!(check));
        }
        for check in crate::rpm_lifecycle::run(
            &mut runner,
            Path::new(package),
            &fixture,
            &version,
            Path::new("/home/among-check"),
        )? {
            evidence["checks"]
                .as_array_mut()
                .ok_or("RPM checks array absent")?
                .push(json!(check));
        }
        Ok(())
    })();
    let saved = crate::deb_installed::save_evidence(
        Path::new("/home/among-check"),
        &output.join("user-logs"),
    )
    .and_then(|()| crate::deb_installed::save_evidence(&checks, &output.join("display-logs")));
    evidence["status"] = json!(if result.is_ok() && saved.is_ok() {
        "passed"
    } else {
        "failed"
    });
    storage::atomic_write(
        &output,
        Path::new("runtime-record.json"),
        &serde_json::to_vec_pretty(&evidence)?,
        0o644,
    )?;
    result?;
    saved
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_children_receive_explicit_hosted_ci_identity() {
        let runner = runner(Path::new("/synthetic-evidence"));
        assert_eq!(
            runner.environment.get(&OsString::from("GITHUB_ACTIONS")),
            Some(&OsString::from("true"))
        );
        assert_eq!(
            runner
                .environment
                .get(&OsString::from("RUNNER_ENVIRONMENT")),
            Some(&OsString::from("github-hosted"))
        );
    }
}
