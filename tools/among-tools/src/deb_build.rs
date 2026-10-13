use crate::{deb, platform, process::OwnedProcess, source_snapshot, storage, Result};
use serde_json::json;
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs::{self, File};
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub fn image_identity(image: &str, target: &str) -> Result<()> {
    let prefix = match target {
        "debian13" => "debian@sha256:",
        "ubuntu24.04" => "ubuntu@sha256:",
        _ => return Err("unsupported build OS target".into()),
    };
    let digest = image
        .strip_prefix(prefix)
        .ok_or("build image must be target-matched and digest-pinned")?;
    if digest.len() != 64
        || !digest
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
    {
        return Err("build image must be digest-pinned".into());
    }
    Ok(())
}

fn command(
    program: &str,
    args: &[&str],
    environment: &BTreeMap<OsString, OsString>,
    cwd: &Path,
    log: &Path,
    timeout: Duration,
) -> Result<String> {
    let file = File::create(log)?;
    let mut process = OwnedProcess::spawn(Path::new(program), args, environment, cwd, &file)?;
    if !process.wait(timeout)?.success() {
        eprintln!("{}", fs::read_to_string(log)?);
        return Err(format!("native release command failed; evidence: {}", log.display()).into());
    }
    Ok(fs::read_to_string(log)?.trim().into())
}

fn copy_tree(root: &Path, destination: &Path) -> Result<()> {
    let inputs = source_snapshot::inputs(root, &[])?;
    fs::DirBuilder::new().mode(0o700).create(destination)?;
    for (name, file) in inputs {
        let path = storage::safe_path(destination, Path::new(&name))?;
        fs::create_dir_all(path.parent().ok_or("build input has no parent")?)?;
        storage::atomic_write(destination, Path::new(&name), &file.bytes, file.mode)?;
    }
    Ok(())
}

fn copy_recipe(source: &Path, destination: &Path) -> Result<()> {
    fs::create_dir(destination)?;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let name = entry.file_name();
        let path = storage::safe_path(source, Path::new(&name))?;
        if path.is_dir() {
            copy_recipe(&path, &destination.join(&name))?;
        } else {
            let mode = fs::metadata(&path)?.permissions().mode() & 0o777;
            storage::atomic_write(destination, Path::new(&name), &fs::read(&path)?, mode)?;
        }
    }
    Ok(())
}

