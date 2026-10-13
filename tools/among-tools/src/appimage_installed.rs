use crate::{deb, deb_installed::Runner, platform, storage, Result};
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    fs::{self, File},
    os::unix::fs::{DirBuilderExt, PermissionsExt},
    path::{Path, PathBuf},
};

const USER: &str = "among-appimage-check";
const HOME: &str = "/home/among-appimage-check";
/// A PATH without any fusermount: the AppImage runtime finds fusermount only
/// through PATH, so this reproduces a host on which FUSE cannot be used.
const NO_FUSE_PATH: &str = "/nonexistent-among-no-fuse";

fn host(name: &str) -> Result<(&'static str, &'static str)> {
    match name {
        "debian13" => Ok(("debian", "13")),
        "ubuntu24.04" => Ok(("ubuntu", "24.04")),
        "fedora43" => Ok(("fedora", "43")),
        _ => Err("unsupported AppImage clean host".into()),
    }
}

/// Reads a candidate directory: exactly one AppImage of the requested
/// architecture, listed with its digest in `SHA256SUMS`, with matching bytes.
pub fn candidate(directory: &Path, architecture: &str) -> Result<(PathBuf, String)> {
    let sums = fs::read_to_string(storage::safe_path(directory, Path::new("SHA256SUMS"))?)?;
    let suffix = format!("-{architecture}.AppImage");
    let mut found = None;
    for line in sums.lines().filter(|line| !line.trim().is_empty()) {
        let (digest, name) = line.split_once("  ").ok_or("malformed SHA256SUMS line")?;
        if !name.starts_with("among-") || !name.ends_with(&suffix) {
            continue;
        }
        if found.is_some() {
            return Err("SHA256SUMS lists several AppImages for one architecture".into());
        }
        let path = storage::safe_path(directory, Path::new(name))?;
        if !path
            .symlink_metadata()
            .is_ok_and(|metadata| metadata.is_file())
            || storage::sha256(File::open(&path)?)? != digest
        {
            return Err(format!("{name} is missing, linked or differs from SHA256SUMS").into());
        }
        found = Some((path, digest.to_string()));
    }
    found.ok_or_else(|| format!("SHA256SUMS lists no AppImage for {architecture}").into())
}

/// A clean host has no Among package, no development packages and no Rust
/// toolchain, so a passing check cannot lean on files the AppImage lacks.
pub fn clean_inventory(packages: &[String], visible_commands: &[&str]) -> Result<()> {
    for name in packages {
        let name = name.split(':').next().unwrap_or(name);
        if name.ends_with("-dev")
            || name.ends_with("-devel")
            || matches!(
                name,
                "among"
                    | "among-terminal"
                    | "cargo"
                    | "rustc"
                    | "rust"
                    | "rustup"
                    | "gcc"
                    | "build-essential"
            )
        {
            return Err(format!("clean AppImage host has {name} installed").into());
        }
    }
    if !visible_commands.is_empty() {
        return Err("build tool visible in clean AppImage host PATH".into());
    }
    Ok(())
}

fn installed_packages(runner: &mut Runner) -> Result<Vec<String>> {
    if Path::new("/usr/bin/dpkg-query").is_file() {
        Ok(runner
            .command(
                "/usr/bin/dpkg-query",
                &["-W", "-f=${binary:Package}\t${db:Status-Status}\n"],
                60,
            )?
            .lines()
            .filter_map(|line| line.split_once('\t'))
            .filter(|(_, status)| *status == "installed")
            .map(|(name, _)| name.to_string())
            .collect())
    } else {
        Ok(runner
            .command("/usr/bin/rpm", &["-qa", "--qf", "%{NAME}\n"], 60)?
            .lines()
            .map(str::to_string)
            .collect())
    }
}

fn add_check(evidence: &mut Value, name: &str) -> Result<()> {
    evidence["checks"]
        .as_array_mut()
        .ok_or("runtime evidence checks absent")?
        .push(name.into());
    Ok(())
}

