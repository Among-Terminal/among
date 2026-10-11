use crate::{process::OwnedProcess, storage, Result};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub const MANIFESTS: [&str; 3] = [
    "Cargo.toml",
    "examples/plugins/Cargo.toml",
    "tools/among-tools/Cargo.toml",
];
pub fn inventory(graphs: &[(&str, Value)]) -> Result<(BTreeMap<String, Vec<u8>>, usize)> {
    let mut packages: BTreeMap<String, (Value, BTreeSet<String>)> = BTreeMap::new();
    for (manifest, graph) in graphs {
        if !MANIFESTS.contains(manifest) {
            return Err("unknown Cargo notice graph".into());
        }
        for package in graph["packages"]
            .as_array()
            .ok_or("Cargo metadata packages absent")?
        {
            if package["source"].is_null() {
                if package["license"] != "Apache-2.0" {
                    return Err("original package license must be Apache-2.0".into());
                }
                continue;
            }
            let id = package["id"].as_str().ok_or("Cargo package id absent")?;
            let entry = packages
                .entry(id.into())
                .or_insert_with(|| (package.clone(), BTreeSet::new()));
            if entry.0 != *package {
                return Err("conflicting Cargo package evidence across locked graphs".into());
            }
            entry.1.insert((*manifest).into());
        }
    }
    let mut packages = packages.into_values().collect::<Vec<_>>();
    packages.sort_by(|left, right| {
        (left.0["name"].as_str(), left.0["version"].as_str())
            .cmp(&(right.0["name"].as_str(), right.0["version"].as_str()))
    });
    let mut outputs: BTreeMap<String, Vec<u8>> = BTreeMap::new();
    let mut records = Vec::new();
    for (package, trees) in packages {
        let name = package["name"].as_str().ok_or("package name absent")?;
        let version = package["version"]
            .as_str()
            .ok_or("package version absent")?;
        if name.is_empty()
            || version.is_empty()
            || ![name, version].into_iter().all(|value| {
                value
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"._+-".contains(&byte))
            })
        {
            return Err("unsafe Cargo notice package identity".into());
        }
        let manifest = Path::new(
            package["manifest_path"]
                .as_str()
                .ok_or("Cargo package manifest path absent")?,
        );
        let source = manifest
            .parent()
            .ok_or("Cargo package source directory absent")?;
        let mut files = BTreeSet::new();
        for entry in fs::read_dir(source)? {
            let entry = entry?;
            let upper = entry.file_name().to_string_lossy().to_uppercase();
            if entry.path().is_file()
                && ["LICENSE", "LICENCE", "COPYING", "COPYRIGHT", "NOTICE"]
                    .iter()
                    .any(|prefix| upper.starts_with(prefix))
            {
                files.insert(entry.path());
            }
        }
        if let Some(relative) = package["license_file"].as_str() {
            files.insert(storage::safe_path(source, Path::new(relative))?);
        }
        if files.is_empty()
            || !package["license"]
                .as_str()
                .is_some_and(|license| !license.is_empty())
        {
            return Err(format!("missing authentic license evidence: {name}").into());
        }
        let mut texts = Vec::new();
        for file in files {
            let filename = file.file_name().ok_or("license filename absent")?;
            let safe = storage::safe_path(source, file.strip_prefix(source)?)?;
            if !safe.is_file() {
                return Err("license evidence must be a regular file".into());
            }
            let relative = format!(
                "texts/{name}-{version}/{}",
                filename.to_str().ok_or("license name is not UTF-8")?
            );
            let bytes = fs::read(safe)?;
            if outputs
                .get(&relative)
                .is_some_and(|previous| previous != &bytes)
            {
                return Err("conflicting package license texts".into());
            }
            let digest = storage::sha256(bytes.as_slice())?;
            outputs.insert(relative.clone(), bytes);
            texts.push(json!({"path":relative,"sha256":digest}));
        }
        records.push(json!({"name":name,"version":version,"source":package["source"],"license":package["license"],"trees":trees,"texts":texts}));
    }
    let count = records.len();
    let mut bytes = serde_json::to_vec_pretty(
        &json!({"schema":1,"scope":"all three locked Cargo graphs, including build and target-specific dependencies","packages":records}),
    )?;
    bytes.push(b'\n');
    outputs.insert("inventory.json".into(), bytes);
    Ok((outputs, count))
}

pub fn run(root: &Path, destination: &Path, check: bool, scratch: &Path) -> Result<()> {
    let base = scratch.join(format!(
        "among-notices-{}-{}",
        std::process::id(),
        SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos()
    ));
    fs::create_dir(&base)?;
    let input = std::env::vars_os().collect();
    let env = crate::process::allowed_environment(
        &input,
        &[
            "PATH",
            "HOME",
            "CARGO_HOME",
            "RUSTUP_HOME",
            "CARGO_BUILD_JOBS",
        ],
    );
    let mut graphs = Vec::new();
    let path = env
        .get(&std::ffi::OsString::from("PATH"))
        .ok_or("Cargo PATH absent")?;
    let cargo = std::env::split_paths(path)
        .map(|directory| directory.join("cargo"))
        .find(|path| path.is_file())
        .ok_or("Cargo unavailable for locked source notice inventory")?;
    for (index, manifest) in MANIFESTS.iter().enumerate() {
        let output = base.join(format!("metadata-{index}.json"));
        let log = File::create(&output)?;
        let errors = File::create(base.join(format!("metadata-{index}.stderr")))?;
        let mut process = OwnedProcess::spawn_split(
            &cargo,
            &[
                "metadata",
                "--locked",
                "--format-version",
                "1",
                "--manifest-path",
                manifest,
            ],
            &env,
            root,
            &log,
            &errors,
        )?;
        if !process.wait(Duration::from_secs(180))?.success() {
            return Err(
                format!("locked Cargo metadata failed; evidence: {}", base.display()).into(),
            );
        }
        graphs.push((*manifest, storage::parse_json(&fs::read(output)?)?));
    }
    let (outputs, count) = inventory(&graphs)?;
    for (relative, bytes) in &outputs {
        let path = storage::safe_path(destination, Path::new(relative))?;
        if check {
            if !path.is_file() || fs::read(path)? != *bytes {
                return Err(format!("missing or stale third-party evidence: {relative}").into());
            }
        } else {
            fs::create_dir_all(path.parent().ok_or("notice output parent absent")?)?;
            storage::atomic_write(destination, Path::new(relative), bytes, 0o644)?;
        }
    }
    println!(
        "{} {count} dependency records and {} verbatim texts from three locked graphs",
        if check { "Checked" } else { "Generated" },
        outputs.len() - 1
    );
    Ok(())
}
