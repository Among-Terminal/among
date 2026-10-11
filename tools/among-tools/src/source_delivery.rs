use crate::{storage, Result};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::Read;
use std::path::Path;

fn regular(path: &Path) -> Result<File> {
    let kind = path.symlink_metadata()?.file_type();
    if !kind.is_file() {
        return Err("associated artifact must be a regular non-symlink file".into());
    }
    Ok(File::open(path)?)
}

pub fn inventory(stream: impl Read, inventory_name: &str) -> Result<BTreeMap<String, String>> {
    let mut hashes = BTreeMap::new();
    let mut archive = tar::Archive::new(stream);
    let mut inventory = None;
    for entry in archive.entries()? {
        let mut entry = entry?;
        if !entry.header().entry_type().is_file() {
            return Err("source archive members must be regular files".into());
        }
        let name = entry.path()?.into_owned();
        storage::relative_path(&name)?;
        let name = name
            .to_str()
            .ok_or("source archive name is not UTF-8")?
            .to_string();
        if hashes.contains_key(&name) {
            return Err("duplicate source archive member".into());
        }
        if name == inventory_name {
            let mut data = Vec::new();
            entry.read_to_end(&mut data)?;
            inventory = Some(storage::parse_json(&data)?);
            hashes.insert(name, storage::sha256(data.as_slice())?);
        } else {
            hashes.insert(name, storage::sha256(&mut entry)?);
        }
    }
    let inventory = inventory.ok_or("source delivery inventory is missing")?;
    hashes.remove(inventory_name);
    if inventory["schema"] != 1 || inventory["files"] != serde_json::to_value(&hashes)? {
        return Err("source delivery inventory is incomplete or a delivered member changed".into());
    }
    Ok(hashes)
}

pub fn check(association: &Path, bundle: &Path, snap: &Path) -> Result<Value> {
    let record = storage::parse_json(&fs::read(association)?)?;
    if record["schema"] != 1 {
        return Err("unsupported source association".into());
    }
    for (key, path) in [("snap", snap), ("source_bundle", bundle)] {
        if record[key]["filename"].as_str() != path.file_name().and_then(|name| name.to_str()) {
            return Err("wrong associated artifact filename".into());
        }
        if record[key]["sha256"] != storage::sha256(regular(path)?)? {
            return Err("associated artifact digest mismatch".into());
        }
    }
    let hashes = inventory(regular(bundle)?, "delivery-inventory.json")?;
    let original = hashes
        .get("original-source.tar")
        .ok_or("original source snapshot absent from delivery")?;
    if record["original_source_sha256"] != *original {
        return Err("original-source association mismatch".into());
    }
    Ok(
        json!({"schema":1,"files":hashes.len()+1,"snap_sha256":record["snap"]["sha256"],"source_sha256":record["source_bundle"]["sha256"],"scope":"artifact association, archive safety and complete member integrity"}),
    )
}
