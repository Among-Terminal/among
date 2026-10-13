use crate::{apt_collect, deb822, platform, process::OwnedProcess, storage, Result};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::OsString,
    fs::{self, File},
    path::Path,
    time::Duration,
};

/// Every source package whose binaries or data an AppImage audit says are
/// bundled. An audit naming nothing is refused: an image with GTK inside
/// always bundles third-party code, so an empty set means a broken audit.
pub fn source_set(audit: &Value) -> Result<BTreeSet<(String, String)>> {
    let mut sources = BTreeSet::new();
    for field in ["bundled_libraries", "bundled_data_packages"] {
        for record in audit[field]
            .as_array()
            .ok_or_else(|| format!("AppImage audit lacks {field}"))?
        {
            let name = record["source_package"]
                .as_str()
                .filter(|value| !value.is_empty())
                .ok_or("AppImage audit record lacks its source package")?;
            let version = record["source_version"]
                .as_str()
                .filter(|value| !value.is_empty())
                .ok_or("AppImage audit record lacks its source version")?;
            sources.insert((name.to_string(), version.to_string()));
        }
    }
    if sources.is_empty() {
        return Err("AppImage audit names no bundled source package".into());
    }
    Ok(sources)
}

/// Source descriptors are named without the epoch: `glib2.0_2.80.0-6ubuntu3.4.dsc`.
pub fn descriptor_name(source: &str, version: &str) -> String {
    let version = version.split_once(':').map_or(version, |(_, rest)| rest);
    format!("{source}_{version}.dsc")
}

/// Checks that the descriptor of one source version and every file it lists
/// are present with the SHA-256 the descriptor states.
pub fn verify_descriptor(directory: &Path, source: &str, version: &str) -> Result<Value> {
    let name = descriptor_name(source, version);
    let descriptor = storage::safe_path(directory, Path::new(&name))?;
    if !descriptor
        .symlink_metadata()
        .is_ok_and(|metadata| metadata.is_file())
    {
        return Err(format!("source descriptor {name} is missing").into());
    }
    let fields = deb822::fields(&fs::read_to_string(&descriptor)?)?;
    if deb822::required(&fields, "Source")? != source
        || deb822::required(&fields, "Version")? != version
    {
        return Err(format!("source descriptor {name} names another source version").into());
    }
    let mut files = vec![json!({"name":name,"sha256":storage::sha256(File::open(&descriptor)?)?})];
    let listed = deb822::required(&fields, "Checksums-Sha256")?;
    for line in listed.lines().filter(|line| !line.trim().is_empty()) {
        let (digest, size, file) = deb822::checksum(line)?;
        let path = storage::safe_path(directory, Path::new(file))?;
        if !path
            .symlink_metadata()
            .is_ok_and(|metadata| metadata.is_file())
            || path.metadata()?.len() != size
            || storage::sha256(File::open(&path)?)? != digest
        {
            return Err(format!("source file {file} of {name} is missing or changed").into());
        }
        files.push(json!({"name":file,"sha256":digest}));
    }
    if files.len() < 2 {
        return Err(format!("source descriptor {name} lists no source files").into());
    }
    Ok(json!({"source_package":source,"source_version":version,"files":files}))
}

