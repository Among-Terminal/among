use crate::{deb, release_policy, source_snapshot, stable_build, storage, Result};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::io::Read;
use std::path::Path;

type Contents = BTreeMap<String, Vec<u8>>;
type Hashes = BTreeMap<String, String>;
fn original(stream: impl Read) -> Result<(Contents, Hashes)> {
    let mut contents = BTreeMap::new();
    let mut hashes = BTreeMap::new();
    let mut archive = tar::Archive::new(stream);
    for entry in archive.entries()? {
        let mut entry = entry?;
        if !entry.header().entry_type().is_file() {
            return Err("original source archive must contain only regular members".into());
        }
        let path = entry.path()?.into_owned();
        storage::relative_path(&path)?;
        let name = path
            .to_str()
            .ok_or("original source path is not UTF-8")?
            .to_string();
        if hashes.contains_key(&name) {
            return Err("duplicate original source archive member".into());
        }
        if name.starts_with("vendor/") {
            hashes.insert(name, storage::sha256(&mut entry)?);
        } else {
            let mut bytes = Vec::new();
            entry.read_to_end(&mut bytes)?;
            hashes.insert(name.clone(), storage::sha256(bytes.as_slice())?);
            contents.insert(name, bytes);
        }
    }
    let inventory = storage::parse_json(
        contents
            .get("source-inventory.json")
            .ok_or("source inventory missing")?,
    )?;
    hashes.remove("source-inventory.json");
    if inventory["schema"] != 1 || inventory["files"] != serde_json::to_value(&hashes)? {
        return Err("original source inventory differs from archived members or digests".into());
    }
    Ok((contents, hashes))
}

pub fn check(repo: &Path, record: &Value, bundle: &Path) -> Result<Value> {
    let tag = record["tag"].as_str().ok_or("stable source tag absent")?;
    let version = deb::stable_version(tag)?;
    if record["version"] != version {
        return Err("stable source version differs from tag".into());
    }
    let source = release_policy::git(
        repo,
        &[
            "rev-parse",
            "--verify",
            &format!("refs/tags/{tag}^{{commit}}"),
        ],
    )?;
    if record["source_sha"] != source {
        return Err("stable source does not resolve to tag commit".into());
    }
    if !bundle.symlink_metadata()?.file_type().is_file() {
        return Err("source delivery bundle must be a regular file".into());
    }
    let mut delivery = tar::Archive::new(File::open(bundle)?);
    let mut archived = None;
    let mut names = BTreeSet::new();
    for entry in delivery.entries()? {
        let entry = entry?;
        let path = entry.path()?.into_owned();
        storage::relative_path(&path)?;
        if !entry.header().entry_type().is_file() || !names.insert(path.clone()) {
            return Err("source delivery members must be unique regular files".into());
        }
        if path == Path::new("original-source.tar") {
            archived = Some(original(entry)?);
        }
    }
    let (contents, hashes) = archived.ok_or("original source archive missing")?;
    let overlay = storage::parse_json(
        contents
            .get("snap/stable-build-input.json")
            .ok_or("stable overlay input record missing")?,
    )?;
    if overlay["schema"] != 1 || overlay["tag"] != tag || overlay["version"] != version {
        return Err("stable overlay identity mismatch".into());
    }
    let listed = release_policy::git_bytes(repo, &["ls-tree", "-rz", "--full-tree", &source])?;
    let mut expected = BTreeMap::new();
    for entry in listed
        .split(|byte| *byte == 0)
        .filter(|entry| !entry.is_empty())
    {
        let split = entry
            .iter()
            .position(|byte| *byte == b'\t')
            .ok_or("malformed Git tree entry")?;
        let metadata = std::str::from_utf8(&entry[..split])?;
        let name = std::str::from_utf8(&entry[split + 1..])?;
        if Path::new(name)
            .components()
            .any(|part| part.as_os_str() == "__pycache__")
        {
            continue;
        }
        if !source_snapshot::FILES.contains(&name)
            && !source_snapshot::DIRECTORIES
                .iter()
                .any(|directory| name.starts_with(&format!("{directory}/")))
        {
            continue;
        }
        let fields = metadata.split_whitespace().collect::<Vec<_>>();
        if fields.len() != 3 || !matches!(fields[0], "100644" | "100755") || fields[1] != "blob" {
            return Err("public Git source inputs must be regular files".into());
        }
        let data = release_policy::git_bytes(repo, &["cat-file", "blob", fields[2]])?;
        expected.insert(name.to_string(), storage::sha256(data.as_slice())?);
    }
    if source_snapshot::FILES
        .iter()
        .any(|name| !expected.contains_key(*name))
    {
        return Err("tagged source is missing mandatory public inputs".into());
    }
    if overlay["original_inputs"] != serde_json::to_value(&expected)? {
        return Err("stable source overlay does not match tagged commit".into());
    }
    let extras = [
        "snap/stable-build-input.json",
        "snap/snapcraft.devel.yaml",
        ".cargo/config.toml",
    ];
    let public = contents
        .keys()
        .filter(|name| name.as_str() != "source-inventory.json")
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    let selected = expected
        .keys()
        .map(String::as_str)
        .chain(extras)
        .collect::<BTreeSet<_>>();
    if public != selected {
        return Err("unexpected or missing public source members".into());
    }
    for (name, digest) in &expected {
        let target = if name == "snap/snapcraft.yaml" {
            "snap/snapcraft.devel.yaml"
        } else {
            name
        };
        if hashes.get(target) != Some(digest) {
            return Err("archived input differs from tagged source".into());
        }
    }
    let before = contents
        .get("snap/snapcraft.devel.yaml")
        .ok_or("original recipe absent")?;
    let after = contents
        .get("snap/snapcraft.yaml")
        .ok_or("effective recipe absent")?;
    if overlay["original_recipe_sha256"] != storage::sha256(before.as_slice())?
        || overlay["effective_recipe_sha256"] != storage::sha256(after.as_slice())?
    {
        return Err("stable recipe hash mismatch".into());
    }
    stable_build::recipe_change(std::str::from_utf8(before)?, std::str::from_utf8(after)?)?;
    Ok(
        json!({"source_sha":source,"original_inputs":expected.len(),"scope":"tagged public source and stable overlay binding"}),
    )
}