pub fn run(
    root: &Path,
    target: &str,
    architecture: &str,
    image: &str,
    output: &Path,
    caller: &BTreeMap<String, String>,
) -> Result<PathBuf> {
    // The release gate precedes any build/output work, including missing inputs.
    let (tag, source) = deb::github_tag_source(root, caller)?;
    let identity = deb::identity(root, &tag, target, architecture)?;
    image_identity(image, target)?;
    if output.symlink_metadata().is_ok() {
        return Err("output directory must be new".into());
    }
    let scratch = PathBuf::from(
        caller
            .get("RUNNER_TEMP")
            .ok_or("release runner temporary directory is missing")?,
    );
    let base = scratch.join(format!(
        "among-deb-build-{}-{}",
        std::process::id(),
        SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos()
    ));
    fs::DirBuilder::new().mode(0o700).create(&base)?;
    let mut environment: BTreeMap<OsString, OsString> = BTreeMap::new();
    for key in [
        "PATH",
        "CARGO_HOME",
        "RUSTUP_HOME",
        "HOME",
        "LANG",
        "LC_ALL",
        "CARGO_BUILD_JOBS",
    ] {
        if let Some(value) = caller.get(key) {
            environment.insert(key.into(), value.into());
        }
    }
    environment.insert("GIT_CONFIG_GLOBAL".into(), "/dev/null".into());
    environment.insert("GIT_CONFIG_NOSYSTEM".into(), "1".into());
    let mut counter = 0usize;
    let mut execute = |program: &str,
                       args: &[&str],
                       cwd: &Path,
                       env: &BTreeMap<OsString, OsString>,
                       timeout|
     -> Result<String> {
        counter += 1;
        command(
            program,
            args,
            env,
            cwd,
            &base.join(format!("command-{counter}.log")),
            timeout,
        )
    };
    let arch = execute(
        "/usr/bin/dpkg",
        &["--print-architecture"],
        root,
        &environment,
        Duration::from_secs(30),
    )?;
    let observed = platform::parse(&fs::read_to_string("/etc/os-release")?, &arch)?;
    let expected = match target {
        "debian13" => ("debian", "13"),
        "ubuntu24.04" => ("ubuntu", "24.04"),
        _ => return Err("unsupported build target".into()),
    };
    if (
        observed.os.as_str(),
        observed.version.as_str(),
        observed.architecture.as_str(),
    ) != (expected.0, expected.1, architecture)
    {
        return Err("native worker OS/architecture differs from build target; cross-build relabelling forbidden".into());
    }
    let rustup = PathBuf::from(caller.get("CARGO_HOME").ok_or("build Cargo home missing")?)
        .join("bin/rustup");
    let toolchain = execute(
        rustup.to_str().ok_or("rustup path is not UTF-8")?,
        &["run", "1.98.1", "rustc", "--version"],
        root,
        &environment,
        Duration::from_secs(60),
    )?;
    if !toolchain.starts_with("rustc 1.98.1 ") {
        return Err("unexpected Rust toolchain".into());
    }
    let safe_directory = format!(
        "safe.directory={}",
        deb::repository_directory(root)?
            .to_str()
            .ok_or("repository path is not UTF-8")?
    );
    let timestamp = execute(
        "/usr/bin/git",
        &["-c", &safe_directory, "show", "-s", "--format=%ct", &source],
        root,
        &environment,
        Duration::from_secs(30),
    )?;
    if timestamp.is_empty() || !timestamp.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err("invalid source timestamp".into());
    }
    let epoch = timestamp.parse::<u64>()?;
    environment.insert("SOURCE_DATE_EPOCH".into(), timestamp.clone().into());
    let date = execute(
        "/usr/bin/date",
        &[
            "--utc",
            "--date",
            &format!("@{timestamp}"),
            "+%a, %d %b %Y %H:%M:%S +0000",
        ],
        root,
        &BTreeMap::from([("LC_ALL".into(), "C".into())]),
        Duration::from_secs(30),
    )?;
    let tree = base.join("source");
    copy_tree(root, &tree)?;
    copy_recipe(&tree.join("packaging/debian"), &tree.join("debian"))?;
    fs::set_permissions(tree.join("debian/rules"), fs::Permissions::from_mode(0o755))?;
    storage::atomic_write(&tree,Path::new("debian/changelog"),format!("among-terminal ({}) unstable; urgency=medium\n\n  * {tag} native candidate from {source}; publication requires installed checks.\n\n -- Among Terminal <hello+amongterminal@highload.zone>  {date}\n",identity.package_version).as_bytes(),0o644)?;
    if nix::unistd::getuid().is_root() {
        execute(
            "/usr/bin/chown",
            &[
                "-R",
                "among-build:among-build",
                base.to_str().ok_or("build path is not UTF-8")?,
            ],
            root,
            &environment,
            Duration::from_secs(60),
        )?;
        // Filesystem-denial tests must run as a real unprivileged user. Root
        // bypasses mode bits and would invalidate their observable assertions.
        execute(
            "/usr/sbin/runuser",
            &[
                "-u",
                "among-build",
                "--",
                "/usr/bin/dpkg-buildpackage",
                "-b",
                "-us",
                "-uc",
            ],
            &tree,
            &environment,
            Duration::from_secs(4500),
        )?;
    } else {
        execute(
            "/usr/bin/dpkg-buildpackage",
            &["-b", "-us", "-uc"],
            &tree,
            &environment,
            Duration::from_secs(4500),
        )?;
    }
    let packages = fs::read_dir(&base)?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<std::io::Result<Vec<_>>>()?
        .into_iter()
        .filter(|path| path.extension() == Some(std::ffi::OsStr::new("deb")))
        .collect::<Vec<_>>();
    let package = base.join(&identity.filename);
    if packages != [package.clone()] {
        return Err("build produced an unexpected package inventory".into());
    }
    for (field, value) in [
        ("Package", "among-terminal"),
        ("Version", identity.package_version.as_str()),
        ("Architecture", architecture),
    ] {
        if execute(
            "/usr/bin/dpkg-deb",
            &[
                "-f",
                package.to_str().ok_or("package path is not UTF-8")?,
                field,
            ],
            &base,
            &environment,
            Duration::from_secs(30),
        )? != value
        {
            return Err("package metadata differs from release identity".into());
        }
    }
    let extracted = base.join("inspect");
    execute(
        "/usr/bin/dpkg-deb",
        &[
            "-x",
            package.to_str().ok_or("package path is not UTF-8")?,
            extracted.to_str().ok_or("inspect path is not UTF-8")?,
        ],
        &base,
        &environment,
        Duration::from_secs(60),
    )?;
    reject_checker_payload(&extracted)?;
    for name in deb::EXECUTABLES {
        let relative = if name == "among" {
            "usr/bin/among".into()
        } else {
            format!("usr/share/among/plugins/{name}")
        };
        deb::elf_architecture(
            &storage::safe_path(&extracted, Path::new(&relative))?,
            architecture,
        )?;
    }
    let entry = extracted.join(format!("usr/share/applications/{}.desktop", deb::APP_ID));
    execute(
        "/usr/bin/desktop-file-validate",
        &[entry.to_str().ok_or("entry path is not UTF-8")?],
        &base,
        &environment,
        Duration::from_secs(30),
    )?;
    if !fs::read_to_string(entry)?.contains("Exec=/usr/bin/among\n") {
        return Err("packaged desktop is not bound to system binary".into());
    }
    let metainfo = extracted.join(format!("usr/share/metainfo/{}.metainfo.xml", deb::APP_ID));
    execute(
        "/usr/bin/appstreamcli",
        &[
            "validate",
            "--no-net",
            "--strict",
            metainfo.to_str().ok_or("metainfo path is not UTF-8")?,
        ],
        &base,
        &environment,
        Duration::from_secs(60),
    )?;
    let dependencies = execute(
        "/usr/bin/dpkg-deb",
        &[
            "-f",
            package.to_str().ok_or("package path is not UTF-8")?,
            "Depends",
        ],
        &base,
        &environment,
        Duration::from_secs(30),
    )?;
    if dependencies
        .split(',')
        .any(|value| value.contains("-dev") || value.contains("cargo") || value.contains("rustc"))
    {
        return Err("development tooling leaked into runtime dependencies".into());
    }
    let build_packages = execute(
        "/usr/bin/dpkg-query",
        &["-W", "-f=${binary:Package}\t${Version}\n"],
        &base,
        &environment,
        Duration::from_secs(60),
    )?;
    let bytes = fs::read(&package)?;
    let digest = storage::sha256(bytes.as_slice())?;
    let mut record = serde_json::to_value(&identity)?;
    for (key, value) in [
        ("schema", json!(1)),
        ("source_sha", json!(source)),
        ("tag", json!(tag)),
        ("sha256", json!(digest)),
        ("image", json!(image)),
        ("rustc", json!(toolchain)),
        ("depends", json!(dependencies)),
        ("source_date_epoch", json!(epoch)),
        (
            "build_packages",
            json!(build_packages.lines().collect::<Vec<_>>()),
        ),
    ] {
        record[key] = value;
    }
    fs::DirBuilder::new().mode(0o755).create(output)?;
    storage::atomic_write(output, Path::new(&identity.filename), &bytes, 0o644)?;
    let (archive, archive_digest) = crate::binary_archive::create(
        &extracted,
        &tag,
        target,
        architecture,
        &dependencies,
        output,
    )?;
    record["binary_archive"] = json!({"filename":archive,"sha256":archive_digest,"scope":"exact inspected DEB executables; distribution runtime libraries required"});
    storage::atomic_write(
        output,
        Path::new("SHA256SUMS"),
        checksum_lines(&[(&digest, &identity.filename), (&archive_digest, &archive)]).as_bytes(),
        0o644,
    )?;
    storage::atomic_write(
        output,
        Path::new("build-record.json"),
        &serde_json::to_vec_pretty(&record)?,
        0o644,
    )?;
    let checker = crate::native_checker::package(
        &std::env::current_exe()?,
        &source,
        &tag,
        target,
        architecture,
        &output.join("native-checker"),
    )?;
    storage::atomic_write(
        output,
        Path::new("native-checker.sha256"),
        format!("{}  native-checker/among-tools\n", checker.sha256).as_bytes(),
        0o644,
    )?;
    println!(
        "Built and inspected {}; installed runtime acceptance still required. Evidence: {}",
        identity.filename,
        base.display()
    );
    Ok(base)
}

/// `SHA256SUMS` in the text form `sha256sum --check` reads: digest, two
/// spaces (text mode), file name. Users verify downloads with exactly that
/// command, so the candidate's file must be in this form.
pub fn checksum_lines(entries: &[(&str, &str)]) -> String {
    entries
        .iter()
        .map(|(digest, name)| format!("{digest}  {name}\n"))
        .collect()
}

pub fn reject_checker_payload(root: &Path) -> Result<()> {
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        if matches!(
            entry.file_name().to_str(),
            Some("among-tools" | "among-uninstall" | "checker-record.json")
        ) {
            return Err("internal tooling leaked into user-facing package payload".into());
        }
        if entry.file_type()?.is_dir() {
            reject_checker_payload(&entry.path())?;
        }
    }
    Ok(())
}
