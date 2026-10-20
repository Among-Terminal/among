use crate::{appimage_tools, deb, platform, process::OwnedProcess, storage, Result};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::OsString,
    fs::{self, File},
    io::Read,
    os::unix::fs::{DirBuilderExt, PermissionsExt},
    path::{Path, PathBuf},
    time::Duration,
};

/// Files the application refuses to start without when it runs from an
/// AppImage payload (compiled schemas including the desktop interface schema,
/// the MIME cache, a pixbuf loader cache that can decode SVG icons) plus the
/// symbolic icon theme the header bar draws from.
const RUNTIME_DATA: [&str; 5] = [
    "usr/share/glib-2.0/schemas/gschemas.compiled",
    "usr/share/glib-2.0/schemas/org.gnome.desktop.interface.gschema.xml",
    "usr/share/mime/mime.cache",
    "usr/lib/gdk-pixbuf-2.0/2.10.0/loaders.cache",
    "usr/share/icons/Adwaita/index.theme",
];

/// Project executables. Every other ELF file in the image is a bundled
/// distribution library and must be attributed to a package.
const OWN_EXECUTABLES: [&str; 5] = [
    "AppRun",
    "usr/bin/among",
    "usr/share/among/plugins/clock",
    "usr/share/among/plugins/system-status",
    "usr/share/among/plugins/pane-pwd",
];

/// Distribution data trees copied into the image next to the libraries. Their
/// packages' notices travel with the image just like the libraries' notices.
const DATA_TREES: [&str; 3] = [
    "/usr/share/icons/Adwaita",
    "/usr/share/mime/packages/freedesktop.org.xml",
    "/usr/share/glib-2.0/schemas/org.gnome.desktop.interface.gschema.xml",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Owner {
    pub package: String,
    pub architecture: String,
    pub version: String,
    pub source_package: String,
    pub source_version: String,
}

/// The version an AppImage is named after. A tag build takes the tag without
/// its `v` and must be a release tag whose base version equals the Cargo
/// version, exactly as the Debian candidates require; otherwise a tag such as
/// `v0.2.0` on a 0.1.0 tree would publish a file claiming the wrong release.
/// Any other build is named after the Cargo version.
pub fn candidate_version(root: &Path, ref_type: &str, ref_name: &str) -> Result<String> {
    let cargo = crate::package_version::read(root)?;
    if ref_type != "tag" {
        return Ok(cargo);
    }
    let (base, _) = deb::release_version(ref_name)?;
    if base != cargo {
        return Err(format!("tag {ref_name} does not match the Cargo version {cargo}").into());
    }
    Ok(ref_name
        .strip_prefix('v')
        .ok_or("release tag lacks its v prefix")?
        .to_string())
}

pub fn file_name(version: &str, architecture: &str) -> Result<String> {
    if !matches!(architecture, "amd64" | "arm64") {
        return Err("unsupported AppImage architecture".into());
    }
    if !version.starts_with(|character: char| character.is_ascii_digit())
        || !version
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b".+~-".contains(&byte))
    {
        return Err("AppImage version must start with a digit and use only [0-9A-Za-z.+~-]".into());
    }
    Ok(format!("among-{version}-{architecture}.AppImage"))
}

fn is_elf(path: &Path) -> Result<bool> {
    let mut magic = [0u8; 4];
    let mut file = File::open(path)?;
    let mut read = 0;
    while read < magic.len() {
        let count = file.read(&mut magic[read..])?;
        if count == 0 {
            return Ok(false);
        }
        read += count;
    }
    Ok(&magic == b"\x7fELF")
}

/// Every runtime data file must be a nonempty regular file reached without
/// symlinks, and the pixbuf loader cache must name the SVG loader: without it
/// GTK draws missing-icon placeholders instead of the symbolic header icons.
pub fn require_runtime_data(appdir: &Path) -> Result<()> {
    for relative in RUNTIME_DATA {
        let path = storage::safe_path(appdir, Path::new(relative))?;
        if !path
            .symlink_metadata()
            .is_ok_and(|metadata| metadata.is_file())
            || fs::metadata(&path)?.len() == 0
        {
            return Err(format!("AppImage payload lacks required runtime data {relative}").into());
        }
    }
    let loaders = fs::read_to_string(appdir.join("usr/lib/gdk-pixbuf-2.0/2.10.0/loaders.cache"))?;
    if !loaders.contains("\"libpixbufloader-svg.so\"") {
        return Err("AppImage pixbuf loader cache does not name the SVG loader".into());
    }
    Ok(())
}

