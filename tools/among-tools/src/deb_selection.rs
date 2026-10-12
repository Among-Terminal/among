use crate::{deb, process::OwnedProcess, storage, Result};
use std::collections::BTreeMap;
use std::fs::{self, File};
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::Duration;

fn inventory(
    root: &Path,
    relative: &Path,
    files: &mut BTreeMap<PathBuf, (String, u32)>,
) -> Result<()> {
    for entry in fs::read_dir(root.join(relative))? {
        let entry = entry?;
        let relative = relative.join(entry.file_name());
        let path = storage::safe_path(root, &relative)?;
        let metadata = fs::metadata(&path)?;
        if metadata.is_dir() {
            inventory(root, &relative, files)?;
        } else if metadata.is_file() {
            files.insert(
                relative,
                (
                    storage::sha256(File::open(path)?)?,
                    metadata.permissions().mode() & 0o7777,
                ),
            );
        } else {
            return Err("nonregular debhelper payload".into());
        }
    }
    Ok(())
}

pub fn require_equal(staged: &Path, installed: &Path) -> Result<usize> {
    let mut source = BTreeMap::new();
    let mut selected = BTreeMap::new();
    inventory(staged, Path::new(""), &mut source)?;
    inventory(installed, Path::new(""), &mut selected)?;
    // debhelper owns its control directory; it is not a user payload file.
    selected.retain(|path, _| !path.starts_with("DEBIAN"));
    if source.is_empty() || source != selected {
        return Err("debhelper changed, added or omitted staged payload bytes/modes".into());
    }
    Ok(source.len())
}

fn copy_recipe(root: &Path, relative: &Path, destination: &Path) -> Result<()> {
    for entry in fs::read_dir(root.join(relative))? {
        let entry = entry?;
        let relative = relative.join(entry.file_name());
        let source = storage::safe_path(root, &relative)?;
        let target = destination.join(&relative);
        let metadata = fs::metadata(&source)?;
        if metadata.is_dir() {
            fs::create_dir(&target)?;
            copy_recipe(root, &relative, destination)?;
        } else if metadata.is_file() {
            storage::atomic_write(
                destination,
                &relative,
                &fs::read(&source)?,
                metadata.permissions().mode() & 0o777,
            )?;
        } else {
            return Err("unsafe Debian recipe input".into());
        }
    }
    Ok(())
}

pub fn run(root: &Path, binaries: &Path, scratch: &Path) -> Result<PathBuf> {
    let base = storage::private_directory(scratch, "among-dh")?;
    let recipe = storage::safe_path(root, Path::new("packaging/debian"))?;
    fs::create_dir(base.join("debian"))?;
    copy_recipe(&recipe, Path::new(""), &base.join("debian"))?;
    fs::set_permissions(base.join("debian/rules"), fs::Permissions::from_mode(0o755))?;
    let staged = base.join("debian/tmp");
    let count = deb::stage(root, binaries, &staged)?;
    let home = base.join("home");
    fs::DirBuilder::new().mode(0o700).create(&home)?;
    let environment = [
        ("PATH", "/usr/bin:/bin".into()),
        ("HOME", home.into_os_string()),
        ("LC_ALL", "C.UTF-8".into()),
    ]
    .into_iter()
    .map(|(key, value)| (key.into(), value))
    .collect();
    for (program, args, name) in [
        ("/usr/bin/dh_install", vec![], "dh-install.log"),
        (
            "/usr/bin/dh_missing",
            vec!["--fail-missing"],
            "dh-missing.log",
        ),
        (
            "/usr/bin/desktop-file-validate",
            vec!["debian/among-terminal/usr/share/applications/com.github.among-terminal.desktop"],
            "desktop.log",
        ),
    ] {
        let log = File::create(base.join(name))?;
        let mut process =
            OwnedProcess::spawn(Path::new(program), &args, &environment, &base, &log)?;
        if !process.wait(Duration::from_secs(60))?.success() {
            return Err(format!(
                "debhelper selection/metadata check failed; evidence: {}",
                base.display()
            )
            .into());
        }
    }
    if require_equal(&staged, &base.join("debian/among-terminal"))? != count {
        return Err("staged count differs from selected inventory".into());
    }
    storage::atomic_write(
        &base,
        Path::new("selection.json"),
        &serde_json::to_vec_pretty(
            &serde_json::json!({"files":count,"status":"passed","scope":"debhelper-file-selection-only","package_built":false,"package_installed":false}),
        )?,
        0o600,
    )?;
    println!("PASS real debhelper selection: {count} exact files, modes and desktop metadata\nEvidence: {}", base.display());
    Ok(base)
}
