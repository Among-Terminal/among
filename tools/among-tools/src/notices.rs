use crate::{process::OwnedProcess, storage, Result};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::io::Read;
use std::path::Path;
use std::time::Duration;

mod supplemental;

pub const MANIFESTS: [&str; 3] = [
    "Cargo.toml",
    "examples/plugins/Cargo.toml",
    "tools/among-tools/Cargo.toml",
];

fn locked_download_identity(root: &Path, manifest: &str, diagnostic: &str) -> Option<String> {
    let identity = diagnostic.lines().find_map(|line| {
        line.strip_prefix("error: failed to download `")?
            .strip_suffix('`')
    })?;
    let (name, version) = identity.split_once(" v")?;
    if name.len() > 64
        || version.len() > 64
        || name.is_empty()
        || version.is_empty()
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"_-".contains(&byte))
        || !version
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b".+-".contains(&byte))
    {
        return None;
    }
    let mut bytes = Vec::new();
    File::open(root.join(manifest).with_file_name("Cargo.lock"))
        .ok()?
        .take(256 * 1024 + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.len() > 256 * 1024 {
        return None;
    }
    let lock = toml::from_str::<toml::Value>(std::str::from_utf8(&bytes).ok()?).ok()?;
    lock.get("package")?.as_array()?.iter().find(|package| {
        package.get("name").and_then(toml::Value::as_str) == Some(name)
            && package.get("version").and_then(toml::Value::as_str) == Some(version)
    })?;
    Some(format!("{name} v{version}"))
}

fn metadata_failure(
    root: &Path,
    base: &Path,
    index: usize,
    manifest: &str,
    status: &std::process::ExitStatus,
) -> Result<String> {
    let stderr = base.join(format!("metadata-{index}.stderr"));
    let mut prefix = Vec::new();
    File::open(&stderr)?.take(8192).read_to_end(&mut prefix)?;
    let text = String::from_utf8_lossy(&prefix);
    // cargo can echo authenticated URLs, local configuration and terminal escapes;
    // only fixed diagnoses and identities already in this locked graph may leave scratch.
    let diagnostic = if text.contains("failed to download") && text.contains("--offline") {
        match locked_download_identity(root, manifest, &text) {
            Some(identity) => {
                format!("offline metadata requires download of locked crate {identity}")
            }
            None => "offline metadata requires an unavailable download".into(),
        }
    } else {
        "Cargo diagnostic withheld; inspect the private raw stderr".into()
    };
    fs::write(
        base.join("metadata-failure.json"),
        serde_json::to_vec_pretty(&json!({
            "schema": 1,
            "graph_index": index,
            "manifest": manifest,
            "status": status.to_string(),
            "stderr_bytes": fs::metadata(stderr)?.len(),
            "diagnostic": diagnostic,
        }))?,
    )?;
    Ok(format!(
        "locked Cargo metadata failed for graph {index} ({manifest}); {status}; {diagnostic}; raw stderr retained privately under the notice check root"
    ))
}

pub fn inventory(
    graphs: &[(&str, Value)],
    root: &Path,
) -> Result<(BTreeMap<String, Vec<u8>>, usize)> {
    let mut supplemental = supplemental::Inputs::load(root)?;
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
        if !package["license"]
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
        let provenance = supplemental.apply(&package, source, &mut outputs, &mut texts)?;
        if texts.is_empty() {
            return Err(format!("missing authentic license evidence: {name}").into());
        }
        texts.sort_by(|a, b| a["path"].as_str().cmp(&b["path"].as_str()));
        texts.dedup();
        let mut record = json!({"name":name,"version":version,"source":package["source"],"license":package["license"],"trees":trees,"texts":texts});
        if let Some(provenance) = provenance {
            record["supplemental"] = provenance;
        }
        records.push(record);
    }
    supplemental.finish()?;
    let count = records.len();
    let mut bytes = serde_json::to_vec_pretty(
        &json!({"schema":1,"scope":"all three locked Cargo graphs, including build and target-specific dependencies","packages":records}),
    )?;
    bytes.push(b'\n');
    outputs.insert("inventory.json".into(), bytes);
    Ok((outputs, count))
}

pub fn run(root: &Path, destination: &Path, check: bool, scratch: &Path) -> Result<()> {
    let base = storage::private_directory(scratch, "among-notices")?;
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
                "--offline",
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
        let status = process.wait(Duration::from_secs(180))?;
        if !status.success() {
            return Err(metadata_failure(root, &base, index, manifest, &status)?.into());
        }
        graphs.push((*manifest, storage::parse_json(&fs::read(output)?)?));
    }
    let (outputs, count) = inventory(&graphs, root)?;
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
        outputs
            .keys()
            .filter(|path| path.starts_with("texts/"))
            .count()
    );
    Ok(())
}