/// linuxdeploy renames an existing AppRun to AppRun.wrapped and installs a
/// shell wrapper that sources plugin hooks. The GTK plugin's hook exports
/// GSETTINGS_SCHEMA_DIR, XDG_DATA_DIRS, GTK_THEME and GDK_BACKEND, which would
/// leak into every shell and plugin started from the terminal. The project
/// launcher already initialises those paths privately, so the wrapper and its
/// hooks are removed and the exact reviewed launcher is put back. Anything
/// other than the two known layouts is refused rather than guessed at.
pub fn restore_launcher(appdir: &Path, launcher_sha256: &str) -> Result<()> {
    let launcher = appdir.join("AppRun");
    let wrapped = appdir.join("AppRun.wrapped");
    let hooks = appdir.join("apprun-hooks");
    if wrapped.symlink_metadata().is_ok() {
        if !launcher.symlink_metadata()?.is_file()
            || !fs::read_to_string(&launcher)?.contains("# autogenerated by linuxdeploy")
        {
            return Err("AppRun.wrapped exists but AppRun is not linuxdeploy's wrapper".into());
        }
        if hooks.symlink_metadata().is_ok() {
            if !hooks.symlink_metadata()?.is_dir() {
                return Err("apprun-hooks is not a directory".into());
            }
            for entry in fs::read_dir(&hooks)? {
                let entry = entry?;
                if !entry.file_type()?.is_file()
                    || entry.path().extension().is_none_or(|value| value != "sh")
                {
                    return Err("unexpected member in apprun-hooks".into());
                }
            }
            fs::remove_dir_all(&hooks)?;
        }
        fs::remove_file(&launcher)?;
        fs::rename(&wrapped, &launcher)?;
    } else if hooks.symlink_metadata().is_ok() {
        return Err("apprun-hooks exists without a wrapped launcher".into());
    }
    if !launcher.symlink_metadata()?.is_file()
        || storage::sha256(File::open(&launcher)?)? != launcher_sha256
    {
        return Err("AppImage launcher differs from the staged reviewed launcher".into());
    }
    Ok(())
}

