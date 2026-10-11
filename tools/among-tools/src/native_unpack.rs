use crate::{process::OwnedProcess, storage, Result};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::path::Path;
use std::time::Duration;

pub fn run(sources: &Path, output: &Path) -> Result<Value> {
    if !Path::new("/.dockerenv").is_file() {
        return Err("extract third-party source only inside a disposable audit container".into());
    }
    let sources = sources.canonicalize()?;
    let inventory = storage::parse_json(&fs::read(storage::safe_path(
        &sources,
        Path::new("inventory.json"),
    )?)?)?;
    let rows = inventory["sources"]
        .as_array()
        .filter(|rows| !rows.is_empty())
        .ok_or("native source inventory rows absent")?;
    let mut inputs = Vec::new();
    let mut names = BTreeSet::new();
    for row in rows {
        let name = row["source_package"]
            .as_str()
            .ok_or("native source name absent")?;
        if name.is_empty()
            || matches!(name, "." | "..")
            || Path::new(name).file_name().and_then(|name| name.to_str()) != Some(name)
            || !names.insert(name)
        {
            return Err("unsafe or duplicate native source name".into());
        }
        let version = row["source_version"]
            .as_str()
            .ok_or("native source version absent")?;
        let mut descriptors = Vec::new();
        for file in row["files"]
            .as_array()
            .ok_or("native source file inventory absent")?
        {
            let relative = file["path"]
                .as_str()
                .ok_or("native source file path absent")?;
            let path = storage::safe_path(&sources, Path::new(relative))?;
            if !path.is_file() {
                return Err("native source inputs must be regular files".into());
            }
            if relative.ends_with(".dsc") {
                descriptors.push(path);
            }
        }
        if descriptors.len() != 1 {
            return Err("one descriptor per native source required".into());
        }
        inputs.push((name.to_string(), version.to_string(), descriptors.remove(0)));
    }
    fs::create_dir(output)?;
    let output = output.canonicalize()?;
    let env = BTreeMap::from([
        ("PATH".into(), "/usr/bin:/bin".into()),
        ("LC_ALL".into(), "C.UTF-8".into()),
    ]);
    let mut records = Vec::new();
    for (name, version, descriptor) in inputs {
        let target = output.join(&name);
        let log = File::create(output.join(format!("{name}.log")))?;
        let mut process = OwnedProcess::spawn(
            Path::new("/usr/bin/dpkg-source"),
            &[
                "--no-check",
                "-x",
                descriptor.to_str().ok_or("descriptor path is not UTF-8")?,
                target.to_str().ok_or("source target path is not UTF-8")?,
            ],
            &env,
            &output,
            &log,
        )?;
        if !process.wait(Duration::from_secs(300))?.success() {
            return Err(format!(
                "native source extraction failed; evidence: {}",
                output.display()
            )
            .into());
        }
        let mut controls = BTreeMap::new();
        for relative in ["debian/control", "debian/rules"] {
            let path = storage::safe_path(&target, Path::new(relative))?;
            if !path.is_file() {
                return Err(format!("missing Debian build input: {name}/{relative}").into());
            }
            controls.insert(relative, storage::sha256(File::open(path)?)?);
        }
        records.push(json!({"source_package":name,"source_version":version,"extraction":"passed including dpkg-source packaging patches","build_files":controls}));
        let evidence = json!({"schema":1,"sources":records,"scope":"source extraction and Debian build-input presence; not a full rebuild of every native package"});
        storage::atomic_write(
            &output,
            Path::new("unpack-evidence.json"),
            &serde_json::to_vec_pretty(&evidence)?,
            0o644,
        )?;
        println!("PASS dpkg-source {name}={version}");
    }
    Ok(
        json!({"schema":1,"sources":records,"scope":"source extraction and Debian build-input presence; not a full rebuild of every native package"}),
    )
}
