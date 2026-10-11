use crate::{storage, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::{Path, PathBuf};

const APP_ID: &str = "com.github.among-terminal";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Record {
    pub path: PathBuf,
    pub sha256: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Manifest {
    pub version: u32,
    pub files: BTreeMap<String, Record>,
}

pub struct Layout {
    pub destinations: BTreeMap<String, PathBuf>,
    pub state: PathBuf,
    pub documentation: PathBuf,
}

pub fn checked_absolute(path: &Path) -> Result<PathBuf> {
    let relative = path
        .strip_prefix("/")
        .map_err(|_| "installation paths must be absolute")?;
    storage::safe_path(Path::new("/"), relative)
}

fn xdg(name: &str, home: &Path, fallback: &str) -> Result<PathBuf> {
    let path = match std::env::var_os(name).filter(|value| !value.is_empty()) {
        Some(value) => PathBuf::from(value),
        None => home.join(fallback),
    };
    checked_absolute(&path)
}

impl Layout {
    pub fn discover() -> Result<Self> {
        let home = checked_absolute(&PathBuf::from(
            std::env::var_os("HOME").ok_or("HOME is missing")?,
        ))?;
        Self::new(
            &home,
            &xdg("XDG_DATA_HOME", &home, ".local/share")?,
            &xdg("XDG_STATE_HOME", &home, ".local/state")?,
        )
    }

    pub fn new(home: &Path, data: &Path, state: &Path) -> Result<Self> {
        let home = checked_absolute(home)?;
        let data = checked_absolute(data)?;
        let state = checked_absolute(&state.join("among-installer"))?;
        let documentation = data.join("doc/among");
        let destinations = BTreeMap::from([
            ("binary".into(), home.join(".local/bin/among")),
            (
                "desktop".into(),
                data.join(format!("applications/{APP_ID}.desktop")),
            ),
            (
                "icon".into(),
                data.join(format!("icons/hicolor/scalable/apps/{APP_ID}.svg")),
            ),
            (
                "metadata".into(),
                data.join(format!("metainfo/{APP_ID}.metainfo.xml")),
            ),
            ("license".into(), documentation.join("LICENSE")),
            ("notice".into(), documentation.join("NOTICE")),
            ("installer".into(), documentation.join("scripts/install.py")),
            (
                "uninstaller".into(),
                home.join(".local/bin/among-uninstall"),
            ),
        ]);
        Ok(Self {
            destinations,
            state,
            documentation,
        })
    }

    pub fn read_manifest(&mut self) -> Result<Option<Manifest>> {
        let path = checked_absolute(&self.state.join("install.json"))?;
        if !path.exists() {
            return Ok(None);
        }
        if !path.is_file() {
            return Err("installer manifest is not a regular file".into());
        }
        let manifest: Manifest = serde_json::from_value(storage::parse_json(&fs::read(path)?)?)?;
        if manifest.version != 1 {
            return Err("Unsupported installer manifest".into());
        }
        for (key, record) in &manifest.files {
            if key.starts_with("THIRD_PARTY/") {
                storage::relative_path(Path::new(key))?;
                self.destinations
                    .insert(key.clone(), self.documentation.join(key));
            }
            let expected = self
                .destinations
                .get(key)
                .ok_or("unknown installer ownership key")?;
            if &record.path != expected {
                return Err("Installation paths changed; restore HOME/XDG_DATA_HOME before updating or removing".into());
            }
            if record.sha256.len() != 64
                || !record
                    .sha256
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
            {
                return Err("invalid ownership digest".into());
            }
            checked_absolute(&record.path)?;
        }
        Ok(Some(manifest))
    }

    pub fn check_owned(&self, records: &BTreeMap<String, Record>) -> Result<()> {
        for record in records.values() {
            let path = checked_absolute(&record.path)?;
            if !path.exists() {
                continue;
            }
            if !path.is_file() {
                return Err("owned destination is not a regular file".into());
            }
            if storage::sha256(fs::File::open(path)?)? != record.sha256 {
                return Err(
                    "File is not owned by this installer or was modified; move it aside and retry"
                        .into(),
                );
            }
        }
        Ok(())
    }
}

pub fn uninstall(mut layout: Layout) -> Result<()> {
    if nix::unistd::geteuid().is_root() {
        return Err("Run as your normal user, not sudo".into());
    }
    checked_absolute(&layout.state)?;
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&layout.state)?;
    let _lock = storage::ExclusiveLock::acquire(&layout.state, Path::new("lock"))?;
    let Some(manifest) = layout.read_manifest()? else {
        println!("No installation owned by this installer was found.");
        return Ok(());
    };
    layout.check_owned(&manifest.files)?;
    for record in manifest.files.values() {
        let path = checked_absolute(&record.path)?;
        match fs::remove_file(path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    fs::remove_file(checked_absolute(&layout.state.join("install.json"))?)?;
    refresh(&layout);
    println!("Removed Among Terminal. Settings, sessions and plugins were preserved.");
    Ok(())
}

pub struct Payload {
    pub bytes: Vec<u8>,
    pub mode: u32,
}

fn write_absolute(path: &Path, bytes: &[u8], mode: u32) -> Result<()> {
    let path = checked_absolute(path)?;
    let parent = path.parent().ok_or("payload destination has no parent")?;
    fs::create_dir_all(parent)?;
    storage::atomic_write(
        parent,
        Path::new(path.file_name().ok_or("destination has no filename")?),
        bytes,
        mode,
    )
}

fn transaction(
    layout: &Layout,
    payload: &BTreeMap<String, Payload>,
    before_commit: impl FnOnce() -> Result<()>,
) -> Result<()> {
    let previous = layout
        .destinations
        .iter()
        .map(|(key, path)| {
            checked_absolute(path)?;
            let state = if path.exists() {
                Some((
                    fs::read(path)?,
                    fs::metadata(path)?.permissions().mode() & 0o777,
                ))
            } else {
                None
            };
            Ok((key.clone(), state))
        })
        .collect::<Result<BTreeMap<_, _>>>()?;
    let manifest_path = layout.state.join("install.json");
    let old_manifest = if manifest_path.exists() {
        Some(fs::read(&manifest_path)?)
    } else {
        None
    };
    let mut touched = Vec::new();
    let operation = (|| -> Result<()> {
        for (key, path) in &layout.destinations {
            touched.push(key.clone());
            if let Some(content) = payload.get(key) {
                write_absolute(path, &content.bytes, content.mode)?;
            } else if path.exists() {
                fs::remove_file(checked_absolute(path)?)?;
            }
        }
        before_commit()?;
        let files = payload
            .iter()
            .map(|(key, content)| {
                Ok((
                    key.clone(),
                    Record {
                        path: layout.destinations[key].clone(),
                        sha256: storage::sha256(content.bytes.as_slice())?,
                    },
                ))
            })
            .collect::<Result<BTreeMap<_, _>>>()?;
        write_absolute(
            &manifest_path,
            &serde_json::to_vec_pretty(&Manifest { version: 1, files })?,
            0o600,
        )
    })();
    if let Err(error) = operation {
        let mut failures = Vec::new();
        for key in touched.into_iter().rev() {
            let path = &layout.destinations[&key];
            let rollback = match &previous[&key] {
                Some((bytes, mode)) if matches_previous(path, bytes, *mode) => Ok(()),
                Some((bytes, mode)) => write_absolute(path, bytes, *mode),
                None => match fs::remove_file(path) {
                    Ok(()) => Ok(()),
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
                    Err(error) => Err(error.into()),
                },
            };
            if let Err(error) = rollback {
                failures.push(error.to_string());
            }
        }
        let rollback = match old_manifest {
            Some(bytes) if matches_previous(&manifest_path, &bytes, 0o600) => Ok(()),
            Some(bytes) => write_absolute(&manifest_path, &bytes, 0o600),
            None => match fs::remove_file(&manifest_path) {
                Ok(()) => Ok(()),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(error) => Err(error.into()),
            },
        };
        if let Err(error) = rollback {
            failures.push(error.to_string());
        }
        return Err(format!(
            "installation transaction failed: {error}; rollback failures: {failures:?}"
        )
        .into());
    }
    Ok(())
}

fn matches_previous(path: &Path, bytes: &[u8], mode: u32) -> bool {
    // Atomic writes can fail before replacing the destination. Rewriting an
    // unchanged original during rollback can itself fail on an exhausted disk.
    fs::symlink_metadata(path).is_ok_and(|metadata| {
        metadata.is_file()
            && metadata.permissions().mode() & 0o777 == mode
            && fs::read(path).is_ok_and(|current| current == bytes)
    })
}

pub fn install_payload(layout: Layout, payload: BTreeMap<String, Payload>) -> Result<()> {
    if nix::unistd::geteuid().is_root() {
        return Err("Run as your normal user, not sudo".into());
    }
    checked_absolute(&layout.state)?;
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&layout.state)?;
    let _lock = storage::ExclusiveLock::acquire(&layout.state, Path::new("lock"))?;
    install_locked(layout, payload)
}

fn install_locked(mut layout: Layout, payload: BTreeMap<String, Payload>) -> Result<()> {
    let records = layout
        .read_manifest()?
        .map(|manifest| manifest.files)
        .unwrap_or_default();
    for (key, content) in &payload {
        if key.starts_with("THIRD_PARTY/") {
            storage::relative_path(Path::new(key))?;
            if content.bytes.is_empty() || content.mode != 0o644 {
                return Err("invalid third-party license payload".into());
            }
            layout
                .destinations
                .insert(key.clone(), layout.documentation.join(key));
        }
        if !layout.destinations.contains_key(key) || key == "installer" {
            return Err("unknown or retired installation payload key".into());
        }
    }
    for required in [
        "binary",
        "desktop",
        "icon",
        "metadata",
        "license",
        "notice",
        "uninstaller",
    ] {
        let content = payload
            .get(required)
            .ok_or("installation payload is incomplete")?;
        let mode = if matches!(required, "binary" | "uninstaller") {
            0o755
        } else {
            0o644
        };
        if content.bytes.is_empty() || content.mode != mode {
            return Err("invalid installation payload bytes or permissions".into());
        }
    }
    layout.check_owned(&records)?;
    for (key, path) in &layout.destinations {
        checked_absolute(path)?;
        if path.exists() && !records.contains_key(key) {
            return Err("existing destination has no installer ownership".into());
        }
    }
    transaction(&layout, &payload, || Ok(()))?;
    refresh(&layout);
    println!("Installed Among Terminal: {}\nLaunch Among Terminal from the applications menu.\nUpdates: rerun this installer. Uninstall: {}",layout.destinations["binary"].display(),layout.destinations["uninstaller"].display());
    if !std::env::var_os("PATH").is_some_and(|paths| {
        std::env::split_paths(&paths)
            .any(|path| Some(path.as_path()) == layout.destinations["binary"].parent())
    }) {
        println!(
            "For terminal launch use {} (its directory is not in PATH).",
            layout.destinations["binary"].display()
        );
    }
    Ok(())
}

fn executable(name: &str) -> Option<PathBuf> {
    std::env::split_paths(&std::env::var_os("PATH")?)
        .filter(|path| path.is_absolute())
        .map(|path| path.join(name))
        .find(|path| {
            path.is_file()
                && fs::metadata(path)
                    .is_ok_and(|metadata| metadata.permissions().mode() & 0o111 != 0)
        })
}

fn external(program: &Path, args: &[&str], cwd: &Path, log: &Path) -> Result<()> {
    let env = crate::process::allowed_environment(
        &std::env::vars_os().collect(),
        &[
            "PATH",
            "HOME",
            "LANG",
            "CARGO_HOME",
            "RUSTUP_HOME",
            "CARGO_BUILD_JOBS",
            "CARGO_PROFILE_DEV_DEBUG",
            "CARGO_NET_OFFLINE",
        ],
    );
    let file = fs::File::create(log)?;
    let mut child = crate::process::OwnedProcess::spawn(program, args, &env, cwd, &file)?;
    if !child.wait(std::time::Duration::from_secs(1800))?.success() {
        return Err(format!(
            "external installer operation failed: {}; log: {}",
            program.display(),
            log.display()
        )
        .into());
    }
    Ok(())
}

fn refresh(layout: &Layout) {
    let operations = [
        (
            "update-desktop-database",
            vec![layout.destinations["desktop"]
                .parent()
                .map(|path| path.as_os_str().to_owned())
                .unwrap_or_default()],
        ),
        (
            "gtk-update-icon-cache",
            vec![
                "--force".into(),
                "--ignore-theme-index".into(),
                layout.destinations["icon"]
                    .ancestors()
                    .nth(3)
                    .map(|path| path.as_os_str().to_owned())
                    .unwrap_or_default(),
            ],
        ),
    ];
    for (name, arguments) in operations {
        if let Some(program) = executable(name) {
            let result = (|| -> Result<()> {
                let args = arguments
                    .iter()
                    .map(|value| value.to_str().ok_or("cache path is not UTF-8"))
                    .collect::<std::result::Result<Vec<_>, _>>()?;
                external(
                    &program,
                    &args,
                    &layout.state,
                    &layout.state.join(format!("{name}.log")),
                )
            })();
            if let Err(error) = result {
                eprintln!("Warning: {error}");
            }
        }
    }
}

pub struct InstallOptions {
    pub binary: Option<PathBuf>,
    pub install_deps: bool,
    pub uninstall: bool,
}

pub fn options(args: &[String]) -> Result<InstallOptions> {
    let mut options = InstallOptions {
        binary: None,
        install_deps: false,
        uninstall: false,
    };
    let mut args = args.iter();
    while let Some(argument) = args.next() {
        match argument.as_str() {
            "--binary" if options.binary.is_none() => {
                options.binary = Some(PathBuf::from(
                    args.next().ok_or("--binary requires a path")?,
                ))
            }
            "--install-deps" if !options.install_deps => options.install_deps = true,
            "--uninstall" if !options.uninstall => options.uninstall = true,
            _ => return Err("unknown or repeated install option".into()),
        }
    }
    if options.uninstall && (options.binary.is_some() || options.install_deps) {
        return Err("--uninstall cannot be combined with --binary or --install-deps".into());
    }
    if options.binary.is_some() && options.install_deps {
        return Err("--install-deps is only for source builds".into());
    }
    Ok(options)
}

pub fn install(root: &Path, args: &[String]) -> Result<()> {
    let options = options(args)?;
    let mut layout = Layout::discover()?;
    if options.uninstall {
        return uninstall(layout);
    }
    if nix::unistd::geteuid().is_root() {
        return Err("Run as your normal user, not sudo".into());
    }
    checked_absolute(&layout.state)?;
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&layout.state)?;
    let _lock = storage::ExclusiveLock::acquire(&layout.state, Path::new("lock"))?;
    let records = layout
        .read_manifest()?
        .map(|manifest| manifest.files)
        .unwrap_or_default();
    layout.check_owned(&records)?;
    for (key, path) in &layout.destinations {
        checked_absolute(path)?;
        if path.exists() && !records.contains_key(key) {
            return Err("existing destination has no installer ownership".into());
        }
    }
    let uninstaller = std::env::current_exe()?
        .parent()
        .ok_or("tool binary has no parent")?
        .join("among-uninstall");
    let binary = if let Some(binary) = options.binary {
        binary.canonicalize()?
    } else {
        if options.install_deps {
            let sudo =
                executable("sudo").ok_or("--install-deps supports Debian/Ubuntu with sudo only")?;
            let apt = executable("apt-get")
                .ok_or("--install-deps supports Debian/Ubuntu with sudo only")?;
            let apt = apt.to_str().ok_or("apt-get path is not UTF-8")?;
            external(
                &sudo,
                &[apt, "update"],
                &layout.state,
                &layout.state.join("apt-update.log"),
            )?;
            external(
                &sudo,
                &[
                    apt,
                    "install",
                    "-y",
                    "build-essential",
                    "pkg-config",
                    "libgtk-4-dev",
                    "libadwaita-1-dev",
                    "libvte-2.91-gtk4-dev",
                    "desktop-file-utils",
                    "libgtk-4-bin",
                    "librsvg2-common",
                ],
                &layout.state,
                &layout.state.join("apt-install.log"),
            )?;
        }
        let cargo = executable("cargo")
            .ok_or("Cargo is missing. Install Rust >= 1.98 with rustup, then retry.")?;
        let pkg = executable("pkg-config")
            .ok_or("pkg-config is missing; retry with --install-deps on Debian/Ubuntu")?;
        for requirement in [
            "gtk4 >= 4.12",
            "libadwaita-1 >= 1.5",
            "vte-2.91-gtk4 >= 0.76",
        ] {
            external(
                &pkg,
                &["--exists", requirement],
                &layout.state,
                &layout.state.join("native-dependency.log"),
            )?;
        }
        let stage = layout.state.join(format!(
            "among-build-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_nanos()
        ));
        fs::DirBuilder::new().mode(0o700).create(&stage)?;
        let root = root.canonicalize()?;
        let target = stage.join("target");
        let result = external(
            &cargo,
            &[
                "install",
                "--path",
                root.to_str().ok_or("source path is not UTF-8")?,
                "--locked",
                "--root",
                stage.to_str().ok_or("staging path is not UTF-8")?,
                "--target-dir",
                target.to_str().ok_or("target path is not UTF-8")?,
            ],
            &root,
            &layout.state.join("source-build.log"),
        );
        result?;
        let payload = source_payload(&root, &stage.join("bin/among"), &uninstaller, &layout)?;
        let result = install_locked(layout, payload);
        if let Err(error) = fs::remove_dir_all(&stage) {
            eprintln!("Warning: could not remove owned build staging: {error}");
        }
        return result;
    };
    let payload = source_payload(root, &binary, &uninstaller, &layout)?;
    install_locked(layout, payload)
}

pub fn desktop_content(template: &str, binary: &Path) -> Result<Vec<u8>> {
    let binary = binary.to_str().ok_or("desktop path is not UTF-8")?;
    if binary.contains(['\n', '\r']) {
        return Err("Installation path contains a newline".into());
    }
    let mut quoted = binary.replace('%', "%%");
    for character in ['\\', '"', '`', '$'] {
        quoted = quoted.replace(character, &format!("\\{character}"));
    }
    quoted = quoted.replace('\\', "\\\\");
    let mut replacements = 0;
    let result = template
        .lines()
        .map(|line| {
            if line.starts_with("Exec=") {
                replacements += 1;
                format!("Exec=/usr/bin/env \"{quoted}\"")
            } else {
                line.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";
    if replacements != 1 {
        return Err("desktop template requires exactly one Exec entry".into());
    }
    Ok(result.into_bytes())
}

pub fn source_payload(
    root: &Path,
    binary: &Path,
    uninstaller: &Path,
    layout: &Layout,
) -> Result<BTreeMap<String, Payload>> {
    let root = root.canonicalize()?;
    let mut payload = BTreeMap::new();
    for (key, source) in [
        ("binary", binary.to_path_buf()),
        ("uninstaller", uninstaller.to_path_buf()),
        ("icon", root.join(format!("data/{APP_ID}.svg"))),
        ("metadata", root.join(format!("data/{APP_ID}.metainfo.xml"))),
        ("license", root.join("LICENSE")),
        ("notice", root.join("NOTICE")),
    ] {
        let source = checked_absolute(&source)?;
        if !source.is_file() {
            return Err("payload input is not a regular file".into());
        }
        let executable = matches!(key, "binary" | "uninstaller");
        if executable && fs::metadata(&source)?.permissions().mode() & 0o111 == 0 {
            return Err("prebuilt payload is not executable".into());
        }
        let bytes = fs::read(source)?;
        if key == "uninstaller" && !bytes.starts_with(b"\x7fELF") {
            return Err(
                "uninstaller must be a prebuilt native executable, not a legacy launcher".into(),
            );
        }
        payload.insert(
            key.into(),
            Payload {
                bytes,
                mode: if executable { 0o755 } else { 0o644 },
            },
        );
    }
    let template = fs::read_to_string(checked_absolute(
        &root.join(format!("data/{APP_ID}.desktop")),
    )?)?;
    payload.insert(
        "desktop".into(),
        Payload {
            bytes: desktop_content(&template, &layout.destinations["binary"])?,
            mode: 0o644,
        },
    );
    fn licenses(
        root: &Path,
        directory: &Path,
        payload: &mut BTreeMap<String, Payload>,
    ) -> Result<()> {
        checked_absolute(directory)?;
        for entry in fs::read_dir(directory)? {
            let entry = entry?;
            if entry.file_type()?.is_symlink() {
                return Err("third-party payload contains a symlink".into());
            }
            if entry.file_type()?.is_dir() {
                licenses(root, &entry.path(), payload)?;
            } else if entry.file_type()?.is_file() {
                let key = entry
                    .path()
                    .strip_prefix(root)?
                    .to_str()
                    .ok_or("license path is not UTF-8")?
                    .to_string();
                payload.insert(
                    key,
                    Payload {
                        bytes: fs::read(entry.path())?,
                        mode: 0o644,
                    },
                );
            } else {
                return Err("third-party payload contains a nonregular member".into());
            }
        }
        Ok(())
    }
    licenses(&root, &root.join("THIRD_PARTY"), &mut payload)?;
    Ok(payload)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rollback_does_not_rewrite_unchanged_originals_when_parents_become_unwritable() -> Result<()>
    {
        if nix::unistd::geteuid().is_root() {
            return Err("run ownership regression as an unprivileged user".into());
        }
        let base = storage::private_directory(&std::env::temp_dir(), "among-unchanged-rollback")?;
        let layout = Layout::new(&base.join("home"), &base.join("data"), &base.join("state"))?;
        fs::create_dir_all(&layout.state)?;
        let binary = &layout.destinations["binary"];
        write_absolute(binary, b"unchanged-binary", 0o750)?;
        let manifest = layout.state.join("install.json");
        write_absolute(&manifest, b"unchanged-manifest", 0o600)?;
        let binary_parent = binary.parent().ok_or("binary parent absent")?;
        let payload = BTreeMap::from([(
            "binary".into(),
            Payload {
                bytes: b"unchanged-binary".to_vec(),
                mode: 0o750,
            },
        )]);
        let result = transaction(&layout, &payload, || {
            fs::set_permissions(binary_parent, fs::Permissions::from_mode(0o555))?;
            fs::set_permissions(&layout.state, fs::Permissions::from_mode(0o555))?;
            Err("injected late loss of writable storage".into())
        });
        fs::set_permissions(binary_parent, fs::Permissions::from_mode(0o755))?;
        fs::set_permissions(&layout.state, fs::Permissions::from_mode(0o755))?;
        let error = result.err().ok_or("late failure accepted")?;
        assert!(error.to_string().contains("rollback failures: []"));
        assert_eq!(fs::read(binary)?, b"unchanged-binary");
        assert_eq!(fs::metadata(binary)?.permissions().mode() & 0o777, 0o750);
        assert_eq!(fs::read(manifest)?, b"unchanged-manifest");
        assert!(!matches_previous(binary, b"changed", 0o750));
        assert!(!matches_previous(binary, b"unchanged-binary", 0o755));
        fs::remove_dir_all(base)?;
        Ok(())
    }
    #[test]
    fn failed_commit_restores_old_bytes_modes_manifest_and_removed_legacy_helper() -> Result<()> {
        let base =
            std::env::temp_dir().join(format!("among-install-rollback-{}", std::process::id()));
        fs::create_dir(&base)?;
        let layout = Layout::new(&base.join("home"), &base.join("data"), &base.join("state"))?;
        fs::create_dir_all(&layout.state)?;
        for key in ["binary", "installer", "uninstaller"] {
            write_absolute(
                &layout.destinations[key],
                format!("old-{key}").as_bytes(),
                0o750,
            )?;
        }
        write_absolute(&layout.state.join("install.json"), b"old-manifest", 0o600)?;
        let payload = BTreeMap::from([
            (
                "binary".into(),
                Payload {
                    bytes: b"new-binary".to_vec(),
                    mode: 0o755,
                },
            ),
            (
                "icon".into(),
                Payload {
                    bytes: b"new-icon".to_vec(),
                    mode: 0o644,
                },
            ),
        ]);
        let error = transaction(&layout, &payload, || {
            Err("injected precommit failure".into())
        })
        .err()
        .ok_or("injected failure accepted")?;
        assert!(error.to_string().contains("injected precommit failure"));
        assert!(error.to_string().contains("rollback failures: []"));
        for key in ["binary", "installer", "uninstaller"] {
            assert_eq!(
                fs::read(&layout.destinations[key])?,
                format!("old-{key}").as_bytes()
            );
            assert_eq!(
                fs::metadata(&layout.destinations[key])?
                    .permissions()
                    .mode()
                    & 0o777,
                0o750
            );
        }
        assert!(!layout.destinations["icon"].exists());
        assert_eq!(
            fs::read(layout.state.join("install.json"))?,
            b"old-manifest"
        );
        fs::remove_dir_all(base)?;
        Ok(())
    }
}
