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

struct Runner {
    base: PathBuf,
    environment: BTreeMap<OsString, OsString>,
    counter: usize,
}
impl Runner {
    fn command(&mut self, program: &str, args: &[&str], seconds: u64) -> Result<String> {
        self.counter += 1;
        let log = self.base.join(format!("command-{}.log", self.counter));
        let mut process = OwnedProcess::spawn(
            Path::new(program),
            args,
            &self.environment,
            &self.base,
            &File::create(&log)?,
        )?;
        if !process.wait(Duration::from_secs(seconds))?.success() {
            return Err(format!("RPM check failed; evidence: {}", log.display()).into());
        }
        Ok(fs::read_to_string(log)?.trim().into())
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

pub fn validate_record(
    record: &Value,
    tag: &str,
    source: &str,
    target: &str,
    architecture: &str,
    package: &Path,
) -> Result<()> {
    let (_, version) = deb::release_version(tag)?;
    let suffix = match (target, architecture) {
        ("fedora43", "amd64") => "fc43.x86_64",
        ("fedora43", "arm64") => "fc43.aarch64",
        ("fedora44", "amd64") => "fc44.x86_64",
        ("fedora44", "arm64") => "fc44.aarch64",
        _ => return Err("unsupported RPM identity".into()),
    };
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

pub fn record(root: &Path, target_name: &str, architecture: &str, directory: &Path) -> Result<()> {
    let (tag, source) = deb::github_tag_source(root, &std::env::vars().collect())?;
    target(target_name, architecture)?;
    let packages = fs::read_dir(directory)?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<std::io::Result<Vec<_>>>()?
        .into_iter()
        .filter(|path| path.extension().is_some_and(|extension| extension == "rpm"))
        .collect::<Vec<_>>();
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
    let observed = std::process::Command::new("/usr/bin/rpm")
        .args(["-qp", "--qf", "%{NAME}|%{VERSION}|%{RELEASE}|%{ARCH}"])
        .arg(package)
        .output()?;
    if !observed.status.success()
        || String::from_utf8(observed.stdout)? != format!("among|{version}|1.fc{dist}|{machine}")
    {
        return Err("actual RPM metadata differs from release tag/target".into());
    }
    let document = json!({"schema":1,"tag":tag,"source_sha":source,"target":target_name,"architecture":architecture,
        "filename":package.file_name().and_then(|name|name.to_str()).ok_or("non UTF-8 RPM")?,
        "sha256":storage::sha256(File::open(package)?)?,"checker_sha256":storage::sha256(File::open(directory.join("among-tools"))?)?});
    validate_record(&document, &tag, &source, target_name, architecture, package)?;
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
    let checker = directory.join("among-tools");
    if checker.canonicalize()? != std::env::current_exe()?.canonicalize()?
        || record["checker_sha256"] != storage::sha256(File::open(&checker)?)?
    {
        return Err("RPM install driver is not the exact transferred checker".into());
    }
    fs::create_dir(output)?;
    let output = output.canonicalize()?;
    let mut runner = runner(&output);
    let mut evidence = json!({"schema":1,"tag":tag,"source_sha":source,"target":target_name,"architecture":architecture,
        "package_sha256":record["sha256"],"checker_sha256":record["checker_sha256"],"status":"in-progress","checks":[],
        "release_acceptance":false,"scope":"native clean RPM installation, X11 desktop/focus/status/Sessions, remove/reinstall leaves synthetic user-home files untouched; no application-state migration, historical upgrade or GNOME/Wayland certification"});
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
        fs::create_dir_all("/tmp/opencode")?;
        runner.command(
            "/usr/bin/chown",
            &["among-check:among-check", "/tmp/opencode"],
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
            (
                "installed-sessions",
                vec!["check-sessions", "/usr/bin/among"],
            ),
        ] {
            let mut command = vec!["--user", "among-check", "--", checker, "ci-x11", checker];
            command.extend(args);
            runner.command("/usr/sbin/runuser", &command, 300)?;
            evidence["checks"]
                .as_array_mut()
                .ok_or("RPM checks array absent")?
                .push(json!(check));
        }
        let states = [
            Path::new("/home/among-check/.config/among/settings.json"),
            Path::new("/home/among-check/.local/state/among/session.json"),
            Path::new("/home/among-check/.config/among/plugins.json"),
        ];
        for state in states {
            fs::create_dir_all(state.parent().ok_or("user state parent absent")?)?;
            fs::write(state, b"{\"synthetic-user-state\":true}")?;
        }
        runner.command(
            "/usr/bin/chown",
            &["-R", "among-check:among-check", "/home/among-check"],
            30,
        )?;
        let digests = states
            .into_iter()
            .map(|path| storage::sha256(File::open(path)?))
            .collect::<Result<Vec<_>>>()?;
        for (label, args) in [
            ("remove", vec!["remove", "-y", "among"]),
            ("reinstall", vec!["install", "-y", package]),
            ("remove-after-reinstall", vec!["remove", "-y", "among"]),
        ] {
            runner.command("/usr/bin/dnf", &args, 300)?;
            for (state, digest) in states.into_iter().zip(&digests) {
                if storage::sha256(File::open(state)?)? != *digest {
                    return Err("RPM operation changed user state".into());
                }
            }
            if label.starts_with("remove") && Path::new("/usr/bin/among").exists() {
                return Err("RPM remove left owned executable".into());
            }
            if label == "reinstall"
                && runner.command("/usr/bin/rpm", &["-q", "--qf", "%{VERSION}", "among"], 30)?
                    != version
            {
                return Err("RPM reinstall version differs".into());
            }
            evidence["checks"]
                .as_array_mut()
                .ok_or("RPM checks array absent")?
                .push(json!(label));
        }
        Ok(())
    })();
    let saved = crate::deb_installed::save_evidence(
        Path::new("/home/among-check"),
        &output.join("user-logs"),
    )
    .and_then(|()| {
        crate::deb_installed::save_evidence(
            Path::new("/tmp/opencode"),
            &output.join("display-logs"),
        )
    });
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