/// The GTK plugin also copies GTK print backends and GObject introspection
/// typelibs. Only the plugin's hook (removed above) would point GTK and
/// GObject at them, so the bundled GTK never loads them; the print backends
/// cannot even resolve `libgtk-4.so.1` from their directory, which the
/// clean-host linkage check reported on every host. Shipping them would add
/// unreachable code whose licences and sources still had to be delivered.
pub fn remove_unreachable_modules(appdir: &Path) -> Result<()> {
    for relative in ["usr/lib/gtk-4.0", "usr/lib/girepository-1.0"] {
        let path = storage::safe_path(appdir, Path::new(relative))?;
        match path.symlink_metadata() {
            Ok(metadata) if metadata.is_dir() => fs::remove_dir_all(&path)?,
            Ok(_) => return Err(format!("{relative} is not a directory").into()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

fn walk(root: &Path, relative: &Path, files: &mut Vec<(PathBuf, bool)>) -> Result<()> {
    let path = if relative.as_os_str().is_empty() {
        root.to_path_buf()
    } else {
        root.join(relative)
    };
    let mut entries = fs::read_dir(&path)?.collect::<std::io::Result<Vec<_>>>()?;
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let child = relative.join(entry.file_name());
        let kind = entry.file_type()?;
        if kind.is_dir() {
            walk(root, &child, files)?;
        } else if kind.is_symlink() {
            files.push((child, true));
        } else if kind.is_file() {
            files.push((child, false));
        } else {
            return Err(format!("special file in AppImage payload: {}", child.display()).into());
        }
    }
    Ok(())
}

/// Lists every bundled ELF library with its digest, refusing links that leave
/// the payload and ELF files outside `usr/lib` that are not project executables.
pub fn bundled_libraries(appdir: &Path) -> Result<BTreeMap<String, String>> {
    let root = appdir.canonicalize()?;
    let mut files = Vec::new();
    walk(&root, Path::new(""), &mut files)?;
    let mut libraries = BTreeMap::new();
    for (relative, link) in files {
        let text = relative
            .to_str()
            .ok_or("payload path is not UTF-8")?
            .to_string();
        let path = root.join(&relative);
        if link {
            let target = path
                .canonicalize()
                .map_err(|_| format!("dangling link in AppImage payload: {text}"))?;
            if !target.starts_with(&root) {
                return Err(format!("link leaves the AppImage payload: {text}").into());
            }
            continue;
        }
        if !is_elf(&path)? || OWN_EXECUTABLES.contains(&text.as_str()) {
            continue;
        }
        if !text.starts_with("usr/lib/") {
            return Err(format!("unexpected ELF file outside usr/lib: {text}").into());
        }
        libraries.insert(text, storage::sha256(File::open(&path)?)?);
    }
    Ok(libraries)
}

/// Attributes every bundled library to the package that installed it in the
/// build environment. An unattributed library stops the build by name: its
/// licence and source cannot be delivered, so it must not ship.
pub fn attribute(
    libraries: &BTreeMap<String, String>,
    owner: &mut dyn FnMut(&str) -> Result<Option<Owner>>,
) -> Result<Vec<Value>> {
    let mut records = Vec::new();
    for (path, digest) in libraries {
        let relative = path
            .strip_prefix("usr/lib/")
            .ok_or("bundled library outside usr/lib")?;
        let found = owner(relative)?
            .ok_or_else(|| format!("bundled library {path} has no owning package"))?;
        records.push(json!({"path":path,"sha256":digest,"package":found.package,
            "architecture":found.architecture,"version":found.version,
            "source_package":found.source_package,"source_version":found.source_version,
            "copyright":format!("usr/share/doc/{}/copyright",found.package)}));
    }
    Ok(records)
}

/// Libraries linuxdeploy refused to bundle because AppImage expects every host
/// to provide them. The installation guide's host requirements come from here.
pub fn host_libraries(log: &str) -> BTreeSet<String> {
    log.lines()
        .filter_map(|line| {
            line.split("Skipping deployment of blacklisted library ")
                .nth(1)
        })
        .filter_map(|path| path.trim().rsplit('/').next())
        .filter(|name| !name.is_empty())
        .map(str::to_string)
        .collect()
}

struct Commands {
    logs: PathBuf,
    counter: usize,
}

impl Commands {
    fn run(
        &mut self,
        name: &str,
        program: &Path,
        args: &[&str],
        env: &BTreeMap<OsString, OsString>,
        cwd: &Path,
        timeout: u64,
    ) -> Result<String> {
        self.counter += 1;
        let log = self.logs.join(format!("{:02}-{name}.log", self.counter));
        let mut process = OwnedProcess::spawn(program, args, env, cwd, &File::create(&log)?)?;
        if !process.wait(Duration::from_secs(timeout))?.success() {
            eprintln!("{}", fs::read_to_string(&log)?);
            return Err(format!(
                "AppImage build step {name} failed; evidence: {}",
                log.display()
            )
            .into());
        }
        Ok(fs::read_to_string(&log)?)
    }
}

fn dpkg_owner(
    commands: &mut Commands,
    env: &BTreeMap<OsString, OsString>,
    triplet: &str,
    relative: &str,
) -> Result<Option<Owner>> {
    for candidate in [
        format!("/usr/lib/{triplet}/{relative}"),
        format!("/lib/{triplet}/{relative}"),
    ] {
        if Path::new(&candidate).symlink_metadata().is_err() {
            continue;
        }
        let log = commands.logs.join("dpkg-query-search.log");
        let mut process = OwnedProcess::spawn(
            Path::new("/usr/bin/dpkg-query"),
            &["--search", &candidate],
            env,
            Path::new("/"),
            &File::create(&log)?,
        )?;
        if !process.wait(Duration::from_secs(60))?.success() {
            continue;
        }
        let text = fs::read_to_string(&log)?;
        let lines = text
            .lines()
            .filter(|line| !line.trim().is_empty())
            .collect::<Vec<_>>();
        let [line] = lines.as_slice() else {
            return Err(format!("ambiguous package ownership for {candidate}").into());
        };
        let (packages, _) = line
            .split_once(": ")
            .ok_or("malformed dpkg-query search output")?;
        if packages.contains(',') {
            return Err(format!("several packages own {candidate}").into());
        }
        let fields = commands.run(
            "dpkg-query-show",
            Path::new("/usr/bin/dpkg-query"),
            &[
                "--show",
                "--showformat=${Package}\t${Architecture}\t${Version}\t${source:Package}\t${source:Version}",
                packages.trim(),
            ],
            env,
            Path::new("/"),
            60,
        )?;
        let parts = fields.trim().split('\t').collect::<Vec<_>>();
        let [package, architecture, version, source_package, source_version] = parts.as_slice()
        else {
            return Err("malformed dpkg-query package fields".into());
        };
        if [
            package,
            architecture,
            version,
            source_package,
            source_version,
        ]
        .iter()
        .any(|value| value.is_empty())
        {
            return Err("incomplete dpkg-query package fields".into());
        }
        return Ok(Some(Owner {
            package: package.to_string(),
            architecture: architecture.to_string(),
            version: version.to_string(),
            source_package: source_package.to_string(),
            source_version: source_version.to_string(),
        }));
    }
    Ok(None)
}

fn copy_tree(source: &Path, destination: &Path) -> Result<()> {
    let metadata = source.symlink_metadata()?;
    if metadata.is_dir() {
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o755)
            .create(destination)?;
        let mut entries = fs::read_dir(source)?.collect::<std::io::Result<Vec<_>>>()?;
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            copy_tree(&entry.path(), &destination.join(entry.file_name()))?;
        }
    } else if metadata.is_file() {
        if destination.symlink_metadata().is_ok() {
            return Err(format!("refusing to replace {}", destination.display()).into());
        }
        fs::copy(source, destination)?;
        let mode = metadata.permissions().mode() & 0o755;
        fs::set_permissions(destination, fs::Permissions::from_mode(mode | 0o644))?;
    }
    // Links in distribution data trees (the cursor aliases of the icon theme)
    // are not copied: the host's cursor theme is used and a copied link could
    // point outside the payload.
    Ok(())
}

fn add_runtime_data(
    commands: &mut Commands,
    env: &BTreeMap<OsString, OsString>,
    appdir: &Path,
) -> Result<()> {
    copy_tree(
        Path::new("/usr/share/icons/Adwaita"),
        &appdir.join("usr/share/icons/Adwaita"),
    )?;
    let packages = appdir.join("usr/share/mime/packages");
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o755)
        .create(&packages)?;
    copy_tree(
        Path::new("/usr/share/mime/packages/freedesktop.org.xml"),
        &packages.join("freedesktop.org.xml"),
    )?;
    let mime = appdir.join("usr/share/mime");
    commands.run(
        "update-mime-database",
        Path::new("/usr/bin/update-mime-database"),
        &[mime.to_str().ok_or("MIME path is not UTF-8")?],
        env,
        appdir,
        300,
    )?;
    Ok(())
}

