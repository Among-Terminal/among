use crate::{deb, native_checker, platform, process::OwnedProcess, storage, Result};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs::{self, File};
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

pub(crate) struct Runner {
    pub(crate) env: BTreeMap<OsString, OsString>,
    pub(crate) base: PathBuf,
    counter: usize,
}
impl Runner {
    pub(crate) fn new(env: BTreeMap<OsString, OsString>, base: PathBuf) -> Self {
        Self {
            env,
            base,
            counter: 0,
        }
    }
    pub(crate) fn execute(
        &mut self,
        program: &str,
        args: &[&str],
        timeout: u64,
    ) -> Result<(bool, String)> {
        let base = self.base.clone();
        self.execute_in(&base, program, args, timeout)
    }
    /// Runs in `directory`, for commands that name files relative to where a
    /// user would stand; the log still lands in the evidence directory.
    pub(crate) fn execute_in(
        &mut self,
        directory: &Path,
        program: &str,
        args: &[&str],
        timeout: u64,
    ) -> Result<(bool, String)> {
        self.counter += 1;
        let path = self.base.join(format!("command-{}.log", self.counter));
        let file = File::create(&path)?;
        let mut child = OwnedProcess::spawn(Path::new(program), args, &self.env, directory, &file)?;
        let success = child.wait(Duration::from_secs(timeout))?.success();
        Ok((success, fs::read_to_string(path)?))
    }
    pub(crate) fn command(&mut self, program: &str, args: &[&str], timeout: u64) -> Result<String> {
        let base = self.base.clone();
        self.command_in(&base, program, args, timeout)
    }
    pub(crate) fn command_in(
        &mut self,
        directory: &Path,
        program: &str,
        args: &[&str],
        timeout: u64,
    ) -> Result<String> {
        let (success, text) = self.execute_in(directory, program, args, timeout)?;
        if !success {
            eprintln!("{text}");
            return Err(format!(
                "installed release command failed; evidence: {}",
                self.base.display()
            )
            .into());
        }
        Ok(text)
    }
    fn runtime_only(&mut self) -> Result<()> {
        let packages = self.command(
            "/usr/bin/dpkg-query",
            &["-W", "-f=${binary:Package}\t${db:Status-Status}\n"],
            30,
        )?;
        let path = self
            .env
            .get(&OsString::from("PATH"))
            .ok_or("runtime PATH missing")?;
        let visible = ["cargo", "rustc", "rustup"]
            .into_iter()
            .filter(|command| {
                std::env::split_paths(path).any(|directory| directory.join(command).is_file())
            })
            .collect::<Vec<_>>();
        deb::runtime_inventory_only(&packages, &visible)
    }
}

