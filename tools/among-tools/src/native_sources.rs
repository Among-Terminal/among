use crate::{deb822, storage, Result};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::path::{Path, PathBuf};

pub fn regular(root: &Path, relative: &str) -> Result<PathBuf> {
    let path = storage::safe_path(root, Path::new(relative))?;
    if !path.is_file() {
        return Err("missing regular native source member".into());
    }
    Ok(path)
}
fn identity(row: &Value) -> Result<(String, String)> {
    Ok((
        row["source_package"]
            .as_str()
            .ok_or("source package absent")?
            .into(),
        row["source_version"]
            .as_str()
            .ok_or("source version absent")?
            .into(),
    ))
}
pub fn check(native: &Value, sources: &Path) -> Result<Value> {
    let expected = native["packages"]
        .as_array()
        .ok_or("native package inventory absent")?
        .iter()
        .map(identity)
        .collect::<Result<BTreeSet<_>>>()?;
    if native["schema"] != 1 || expected.is_empty() {
        return Err("empty or unsupported native inventory".into());
    }
    let inventory_path = regular(sources, "inventory.json")?;
    let sources = inventory_path
        .parent()
        .ok_or("source inventory root absent")?;
    let inventory = storage::parse_json(&fs::read(&inventory_path)?)?;
    let records = inventory["sources"]
        .as_array()
        .ok_or("source inventory records absent")?;
    let identities = records.iter().map(identity).collect::<Result<Vec<_>>>()?;
    let unique = identities.iter().cloned().collect::<BTreeSet<_>>();
    if inventory["schema"] != 1 || identities.len() != unique.len() {
        return Err("unsupported source inventory or duplicate identity".into());
    }
    if unique != expected {
        return Err("source inventory does not exactly cover native source versions".into());
    }
    let mut count = 0;
    for row in records {
        if row["downloaded"] != true {
            return Err("failed native source download".into());
        }
        let mut files = BTreeMap::new();
        let mut names = BTreeSet::new();
        for item in row["files"]
            .as_array()
            .ok_or("source file inventory absent")?
        {
            let relative = item["path"].as_str().ok_or("source file path absent")?;
            let path = regular(sources, relative)?;
            let name = path
                .file_name()
                .ok_or("source filename absent")?
                .to_os_string();
            if files.contains_key(relative) || !names.insert(name) {
                return Err("duplicate source member".into());
            }
            if item["bytes"].as_u64() != Some(path.metadata()?.len())
                || item["sha256"] != storage::sha256(File::open(&path)?)?
            {
                return Err("source inventory digest/size mismatch".into());
            }
            files.insert(relative.to_string(), path);
        }
        let descriptors = files
            .values()
            .filter(|path| path.extension().is_some_and(|extension| extension == "dsc"))
            .collect::<Vec<_>>();
        if descriptors.len() != 1 {
            return Err("exactly one source descriptor required".into());
        }
        let descriptor = descriptors[0];
        let fields = deb822::fields(&fs::read_to_string(descriptor)?)?;
        if deb822::required(&fields, "Source")?
            != row["source_package"]
                .as_str()
                .ok_or("source package absent")?
            || deb822::required(&fields, "Version")?
                != row["source_version"]
                    .as_str()
                    .ok_or("source version absent")?
        {
            return Err("descriptor source/version mismatch".into());
        }
        let mut members = BTreeSet::new();
        for line in deb822::required(&fields, "Checksums-Sha256")?
            .lines()
            .filter(|line| !line.trim().is_empty())
        {
            let (digest, size, name) = deb822::checksum(line)?;
            if Path::new(name).file_name().and_then(|name| name.to_str()) != Some(name)
                || !members.insert(name.to_string())
            {
                return Err("unsafe or duplicate descriptor member".into());
            }
            let relative = descriptor
                .parent()
                .ok_or("descriptor parent absent")?
                .strip_prefix(sources)?
                .join(name);
            let path = regular(
                sources,
                relative.to_str().ok_or("descriptor member is not UTF-8")?,
            )?;
            if !files.values().any(|file| file == &path)
                || path.metadata()?.len() != size
                || storage::sha256(File::open(path)?)? != digest
            {
                return Err("descriptor digest/size mismatch".into());
            }
            count += 1;
        }
        let archives = files
            .values()
            .filter(|path| *path != descriptor)
            .map(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .map(str::to_string)
                    .ok_or_else(|| "archive filename is not UTF-8".into())
            })
            .collect::<Result<BTreeSet<_>>>()?;
        if members.is_empty() || members != archives {
            return Err("descriptor and inventoried source archives differ".into());
        }
    }
    Ok(
        json!({"source_versions":expected.len(),"archive_members":count,"scope":"exact source version coverage and inventory/descriptor SHA-256 integrity only","signature_trust":"not checked; retain authenticated APT repository/download provenance","compliance":"not established; patch/build, notices, delivery and installed runtime gates remain"}),
    )
}