fn data_packages(
    commands: &mut Commands,
    env: &BTreeMap<OsString, OsString>,
) -> Result<Vec<Owner>> {
    let mut owners = Vec::new();
    for path in DATA_TREES {
        let probe = if Path::new(path).is_dir() {
            format!("{path}/index.theme")
        } else {
            path.to_string()
        };
        let output = commands.run(
            "dpkg-query-data",
            Path::new("/usr/bin/dpkg-query"),
            &["--search", &probe],
            env,
            Path::new("/"),
            60,
        )?;
        let package = output
            .trim()
            .split_once(": ")
            .map(|(package, _)| package.to_string())
            .filter(|package| !package.contains(','))
            .ok_or("runtime data tree has no single owning package")?;
        let fields = commands.run(
            "dpkg-query-show",
            Path::new("/usr/bin/dpkg-query"),
            &[
                "--show",
                "--showformat=${Package}\t${Architecture}\t${Version}\t${source:Package}\t${source:Version}",
                &package,
            ],
            env,
            Path::new("/"),
            60,
        )?;
        let parts = fields.trim().split('\t').collect::<Vec<_>>();
        let [package, architecture, version, source_package, source_version] = parts.as_slice()
        else {
            return Err("malformed dpkg-query package fields".into());
        };
        owners.push(Owner {
            package: package.to_string(),
            architecture: architecture.to_string(),
            version: version.to_string(),
            source_package: source_package.to_string(),
            source_version: source_version.to_string(),
        });
    }
    Ok(owners)
}