pub fn run(
    root: &Path,
    target: &str,
    architecture: &str,
    candidate: &Path,
    output: &Path,
    caller: &BTreeMap<String, String>,
) -> Result<()> {
    let (tag, source) = deb::github_tag_source(root, caller)?;
    if !nix::unistd::getuid().is_root()
        || caller.get("RUNNER_ENVIRONMENT").map(String::as_str) != Some("github-hosted")
    {
        return Err(
            "installed package operations require a disposable GitHub-hosted root container".into(),
        );
    }
    let candidate = candidate.canonicalize()?;
    let (package, record) =
        deb::validate_candidate(root, &candidate, &tag, &source, target, architecture)?;
    crate::binary_archive::validate(&record, &candidate, &tag, target, architecture)?;
    let previous = crate::deb_previous::read(root, target, architecture)?;
    let documented = crate::deb_lifecycle::documented_commands(&fs::read_to_string(
        storage::safe_path(root, Path::new("docs/debian-packaging.md"))?,
    )?)?;
    let checker = native_checker::validate(
        &candidate.join("native-checker"),
        &source,
        &tag,
        target,
        architecture,
    )?;
    if fs::canonicalize(&checker)? != std::env::current_exe()?.canonicalize()? {
        return Err(
            "installed orchestration must execute the exact transferred native checker".into(),
        );
    }
    let env = BTreeMap::from([
        ("PATH".into(), "/usr/sbin:/usr/bin:/sbin:/bin".into()),
        ("HOME".into(), "/root".into()),
        ("LANG".into(), "C.UTF-8".into()),
        ("DEBIAN_FRONTEND".into(), "noninteractive".into()),
    ]);
    // Read-only policy preflight is outside the evidence output. Nothing is
    // installed until source, package, checker and native OS all agree.
    let scratch = Path::new(
        caller
            .get("RUNNER_TEMP")
            .ok_or("runner temporary directory missing")?,
    );
    let base = scratch.join(format!("among-deb-preflight-{}", std::process::id()));
    fs::create_dir(&base)?;
    let mut runner = Runner {
        env,
        base,
        counter: 0,
    };
    let arch = runner.command("/usr/bin/dpkg", &["--print-architecture"], 30)?;
    let os = platform::parse(&fs::read_to_string("/etc/os-release")?, arch.trim())?;
    let expected = match target {
        "debian13" => ("debian", "13"),
        "ubuntu24.04" => ("ubuntu", "24.04"),
        _ => return Err("unsupported runtime target".into()),
    };
    if (
        os.os.as_str(),
        os.version.as_str(),
        os.architecture.as_str(),
    ) != (expected.0, expected.1, architecture)
    {
        return Err("runtime OS/architecture differs from candidate target".into());
    }
    for (field, expected) in [
        ("Package", "among-terminal"),
        (
            "Version",
            record["package_version"]
                .as_str()
                .ok_or("candidate version absent")?,
        ),
        ("Architecture", architecture),
        (
            "Depends",
            record["depends"]
                .as_str()
                .ok_or("candidate dependency record absent")?,
        ),
    ] {
        if runner
            .command(
                "/usr/bin/dpkg-deb",
                &[
                    "-f",
                    package.to_str().ok_or("package path is not UTF-8")?,
                    field,
                ],
                30,
            )?
            .trim()
            != expected
        {
            return Err("candidate metadata differs from build record".into());
        }
    }
    runner.runtime_only()?;
    fs::DirBuilder::new().mode(0o755).create(output)?;
    runner.base = output.canonicalize()?;
    let mut evidence = json!({"schema":1,"target":target,"architecture":architecture,"tag":tag,"source_sha":source,"package_sha256":record["sha256"],"binary_archive":record["binary_archive"],"checker_sha256":storage::sha256(File::open(&checker)?)?,"display":"X11/Xvfb/Openbox","checks":[],"status":"failed","scope":"partial-installed-runtime-lifecycle-and-extracted-binary-runtime","release_acceptance":false});
    // Resolved once here as root; each unprivileged check user receives it
    // explicitly, because its own HOME would yield a different default.
    let checks = crate::scratch::root()?;
    let checks_text = checks.to_str().ok_or("check root is not UTF-8")?;
    let check_root = format!("{}={checks_text}", crate::scratch::VARIABLE);
    let result = (|| -> Result<()> {
        let published = crate::deb_lifecycle::fetch_published(
            &mut runner,
            &previous,
            architecture,
            record["package_version"]
                .as_str()
                .ok_or("candidate version absent")?,
        )?;
        add_check(&mut evidence, "published-release-fetched-and-pinned")?;
        missing_dependency(&mut runner)?;
        add_check(&mut evidence, "unavailable-dependency-rejected")?;
        // Slim base images exclude most /usr/share/doc files. Acceptance must
        // observe our complete licensed payload, including the older-only
        // lifecycle marker, without changing filtering for unrelated packages.
        let policy_root = Path::new("/etc/dpkg/dpkg.cfg.d");
        let policy = Path::new("zz-among-release-acceptance");
        if policy_root.join(policy).symlink_metadata().is_ok() {
            return Err("disposable acceptance documentation policy already exists".into());
        }
        storage::atomic_write(
            policy_root,
            policy,
            b"path-include /usr/share/doc/among-terminal/*\n",
            0o644,
        )?;
        add_check(&mut evidence, "complete-among-documentation-included")?;
        runner.command(
            "/usr/bin/apt-get",
            &[
                "install",
                "-y",
                "--no-install-recommends",
                package.to_str().ok_or("package path is not UTF-8")?,
            ],
            240,
        )?;
        let configured = runner.command(
            "/usr/bin/dpkg-query",
            &["-W", "-f=${Status}\t${Version}", "among-terminal"],
            30,
        )?;
        if configured
            != format!(
                "install ok installed\t{}",
                record["package_version"]
                    .as_str()
                    .ok_or("version missing")?
            )
        {
            return Err("candidate not fully configured at exact version".into());
        }
        runner.runtime_only()?;
        let linkage = runner.command("/usr/bin/ldd", &["/usr/bin/among"], 30)?;
        fs::write(runner.base.join("linkage.log"), &linkage)?;
        if linkage.contains("not found") {
            return Err("installed application has unresolved native libraries".into());
        }
        add_check(&mut evidence, "apt-configured-and-linked")?;
        runner.command(
            "/usr/sbin/useradd",
            &["--create-home", "--shell", "/bin/bash", "among-check"],
            30,
        )?;
        let home = Path::new("/home/among-check");
        for name in ["config", "data", "cache", "runtime"] {
            fs::DirBuilder::new().mode(0o700).create(home.join(name))?;
        }
        runner.command(
            "/usr/bin/chown",
            &["among-check:among-check", checks_text],
            30,
        )?;
        runner.command(
            "/usr/bin/chown",
            &["-R", "among-check:among-check", "/home/among-check"],
            30,
        )?;
        let root = root.canonicalize()?;
        let tools = checker.to_str().ok_or("checker path is not UTF-8")?;
        for (name, args, timeout) in [
            (
                "installed-desktop-identity-icon-input",
                vec!["check-deb-desktop"],
                120,
            ),
            (
                "installed-live-focus",
                vec!["check-live-focus", "/usr/bin/among"],
                180,
            ),
            (
                "installed-check-live-status",
                vec![
                    "check-live-status",
                    root.to_str().ok_or("root path is not UTF-8")?,
                    "/usr/bin/among",
                    "/usr/share/among/plugins",
                ],
                240,
            ),
            (
                "installed-check-sessions",
                vec!["check-sessions", "/usr/bin/among"],
                420,
            ),
            (
                "installed-keyboard-divider-movement",
                vec!["check-live-resize", "/usr/bin/among"],
                180,
            ),
        ] {
            let mut argv = vec![
                "-u",
                "among-check",
                "--",
                "/usr/bin/env",
                "-i",
                "PATH=/usr/bin:/bin",
                "HOME=/home/among-check",
                "LANG=C.UTF-8",
                "GITHUB_ACTIONS=true",
                "XDG_CONFIG_HOME=/home/among-check/config",
                "XDG_DATA_HOME=/home/among-check/data",
                "XDG_CACHE_HOME=/home/among-check/cache",
                "XDG_RUNTIME_DIR=/home/among-check/runtime",
                &check_root,
                tools,
                "ci-x11",
                tools,
            ];
            argv.extend(args);
            runner.command("/usr/sbin/runuser", &argv, timeout)?;
            add_check(&mut evidence, name)?;
        }
        let archive_home = Path::new("/home/among-archive-check");
        runner.command(
            "/usr/sbin/useradd",
            &[
                "--create-home",
                "--shell",
                "/bin/bash",
                "among-archive-check",
            ],
            30,
        )?;
        crate::binary_archive::extract_checked(
            &record,
            &candidate,
            &tag,
            target,
            architecture,
            Path::new("/"),
            &archive_home.join("bundle"),
        )?;
        add_check(&mut evidence, "archive-extracted-exact-package-payload")?;
        for check in crate::deb_lifecycle::run(
            &mut runner,
            &package,
            &record,
            home,
            &published,
            &previous.version,
            &documented,
        )? {
            add_check(&mut evidence, &check)?;
        }
        if Path::new("/usr/bin/among").symlink_metadata().is_ok()
            || Path::new("/usr/share/among").symlink_metadata().is_ok()
        {
            return Err(
                "archive launch acceptance requires the installed package payload to be absent"
                    .into(),
            );
        }
        for name in ["config", "data", "cache", "runtime"] {
            fs::DirBuilder::new()
                .mode(0o700)
                .create(archive_home.join(name))?;
        }
        runner.command(
            "/usr/bin/chown",
            &[
                "-R",
                "among-archive-check:among-archive-check",
                "/home/among-archive-check",
            ],
            30,
        )?;
        runner.command(
            "/usr/bin/chown",
            &["among-archive-check:among-archive-check", checks_text],
            30,
        )?;
        runner.runtime_only()?;
        let archive_binary = "/home/among-archive-check/bundle/bin/among";
        let linkage = runner.command("/usr/bin/ldd", &[archive_binary], 30)?;
        fs::write(runner.base.join("archive-linkage.log"), &linkage)?;
        if linkage.contains("not found") {
            return Err("extracted binary has unresolved native libraries".into());
        }
        add_check(&mut evidence, "archive-linked-without-installed-among")?;
        for (name, args, timeout) in [
            (
                "archive-live-focus",
                vec!["check-live-focus", archive_binary],
                180,
            ),
            (
                "archive-live-status",
                vec![
                    "check-live-status",
                    root.to_str().ok_or("root path is not UTF-8")?,
                    archive_binary,
                    "/home/among-archive-check/bundle/plugins",
                ],
                240,
            ),
            (
                "archive-sessions",
                vec!["check-sessions", archive_binary],
                420,
            ),
        ] {
            let mut argv = vec![
                "-u",
                "among-archive-check",
                "--",
                "/usr/bin/env",
                "-i",
                "PATH=/usr/bin:/bin",
                "HOME=/home/among-archive-check",
                "LANG=C.UTF-8",
                "GITHUB_ACTIONS=true",
                "XDG_CONFIG_HOME=/home/among-archive-check/config",
                "XDG_DATA_HOME=/home/among-archive-check/data",
                "XDG_CACHE_HOME=/home/among-archive-check/cache",
                "XDG_RUNTIME_DIR=/home/among-archive-check/runtime",
                &check_root,
                tools,
                "ci-x11",
                tools,
            ];
            argv.extend(args);
            runner.command("/usr/sbin/runuser", &argv, timeout)?;
            add_check(&mut evidence, name)?;
        }
        evidence["status"] = "passed".into();
        Ok(())
    })();
    // A failed scenario retains a failed receipt; it never becomes a release
    // acceptance result merely because preceding checks succeeded.
    let saved = save_evidence(
        Path::new("/home/among-check"),
        &runner.base.join("user-logs"),
    )
    .and_then(|()| {
        save_evidence(
            Path::new("/home/among-archive-check"),
            &runner.base.join("archive-user-logs"),
        )
    })
    .and_then(|()| save_evidence(&checks, &runner.base.join("display-logs")));
    if saved.is_err() {
        evidence["status"] = "failed".into();
    }
    storage::atomic_write(
        &runner.base,
        Path::new("runtime-record.json"),
        &serde_json::to_vec_pretty(&evidence)?,
        0o644,
    )?;
    result?;
    saved?;
    println!(
        "PASS scoped installed runtime/lifecycle; remaining release acceptance gates are separate"
    );
    Ok(())
}