/// Downloads the corresponding Debian source packages of everything an AppImage
/// bundles, from the same signed Ubuntu archive the build image installed them
/// from. APT checks each download against the signed source index; the result
/// is re-checked against each descriptor before it is published.
pub fn collect(audit: &Path, output: &Path) -> Result<Value> {
    if !Path::new("/.dockerenv").is_file() || !nix::unistd::geteuid().is_root() {
        return Err(
            "source collection edits APT configuration; run it only as root in the disposable build container"
                .into(),
        );
    }
    let actual = platform::discover()?;
    if actual.os != "ubuntu" || actual.version != "24.04" {
        return Err("AppImage sources come from the Ubuntu 24.04 build image".into());
    }
    let record = storage::parse_json(&fs::read(audit)?)?;
    let sources = source_set(&record)?;
    if output.symlink_metadata().is_ok() {
        return Err("source output directory must be new".into());
    }
    let parent = output
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or(Path::new("."))
        .canonicalize()?;
    let base = storage::private_directory(&parent, "among-appimage-sources")?;
    let payload = base.join("payload");
    fs::create_dir(&payload)?;
    let lists = Path::new("/etc/apt/sources.list.d");
    let mut edited = 0;
    for entry in fs::read_dir(lists)? {
        let entry = entry?;
        if entry
            .path()
            .extension()
            .is_some_and(|extension| extension == "sources")
        {
            let path = storage::safe_path(lists, Path::new(&entry.file_name()))?;
            let text = apt_collect::enable_sources(&fs::read_to_string(&path)?)?;
            storage::atomic_write(lists, Path::new(&entry.file_name()), text.as_bytes(), 0o644)?;
            edited += 1;
        }
    }
    if edited == 0 {
        return Err("Deb822 repository configuration is absent".into());
    }
    let env: BTreeMap<OsString, OsString> = BTreeMap::from([
        ("PATH".into(), "/usr/bin:/bin".into()),
        ("LC_ALL".into(), "C.UTF-8".into()),
        ("DEBIAN_FRONTEND".into(), "noninteractive".into()),
        ("HOME".into(), "/root".into()),
    ]);
    let run = |name: &str, args: &[&str], cwd: &Path, timeout: u64| -> Result<()> {
        let log = base.join(format!("{name}.log"));
        let mut process = OwnedProcess::spawn(
            Path::new("/usr/bin/apt-get"),
            args,
            &env,
            cwd,
            &File::create(&log)?,
        )?;
        if !process.wait(Duration::from_secs(timeout))?.success() {
            eprintln!("{}", fs::read_to_string(&log)?);
            return Err(format!("APT step {name} failed; evidence: {}", log.display()).into());
        }
        Ok(())
    };
    run(
        "update",
        &["update", "-o", "APT::Update::Error-Mode=any"],
        &base,
        600,
    )?;
    let mut records = Vec::new();
    for (source, version) in &sources {
        let request = format!("{source}={version}");
        run(
            &format!("source-{source}"),
            &["source", "--download-only", "--only-source", &request],
            &payload,
            1200,
        )?;
        records.push(verify_descriptor(&payload, source, version)?);
    }
    let document = json!({"schema":1,
        "appimage":record["appimage"],
        "architecture":record["architecture"],
        "scope":"Debian source packages (descriptor, upstream archives, packaging changes) of every bundled library and data package, downloaded through APT from the signed Ubuntu archive",
        "sources":records});
    storage::atomic_write(
        &payload,
        Path::new("sources.json"),
        &serde_json::to_vec_pretty(&document)?,
        0o644,
    )?;
    storage::publish_directory(
        &payload,
        &parent.join(output.file_name().ok_or("output has no name")?),
    )?;
    println!(
        "Collected {} source packages; logs: {}",
        sources.len(),
        base.display()
    );
    Ok(document)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_set_merges_libraries_and_data_and_refuses_gaps() -> Result<()> {
        let audit = json!({"bundled_libraries":[
            {"source_package":"gtk4","source_version":"4.14.5+ds-0ubuntu0.1"},
            {"source_package":"gtk4","source_version":"4.14.5+ds-0ubuntu0.1"}],
            "bundled_data_packages":[{"source_package":"adwaita-icon-theme","source_version":"46.0-1"}]});
        assert_eq!(source_set(&audit)?.len(), 2);
        assert!(source_set(&json!({"bundled_libraries":[],"bundled_data_packages":[]})).is_err());
        assert!(source_set(
            &json!({"bundled_libraries":[{"source_package":"gtk4"}],"bundled_data_packages":[]})
        )
        .is_err());
        assert!(source_set(&json!({"bundled_data_packages":[]})).is_err());
        assert_eq!(
            descriptor_name("glib2.0", "2.80.0-6ubuntu3.4"),
            "glib2.0_2.80.0-6ubuntu3.4.dsc"
        );
        assert_eq!(descriptor_name("libx", "1:2.0-1"), "libx_2.0-1.dsc");
        Ok(())
    }

    #[test]
    fn descriptor_check_refuses_missing_and_changed_source_files() -> Result<()> {
        let root = storage::private_directory(&std::env::temp_dir(), "among-appimage-dsc")?;
        let archive = b"upstream source bytes";
        let digest = storage::sha256(archive.as_slice())?;
        fs::write(root.join("demo_1.0.orig.tar.xz"), archive)?;
        fs::write(
            root.join("demo_1.0-1.dsc"),
            format!("Format: 3.0 (quilt)\nSource: demo\nVersion: 1:1.0-1\nChecksums-Sha256:\n {digest} {} demo_1.0.orig.tar.xz\n", archive.len()),
        )?;
        let record = verify_descriptor(&root, "demo", "1:1.0-1")?;
        assert_eq!(record["files"].as_array().map(Vec::len), Some(2));
        assert!(verify_descriptor(&root, "other", "1:1.0-1").is_err());
        fs::write(root.join("demo_1.0.orig.tar.xz"), b"changed upstream bytes")?;
        assert!(verify_descriptor(&root, "demo", "1:1.0-1").is_err());
        fs::remove_file(root.join("demo_1.0.orig.tar.xz"))?;
        assert!(verify_descriptor(&root, "demo", "1:1.0-1").is_err());
        fs::remove_dir_all(root)?;
        Ok(())
    }
}