/// Copies each attributed package's copyright file and the common licence
/// texts those files refer to into the image, so the notices ship with the
/// binaries they cover.
fn install_notices(appdir: &Path, packages: &BTreeSet<String>) -> Result<()> {
    for package in packages {
        let source = Path::new("/usr/share/doc").join(package).join("copyright");
        let bytes = fs::read(&source)
            .map_err(|_| format!("bundled package {package} has no copyright notice"))?;
        if bytes.is_empty() {
            return Err(format!("bundled package {package} has an empty copyright notice").into());
        }
        let directory = appdir.join("usr/share/doc").join(package);
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o755)
            .create(&directory)?;
        storage::atomic_write(&directory, Path::new("copyright"), &bytes, 0o644)?;
    }
    let licenses = appdir.join("usr/share/common-licenses");
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o755)
        .create(&licenses)?;
    let mut entries =
        fs::read_dir("/usr/share/common-licenses")?.collect::<std::io::Result<Vec<_>>>()?;
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        // Aliases such as GPL -> GPL-3 are links; their text is the target's.
        let bytes = fs::read(entry.path())?;
        storage::atomic_write(&licenses, Path::new(&entry.file_name()), &bytes, 0o644)?;
    }
    Ok(())
}

fn staged_identity(appdir: &Path, architecture: &str) -> Result<String> {
    let record = storage::parse_json(&fs::read(storage::safe_path(
        appdir,
        Path::new("owned-payload.json"),
    )?)?)?;
    if record["schema"] != 1 || record["architecture"] != architecture {
        return Err("staged AppDir record differs from the requested architecture".into());
    }
    let launcher = record["launcher_sha256"]
        .as_str()
        .ok_or("staged launcher digest absent")?;
    if storage::sha256(File::open(storage::safe_path(
        appdir,
        Path::new("AppRun"),
    )?)?)?
        != launcher
        || storage::sha256(File::open(storage::safe_path(
            appdir,
            Path::new("usr/bin/among"),
        )?)?)?
            != record["app_sha256"]
                .as_str()
                .ok_or("staged app digest absent")?
    {
        return Err("staged AppDir changed after staging".into());
    }
    Ok(launcher.to_string())
}

