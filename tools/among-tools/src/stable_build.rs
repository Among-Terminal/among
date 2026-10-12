use crate::{deb, deb_workflow, source_snapshot, storage, Result};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};

pub fn recipe_change(before: &str, after: &str) -> Result<()> {
    let original = deb_workflow::read(before)?;
    let effective = deb_workflow::read(after)?;
    if original["name"] != "among"
        || original["base"] != "core24"
        || original["confinement"] != "classic"
        || original["grade"] != "devel"
    {
        return Err("expected unchanged core24/classic devel candidate recipe".into());
    }
    let mut expected = original;
    expected["grade"] = "stable".into();
    if effective != expected {
        return Err("stable overlay changes more than effective grade".into());
    }
    Ok(())
}

pub fn check(tree: &Path) -> Result<Value> {
    let record = storage::parse_json(&fs::read(storage::safe_path(
        tree,
        Path::new("snap/stable-build-input.json"),
    )?)?)?;
    let version = deb::stable_version(record["tag"].as_str().ok_or("stable overlay tag absent")?)?;
    if record["schema"] != 1 || record["version"] != version {
        return Err("invalid stable build identity".into());
    }
    let before = fs::read(storage::safe_path(
        tree,
        Path::new("snap/snapcraft.devel.yaml"),
    )?)?;
    let after = fs::read(storage::safe_path(tree, Path::new("snap/snapcraft.yaml"))?)?;
    if record["original_recipe_sha256"] != storage::sha256(before.as_slice())?
        || record["effective_recipe_sha256"] != storage::sha256(after.as_slice())?
    {
        return Err("stable original/effective recipe changed".into());
    }
    recipe_change(std::str::from_utf8(&before)?, std::str::from_utf8(&after)?)?;
    let inputs = record["original_inputs"]
        .as_object()
        .ok_or("original input inventory absent")?;
    for required in source_snapshot::FILES
        .iter()
        .copied()
        .chain(["snap/snapcraft.yaml"])
    {
        if !inputs.contains_key(required) {
            return Err("mandatory original input missing".into());
        }
    }
    for (name, expected) in inputs {
        storage::relative_path(Path::new(name))?;
        let target = if name == "snap/snapcraft.yaml" {
            "snap/snapcraft.devel.yaml"
        } else {
            name
        };
        let path = storage::safe_path(tree, Path::new(target))?;
        if !path.is_file() || expected != &Value::String(storage::sha256(fs::File::open(path)?)?) {
            return Err("source input changed after stable preparation".into());
        }
    }
    Ok(record)
}

struct Stage(PathBuf);
impl Drop for Stage {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

pub fn prepare(root: &Path, output: &Path, tag: &str) -> Result<Value> {
    let version = deb::stable_version(tag)?;
    let root = root.canonicalize()?;
    if output.symlink_metadata().is_ok() {
        return Err("stable build destination must not exist".into());
    }
    let absolute = std::path::absolute(output)?;
    let parent = absolute
        .parent()
        .ok_or("stable destination parent absent")?
        .canonicalize()?;
    if parent.starts_with(&root) {
        return Err("stable build tree must be outside source directory".into());
    }
    let output = parent.join(
        absolute
            .file_name()
            .ok_or("stable destination name absent")?,
    );
    for manifest in ["Cargo.toml", "examples/plugins/Cargo.toml"] {
        let document: toml::Value = toml::from_str(&fs::read_to_string(storage::safe_path(
            &root,
            Path::new(manifest),
        )?)?)?;
        if document
            .get("package")
            .and_then(|value| value.get("version"))
            .and_then(toml::Value::as_str)
            != Some(version)
        {
            return Err("stable tag must match both package versions".into());
        }
    }
    let original =
        fs::read_to_string(storage::safe_path(&root, Path::new("snap/snapcraft.yaml"))?)?;
    let mut count = 0;
    let mut effective = String::new();
    for line in original.split_inclusive('\n') {
        if line.strip_suffix('\n').unwrap_or(line) == "grade: devel" {
            count += 1;
            effective.push_str("grade: stable");
            if line.ends_with('\n') {
                effective.push('\n');
            }
        } else {
            effective.push_str(line);
        }
    }
    if count != 1 {
        return Err("stable overlay requires exactly one literal top-level grade".into());
    }
    recipe_change(&original, &effective)?;
    for name in ["snap/snapcraft.devel.yaml", "snap/stable-build-input.json"] {
        if root.join(name).symlink_metadata().is_ok() {
            return Err("source already contains a stable overlay".into());
        }
    }
    let inputs = source_snapshot::inputs(&root, &[])?;
    let hashes = inputs
        .iter()
        .map(|(name, file)| Ok((name.clone(), storage::sha256(file.bytes.as_slice())?)))
        .collect::<Result<BTreeMap<_, _>>>()?;
    let stage = Stage(storage::private_directory(&parent, "among-stable-stage")?);
    for (name, file) in &inputs {
        let path = storage::safe_path(&stage.0, Path::new(name))?;
        fs::create_dir_all(path.parent().ok_or("stable input parent absent")?)?;
        storage::atomic_write(&stage.0, Path::new(name), &file.bytes, file.mode)?;
    }
    storage::atomic_write(
        &stage.0,
        Path::new("snap/snapcraft.devel.yaml"),
        original.as_bytes(),
        0o644,
    )?;
    storage::atomic_write(
        &stage.0,
        Path::new("snap/snapcraft.yaml"),
        effective.as_bytes(),
        0o644,
    )?;
    let reread = source_snapshot::inputs(&root, &[])?;
    if inputs.len() != reread.len()
        || inputs.iter().any(|(name, file)| {
            reread
                .get(name)
                .is_none_or(|other| other.bytes != file.bytes || other.mode != file.mode)
        })
    {
        return Err("source inputs changed while preparing stable build".into());
    }
    let record = json!({"schema":1,"tag":tag,"version":version,"original_recipe_sha256":storage::sha256(original.as_bytes())?,"effective_recipe_sha256":storage::sha256(effective.as_bytes())?,"original_inputs":hashes,"scope":"isolated build overlay, not publication approval or commit provenance"});
    storage::atomic_write(
        &stage.0,
        Path::new("snap/stable-build-input.json"),
        &serde_json::to_vec_pretty(&record)?,
        0o644,
    )?;
    check(&stage.0)?;
    // Exclusive destination creation preserves owner directories. A subsequent
    // move failure can leave partial owned output; it is not crash atomicity.
    fs::DirBuilder::new().mode(0o700).create(&output)?;
    for entry in fs::read_dir(&stage.0)? {
        let entry = entry?;
        fs::rename(entry.path(), output.join(entry.file_name()))?;
    }
    check(&output)
}