/// Runs the AppImage on a clean host as an unprivileged user: once mounted
/// through FUSE exactly as a user starts it, and once through the documented
/// no-FUSE method (extract, then start `squashfs-root/AppRun`) with fusermount
/// unreachable. Both modes must pass live focus, live status and sessions.
pub fn run(
    root: &Path,
    host_name: &str,
    architecture: &str,
    directory: &Path,
    check_root: &Path,
    output: &Path,
    caller: &BTreeMap<String, String>,
) -> Result<()> {
    let expected = host(host_name)?;
    if !check_root.is_absolute() {
        return Err("the check working root must be an absolute path".into());
    }
    let check_root_text = check_root
        .to_str()
        .ok_or("check working root is not UTF-8")?
        .to_string();
    if !nix::unistd::getuid().is_root()
        || caller.get("RUNNER_ENVIRONMENT").map(String::as_str) != Some("github-hosted")
    {
        return Err(
            "clean-host AppImage checks require a disposable GitHub-hosted root container".into(),
        );
    }
    let actual = platform::discover()?;
    if (
        actual.os.as_str(),
        actual.version.as_str(),
        actual.architecture.as_str(),
    ) != (expected.0, expected.1, architecture)
    {
        return Err("clean host OS/architecture differs from the requested host".into());
    }
    if !Path::new("/dev/fuse").exists() {
        return Err("the FUSE start check needs /dev/fuse in the clean host container".into());
    }
    let (image, digest) = candidate(directory, architecture)?;
    let checker = std::env::current_exe()?;
    let tools = checker
        .to_str()
        .ok_or("checker path is not UTF-8")?
        .to_string();
    if output.symlink_metadata().is_ok() {
        return Err("evidence output must be new".into());
    }
    fs::DirBuilder::new().mode(0o755).create(output)?;
    let mut runner = Runner::new(
        BTreeMap::from([
            ("PATH".into(), "/usr/sbin:/usr/bin:/sbin:/bin".into()),
            ("HOME".into(), "/root".into()),
            ("LANG".into(), "C.UTF-8".into()),
        ]),
        output.canonicalize()?,
    );
    let mut evidence = json!({"schema":1,"host":host_name,"architecture":architecture,
        "appimage":image.file_name().and_then(|name| name.to_str()),"sha256":digest,
        "checker_sha256":storage::sha256(File::open(&checker)?)?,
        "display":"X11/Xvfb/Openbox","checks":[],"status":"failed"});
    let result = (|| -> Result<()> {
        let visible = ["cargo", "rustc", "rustup"]
            .into_iter()
            .filter(|command| {
                std::env::split_paths("/usr/sbin:/usr/bin:/sbin:/bin")
                    .any(|directory| directory.join(command).is_file())
            })
            .collect::<Vec<_>>();
        let packages = installed_packages(&mut runner)?;
        clean_inventory(&packages, &visible)?;
        add_check(
            &mut evidence,
            "clean-host-without-among-or-development-packages",
        )?;
        runner.command(
            "/usr/sbin/useradd",
            &["--create-home", "--shell", "/bin/bash", USER],
            30,
        )?;
        let home = Path::new(HOME);
        for name in ["config", "data", "cache", "runtime"] {
            fs::DirBuilder::new().mode(0o700).create(home.join(name))?;
        }
        let installed = home.join("Among.AppImage");
        fs::copy(&image, &installed)?;
        fs::set_permissions(&installed, fs::Permissions::from_mode(0o755))?;
        // Display checks keep their working directories and evidence under this
        // root, which the unprivileged user must own.
        fs::create_dir_all(check_root)?;
        runner.command(
            "/usr/bin/chown",
            &[&format!("{USER}:{USER}"), &check_root_text],
            30,
        )?;
        runner.command(
            "/usr/bin/chown",
            &["-R", &format!("{USER}:{USER}"), HOME],
            30,
        )?;
        let installed_text = installed.to_str().ok_or("AppImage path is not UTF-8")?;
        let as_user = |path: &str, program: &str, args: &[&str]| -> Vec<String> {
            let mut argv = vec![
                "-u".to_string(),
                USER.into(),
                "--".into(),
                "/usr/bin/env".into(),
                "-i".into(),
                format!("PATH={path}"),
                format!("HOME={HOME}"),
                "LANG=C.UTF-8".into(),
            ];
            argv.push(program.into());
            argv.extend(args.iter().map(|value| value.to_string()));
            argv
        };
        // Without a reachable fusermount the image cannot be mounted, so a
        // direct start fails; this proves the no-FUSE condition is real.
        let argv = as_user(NO_FUSE_PATH, installed_text, &[]);
        let (started, text) = runner.execute(
            "/usr/sbin/runuser",
            &argv.iter().map(String::as_str).collect::<Vec<_>>(),
            60,
        )?;
        fs::write(runner.base.join("no-fuse-direct-start.log"), &text)?;
        if started || !text.contains("fusermount") {
            return Err("AppImage did not refuse to start without fusermount".into());
        }
        add_check(&mut evidence, "no-fuse-direct-start-refused")?;
        let argv = as_user(
            NO_FUSE_PATH,
            "/bin/sh",
            &[
                "-c",
                &format!("cd {HOME} && exec ./Among.AppImage --appimage-extract"),
            ],
        );
        runner.command(
            "/usr/sbin/runuser",
            &argv.iter().map(String::as_str).collect::<Vec<_>>(),
            300,
        )?;
        let extracted = home.join("squashfs-root");
        if !extracted
            .join("AppRun")
            .symlink_metadata()
            .is_ok_and(|metadata| metadata.is_file())
        {
            return Err("extraction did not produce squashfs-root/AppRun".into());
        }
        add_check(&mut evidence, "no-fuse-extracted-without-fusermount")?;
        let mut unresolved = String::new();
        // Every bundled ELF library, including pixbuf loaders and GTK modules
        // in subdirectories, plus the application itself.
        let mut elves = vec![extracted.join("usr/bin/among")];
        elves.extend(
            crate::appimage_build::bundled_libraries(&extracted)?
                .into_keys()
                .map(|relative| extracted.join(relative)),
        );
        // With several files ldd prints a "<file>:" header before each list.
        let arguments = elves
            .iter()
            .map(|path| path.to_str().ok_or("library path is not UTF-8"))
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let linkage = runner.command("/usr/bin/ldd", &arguments, 300)?;
        fs::write(runner.base.join("linkage.log"), &linkage)?;
        let mut current = "";
        for line in linkage.lines() {
            if !line.starts_with(char::is_whitespace) && line.ends_with(':') {
                current = line;
            } else if line.contains("not found") {
                unresolved.push_str(&format!("{current} {}\n", line.trim()));
            }
        }
        fs::write(runner.base.join("unresolved-libraries.log"), &unresolved)?;
        if !unresolved.is_empty() {
            return Err(
                format!("AppImage libraries unresolved on this host:\n{unresolved}").into(),
            );
        }
        add_check(&mut evidence, "all-bundled-libraries-resolve-on-host")?;
        let root = root.canonicalize()?;
        let root = root.to_str().ok_or("root path is not UTF-8")?.to_string();
        let plugins = extracted.join("usr/share/among/plugins");
        let plugins = plugins
            .to_str()
            .ok_or("plugin path is not UTF-8")?
            .to_string();
        let apprun = extracted.join("AppRun");
        let apprun = apprun
            .to_str()
            .ok_or("AppRun path is not UTF-8")?
            .to_string();
        for (mode, binary) in [("fuse", installed_text.to_string()), ("no-fuse", apprun)] {
            for (name, args, timeout) in [
                ("live-focus", vec!["check-live-focus", binary.as_str()], 240),
                (
                    "live-status",
                    vec![
                        "check-live-status",
                        root.as_str(),
                        binary.as_str(),
                        plugins.as_str(),
                    ],
                    300,
                ),
                ("sessions", vec!["check-sessions", binary.as_str()], 480),
            ] {
                let mut argv = vec![
                    "-u",
                    USER,
                    "--",
                    "/usr/bin/env",
                    "-i",
                    "PATH=/usr/bin:/bin",
                    "LANG=C.UTF-8",
                    "GITHUB_ACTIONS=true",
                ];
                let home_env = format!("HOME={HOME}");
                let config = format!("XDG_CONFIG_HOME={HOME}/config");
                let data = format!("XDG_DATA_HOME={HOME}/data");
                let cache = format!("XDG_CACHE_HOME={HOME}/cache");
                let runtime = format!("XDG_RUNTIME_DIR={HOME}/runtime");
                let check_env = format!("AMONG_CHECK_ROOT={check_root_text}");
                argv.extend([
                    home_env.as_str(),
                    &config,
                    &data,
                    &cache,
                    &runtime,
                    &check_env,
                ]);
                argv.extend([tools.as_str(), "ci-x11", tools.as_str()]);
                argv.extend(args);
                runner.command("/usr/sbin/runuser", &argv, timeout)?;
                add_check(&mut evidence, &format!("{mode}-{name}"))?;
            }
        }
        evidence["status"] = "passed".into();
        Ok(())
    })();
    let saved = crate::deb_installed::save_evidence(check_root, &runner.base.join("display-logs"));
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
    deb::elf_architecture(&image, architecture)?;
    println!("PASS AppImage clean-host checks on {host_name} {architecture} with and without FUSE");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn candidate_requires_one_listed_appimage_with_matching_bytes() -> Result<()> {
        let root = storage::private_directory(&std::env::temp_dir(), "among-appimage-candidate")?;
        fs::write(root.join("among-0.1.0-amd64.AppImage"), b"image bytes")?;
        let digest = storage::sha256(b"image bytes".as_slice())?;
        fs::write(
            root.join("SHA256SUMS"),
            format!("{digest}  among-0.1.0-amd64.AppImage\n"),
        )?;
        assert_eq!(candidate(&root, "amd64")?.1, digest);
        assert!(candidate(&root, "arm64").is_err());
        fs::write(root.join("among-0.1.0-amd64.AppImage"), b"changed bytes")?;
        assert!(candidate(&root, "amd64").is_err());
        fs::write(
            root.join("SHA256SUMS"),
            format!("{digest}  among-0.1.0-amd64.AppImage\n{digest}  among-0.2.0-amd64.AppImage\n"),
        )?;
        assert!(candidate(&root, "amd64").is_err());
        fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn clean_inventory_refuses_among_development_packages_and_toolchains() -> Result<()> {
        let base = ["libc6:amd64".to_string(), "xvfb".into(), "fuse3".into()];
        clean_inventory(&base, &[])?;
        for extra in [
            "among-terminal",
            "libgtk-4-dev:amd64",
            "gtk4-devel",
            "cargo",
            "gcc",
        ] {
            let mut packages = base.to_vec();
            packages.push(extra.into());
            assert!(clean_inventory(&packages, &[]).is_err(), "{extra}");
        }
        assert!(clean_inventory(&base, &["rustc"]).is_err());
        assert!(host("arch").is_err());
        Ok(())
    }
}