/// Builds one AppImage from a staged AppDir and fetched pinned helpers inside
/// the Ubuntu 24.04 build environment, audits the deployed libraries, and
/// re-checks the final image by extracting it. Nothing is written to `output`
/// unless every step succeeds.
pub fn build(
    staged: &Path,
    fetched: &Path,
    architecture: &str,
    version: &str,
    output: &Path,
) -> Result<PathBuf> {
    let name = file_name(version, architecture)?;
    if output.symlink_metadata().is_ok() {
        return Err("AppImage output directory must be new".into());
    }
    let actual = platform::discover()?;
    if (
        actual.os.as_str(),
        actual.version.as_str(),
        actual.architecture.as_str(),
    ) != ("ubuntu", "24.04", architecture)
    {
        return Err(
            "AppImage builds run natively in the Ubuntu 24.04 build image; cross-architecture relabelling is refused"
                .into(),
        );
    }
    let launcher_sha256 = staged_identity(staged, architecture)?;
    let parent = output
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or(Path::new("."))
        .canonicalize()?;
    let base = storage::private_directory(&parent, "among-appimage-build")?;
    let logs = base.join("logs");
    let helpers = base.join("helpers");
    let temporary = base.join("tmp");
    let home = base.join("home");
    for directory in [&logs, &helpers, &temporary, &home] {
        fs::DirBuilder::new().mode(0o700).create(directory)?;
    }
    let tools = appimage_tools::verified_copy(architecture, fetched, &helpers)?;
    let appdir = base.join("Among.AppDir");
    copy_tree(&staged.canonicalize()?, &appdir)?;
    fs::set_permissions(&appdir, fs::Permissions::from_mode(0o755))?;
    let metainfo = appdir.join("usr/share/metainfo/com.github.among-terminal.metainfo.xml");
    crate::release_metadata::write_package_metainfo(&metainfo, version, &metainfo)?;
    let mut commands = Commands { logs, counter: 0 };
    let system = BTreeMap::from([
        ("PATH".into(), "/usr/bin:/bin".into()),
        ("LC_ALL".into(), "C.UTF-8".into()),
        ("HOME".into(), home.clone().into_os_string()),
        ("TMPDIR".into(), temporary.clone().into_os_string()),
    ]);
    let triplet = commands
        .run(
            "dpkg-architecture",
            Path::new("/usr/bin/dpkg-architecture"),
            &["--query", "DEB_HOST_MULTIARCH"],
            &system,
            &base,
            60,
        )?
        .trim()
        .to_string();
    add_runtime_data(&mut commands, &system, &appdir)?;
    // The helpers are AppImages themselves; build containers have no FUSE, so
    // the runtime's documented extract-and-run mode is used for them.
    let mut deploy = system.clone();
    let path = format!(
        "{}:/usr/bin:/bin",
        helpers.to_str().ok_or("helper path is not UTF-8")?
    );
    deploy.insert("PATH".into(), path.into());
    deploy.insert("APPIMAGE_EXTRACT_AND_RUN".into(), "1".into());
    deploy.insert("DEPLOY_GTK_VERSION".into(), "4".into());
    let appdir_text = appdir.to_str().ok_or("AppDir path is not UTF-8")?;
    let deployment = commands.run(
        "linuxdeploy",
        &helpers.join("linuxdeploy.AppImage"),
        &["--appdir", appdir_text, "--plugin", "gtk"],
        &deploy,
        &base,
        1800,
    )?;
    restore_launcher(&appdir, &launcher_sha256)?;
    crate::appimage::verify_raster_icons(&appdir)?;
    remove_unreachable_modules(&appdir)?;
    require_runtime_data(&appdir)?;
    let libraries = bundled_libraries(&appdir)?;
    let audit_env = system.clone();
    let mut lookup = |relative: &str| dpkg_owner(&mut commands, &audit_env, &triplet, relative);
    let attributed = attribute(&libraries, &mut lookup)?;
    let data = data_packages(&mut commands, &system)?;
    let packages = attributed
        .iter()
        .filter_map(|record| record["package"].as_str().map(str::to_string))
        .chain(data.iter().map(|owner| owner.package.clone()))
        .collect::<BTreeSet<_>>();
    install_notices(&appdir, &packages)?;
    let hosted = host_libraries(&deployment);
    let product = base.join("product");
    fs::DirBuilder::new().mode(0o755).create(&product)?;
    let image = product.join(&name);
    let mut pack = system.clone();
    pack.insert("APPIMAGE_EXTRACT_AND_RUN".into(), "1".into());
    pack.insert(
        "ARCH".into(),
        match architecture {
            "amd64" => "x86_64",
            _ => "aarch64",
        }
        .into(),
    );
    // AppStream data is validated strictly by the package builds; appimagetool
    // would otherwise need its own appstreamcli and network access.
    commands.run(
        "appimagetool",
        &helpers.join("appimagetool.AppImage"),
        &[
            "--runtime-file",
            helpers
                .join("runtime")
                .to_str()
                .ok_or("runtime path is not UTF-8")?,
            "--no-appstream",
            appdir_text,
            image.to_str().ok_or("image path is not UTF-8")?,
        ],
        &pack,
        &base,
        1800,
    )?;
    deb::elf_architecture(&image, architecture)?;
    // The checks above ran on the AppDir; repeat them on what is actually
    // inside the image so that packing cannot drop or alter anything unseen.
    let extracted = base.join("extracted");
    fs::DirBuilder::new().mode(0o700).create(&extracted)?;
    commands.run(
        "extract-final-image",
        &image,
        &["--appimage-extract"],
        &system,
        &extracted,
        600,
    )?;
    let final_root = extracted.join("squashfs-root");
    restore_launcher(&final_root, &launcher_sha256)?;
    crate::appimage::verify_raster_icons(&final_root)?;
    require_runtime_data(&final_root)?;
    if bundled_libraries(&final_root)? != libraries {
        return Err("final AppImage libraries differ from the audited AppDir".into());
    }
    for package in &packages {
        if !final_root
            .join("usr/share/doc")
            .join(package)
            .join("copyright")
            .is_file()
        {
            return Err(format!("final AppImage lacks the notice of {package}").into());
        }
    }
    let digest = storage::sha256(File::open(&image)?)?;
    let runtime_help = commands.run(
        "runtime-version",
        &image,
        &["--appimage-version"],
        &system,
        &extracted,
        60,
    )?;
    let runtime_version = runtime_help
        .lines()
        .find_map(|line| line.strip_prefix("AppImage runtime version: "))
        .ok_or("pinned runtime did not report its version")?
        .trim()
        .to_string();
    let audit = json!({
        "schema":1,
        "appimage":{"filename":name,"sha256":digest,"bytes":image.metadata()?.len()},
        "architecture":architecture,
        "version":version,
        "build_environment":{"os":actual.os,"version":actual.version,"multiarch":triplet},
        "launcher_sha256":launcher_sha256,
        "helpers":tools,
        "runtime_version":runtime_version,
        "bundled_libraries":attributed,
        "bundled_data_packages":data.iter().map(|owner| json!({"package":owner.package,
            "architecture":owner.architecture,"version":owner.version,
            "source_package":owner.source_package,"source_version":owner.source_version,
            "copyright":format!("usr/share/doc/{}/copyright",owner.package)})).collect::<Vec<_>>(),
        "host_libraries":hosted,
        "scope":"library ownership comes from the build image's dpkg database; linuxdeploy rewrites RUNPATH, so bundled bytes are not compared with package bytes; corresponding source is collected separately"
    });
    let audit_name = format!("among-{version}-{architecture}.audit.json");
    storage::atomic_write(
        &product,
        Path::new(&audit_name),
        &serde_json::to_vec_pretty(&audit)?,
        0o644,
    )?;
    storage::atomic_write(
        &product,
        Path::new("SHA256SUMS"),
        format!("{digest}  {name}\n").as_bytes(),
        0o644,
    )?;
    fs::set_permissions(&image, fs::Permissions::from_mode(0o755))?;
    storage::publish_directory(
        &product,
        &parent.join(output.file_name().ok_or("output has no name")?),
    )?;
    fs::remove_dir_all(&appdir)?;
    fs::remove_dir_all(&extracted)?;
    fs::remove_dir_all(&helpers)?;
    println!(
        "Built {name} ({digest}); clean-host checks are separate. Build logs: {}",
        commands.logs.display()
    );
    Ok(commands.logs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn appimage_names_carry_version_and_architecture_and_refuse_others() -> Result<()> {
        assert_eq!(file_name("0.1.0", "amd64")?, "among-0.1.0-amd64.AppImage");
        assert_eq!(
            file_name("0.1.0-rc.10", "arm64")?,
            "among-0.1.0-rc.10-arm64.AppImage"
        );
        for (version, architecture) in [
            ("0.1.0", "x86_64"),
            ("0.1.0", "universal"),
            ("v0.1.0", "amd64"),
            ("0.1.0/../x", "amd64"),
            ("", "amd64"),
        ] {
            assert!(
                file_name(version, architecture).is_err(),
                "{version} {architecture}"
            );
        }
        Ok(())
    }

    #[test]
    fn tag_builds_must_match_the_cargo_version_and_other_builds_use_it() -> Result<()> {
        let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
        let cargo = crate::package_version::read(&root)?;
        assert_eq!(candidate_version(&root, "branch", "main")?, cargo);
        assert_eq!(
            candidate_version(&root, "tag", &format!("v{cargo}"))?,
            cargo
        );
        assert_eq!(
            candidate_version(&root, "tag", &format!("v{cargo}-rc.3"))?,
            format!("{cargo}-rc.3")
        );
        for tag in [
            "v999.0.0",
            "v999.0.0-rc.1",
            cargo.as_str(),
            &format!("v{cargo}-rc.0"),
            "nightly",
        ] {
            assert!(candidate_version(&root, "tag", tag).is_err(), "{tag}");
        }
        Ok(())
    }

    #[test]
    fn host_library_list_comes_from_linuxdeploy_exclusions() {
        let log = "Deploying shared library /lib/x86_64-linux-gnu/libgtk-4.so.1\n\
Skipping deployment of blacklisted library /lib/x86_64-linux-gnu/libX11.so.6\n\
[gtk/stdout] Skipping deployment of blacklisted library /lib/x86_64-linux-gnu/libc.so.6 \n\
Skipping deployment of blacklisted library /lib/x86_64-linux-gnu/libX11.so.6\n";
        assert_eq!(
            host_libraries(log).into_iter().collect::<Vec<_>>(),
            vec!["libX11.so.6".to_string(), "libc.so.6".to_string()]
        );
    }
}