pub(crate) fn save_evidence(source: &Path, destination: &Path) -> Result<()> {
    if !source.exists() {
        return Ok(());
    }
    fs::create_dir(destination)?;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        if (entry.file_name() == "runtime"
            && matches!(
                source.to_str(),
                Some("/home/among-check" | "/home/among-archive-check")
            ))
            || (source == Path::new("/home/among-archive-check") && entry.file_name() == "bundle")
        {
            continue;
        }
        let kind = entry.file_type()?;
        if kind.is_dir() {
            save_evidence(&entry.path(), &destination.join(entry.file_name()))?;
        } else if kind.is_file() {
            fs::copy(entry.path(), destination.join(entry.file_name()))?;
        }
    }
    Ok(())
}

fn add_check(evidence: &mut Value, name: &str) -> Result<()> {
    evidence["checks"]
        .as_array_mut()
        .ok_or("runtime evidence checks absent")?
        .push(name.into());
    Ok(())
}
fn missing_dependency(runner: &mut Runner) -> Result<()> {
    let tree = runner.base.join("missing-dependency");
    fs::create_dir_all(tree.join("DEBIAN"))?;
    fs::write(tree.join("DEBIAN/control"),"Package: among-ci-unavailable-dependency\nVersion: 1.0\nArchitecture: all\nMaintainer: Among Terminal <hello+amongterminal@highload.zone>\nDepends: among-ci-missing-required-library-6fd17630\nDescription: Negative dependency resolution fixture, not a release asset\n")?;
    let package = runner.base.join("missing-dependency.deb");
    runner.command(
        "/usr/bin/dpkg-deb",
        &[
            "--build",
            tree.to_str().ok_or("fixture path is not UTF-8")?,
            package
                .to_str()
                .ok_or("fixture package path is not UTF-8")?,
        ],
        30,
    )?;
    let (success, text) = runner.execute(
        "/usr/bin/apt-get",
        &[
            "install",
            "--simulate",
            package
                .to_str()
                .ok_or("fixture package path is not UTF-8")?,
        ],
        60,
    )?;
    fs::write(runner.base.join("missing-dependency.log"), &text)?;
    if success || !text.contains("among-ci-missing-required-library-6fd17630") {
        return Err("APT did not reject unresolved dependency".into());
    }
    Ok(())
}
