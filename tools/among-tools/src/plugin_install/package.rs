use crate::{live_status::profile, release_policy, storage, Result};
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    fs,
    os::unix::fs::DirBuilderExt,
    path::{Path, PathBuf},
};

fn inventory(root: &Path) -> Result<BTreeMap<PathBuf, Vec<u8>>> {
    if !root.symlink_metadata()?.is_dir() {
        return Err("package profile root must be a real directory".into());
    }
    let mut pending = vec![PathBuf::new()];
    let mut files = BTreeMap::new();
    let mut remaining = 8 * 1024 * 1024usize;
    let mut entries = 0;
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(root.join(&directory))? {
            entries += 1;
            if entries > 64 {
                return Err("package profile exceeds its entry budget".into());
            }
            let entry = entry?;
            let relative = directory.join(entry.file_name());
            let path = storage::safe_path(root, &relative)?;
            if entry.file_type()?.is_dir() {
                pending.push(relative);
                continue;
            }
            if !entry.file_type()?.is_file() {
                return Err("package profile contains a non-regular input".into());
            }
            let bytes = profile::bounded(&path, remaining as u64)?;
            remaining = remaining
                .checked_sub(bytes.len())
                .ok_or("package profile exceeds its byte budget")?;
            files.insert(relative, bytes);
        }
    }
    Ok(files)
}

pub fn clean_source(root: &Path) -> Result<String> {
    if !release_policy::git(root, &["status", "--porcelain=v1", "--untracked-files=all"])?
        .is_empty()
    {
        return Err(
            "package profile capture requires an unchanged clean host source checkout".into(),
        );
    }
    release_policy::git(root, &["rev-parse", "HEAD"])
}

fn host_test(root: &Path, records: &str) -> Result<PathBuf> {
    let executables = crate::ignored_tests::executables(records)?;
    if executables.len() != 1 {
        return Err("package profile needs exactly one Cargo host-test executable".into());
    }
    let executable = executables
        .into_iter()
        .next()
        .ok_or("Cargo host test absent")?;
    let root = root.canonicalize()?;
    let mut matching = 0;
    for line in records.lines().filter(|line| !line.trim().is_empty()) {
        let item = storage::parse_json(line.as_bytes())?;
        if item["reason"] == "compiler-artifact"
            && item["profile"]["test"] == true
            && item["executable"].as_str() == executable.to_str()
        {
            if item["manifest_path"].as_str() != root.join("Cargo.toml").to_str()
                || item["target"]["src_path"].as_str() != root.join("src/main.rs").to_str()
                || item["target"]["name"] != "among"
            {
                return Err("Cargo test record belongs to a different host source/target".into());
            }
            matching += 1;
        }
    }
    if matching != 1 {
        return Err("Cargo host test record missing or duplicated".into());
    }
    Ok(executable)
}

pub fn capture(root: &Path, records: &Path, output: &Path, scratch: &Path) -> Result<()> {
    crate::preferences::owned_display()?;
    let source = clean_source(root)?;
    let record_bytes = profile::bounded(records, 8 * 1024 * 1024)?;
    let record_sha = storage::sha256(record_bytes.as_slice())?;
    let executable = host_test(root, std::str::from_utf8(&record_bytes)?)?;
    let test_sha = storage::sha256(profile::bounded(&executable, 256 * 1024 * 1024)?.as_slice())?;
    let inputs =
        storage::private_directory(scratch, "among-package-profile-inputs")?.join("release");
    super::release::prepare(&inputs)?;
    let observed = super::run(&executable, &inputs, scratch)?;
    let lifecycle = storage::parse_json(&profile::bounded(
        &observed.join("published-release-lifecycle.json"),
        256 * 1024,
    )?)?;
    let profile = observed.join(".runtime-published-live-profile");
    let metadata =
        storage::parse_json(&profile::bounded(&profile.join("profile.json"), 64 * 1024)?)?;
    if metadata["host_source_sha"] != source {
        return Err("observed host test was not compiled for this exact source SHA".into());
    }
    if clean_source(root)? != source
        || storage::sha256(profile::bounded(records, 8 * 1024 * 1024)?.as_slice())? != record_sha
        || storage::sha256(profile::bounded(&executable, 256 * 1024 * 1024)?.as_slice())?
            != test_sha
    {
        return Err("host source/test input changed during package profile capture".into());
    }
    // verify saved selection/grants before transfer; this is not installation
    // on the future compiler-free package host.
    profile::replay(&profile, &observed.join(".runtime-transfer-validation")).map_err(|error| {
        format!(
            "observed package profile failed renderer replay preflight: {error}; evidence: {}",
            observed.display()
        )
    })?;
    let files = inventory(&profile)?;
    let hashes = files
        .iter()
        .map(|(path, bytes)| {
            Ok(json!({"path":path,"size":bytes.len(),"sha256":storage::sha256(bytes.as_slice())?}))
        })
        .collect::<Result<Vec<_>>>()?;
    let record = serde_json::to_vec_pretty(
        &json!({"schema":1,"host_source_sha":source,"host_test_sha256":test_sha,"cargo_records_sha256":record_sha,"architecture":crate::platform::discover()?.architecture,"files":hashes,"lifecycle":lifecycle,"scope":"observed build-host installation/access; compiler-free package renderer replay only"}),
    )?;
    if record.len() > 64 * 1024
        || record.len() + files.values().map(Vec::len).sum::<usize>() > 8 * 1024 * 1024
    {
        return Err("package profile plus source receipt exceeds its transfer budget".into());
    }
    fs::DirBuilder::new().mode(0o755).create(output)?;
    for (path, bytes) in files {
        let target = storage::safe_path(output, &path)?;
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o755)
            .create(target.parent().ok_or("profile target lacks parent")?)?;
        storage::atomic_write(output, &path, &bytes, 0o644)?;
    }
    storage::atomic_write(output, Path::new("transfer.json"), &record, 0o644)?;
    validate(output, &source)?;
    println!("PASS transferred actual native installed profile bound to host source {source}; not packaged installation acceptance");
    Ok(())
}

pub fn validate(directory: &Path, source: &str) -> Result<()> {
    let mut files = inventory(directory)?;
    let record: Value = storage::parse_json(
        &files
            .remove(Path::new("transfer.json"))
            .ok_or("package profile transfer record absent")?,
    )?;
    let hex = |text: &str, length| {
        text.len() == length
            && text
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    };
    if !hex(source, 40)
        || record["schema"] != 1
        || record["host_source_sha"] != source
        || !record["host_test_sha256"]
            .as_str()
            .is_some_and(|sha| hex(sha, 64))
        || !record["cargo_records_sha256"]
            .as_str()
            .is_some_and(|sha| hex(sha, 64))
        || record["architecture"] != crate::platform::discover()?.architecture
    {
        return Err("package profile host-source/test/native architecture binding differs".into());
    }
    let metadata = storage::parse_json(
        files
            .get(Path::new("profile.json"))
            .ok_or("package profile metadata absent")?,
    )?;
    if metadata["host_source_sha"] != source {
        return Err("package profile test/source receipt differs".into());
    }
    let declared = record["files"]
        .as_array()
        .ok_or("package profile file hashes absent")?;
    for entry in declared {
        let path = Path::new(
            entry["path"]
                .as_str()
                .ok_or("package profile path absent")?,
        );
        storage::relative_path(path)?;
        let bytes = files
            .remove(path)
            .ok_or("package profile file missing or duplicated")?;
        if entry["size"] != bytes.len() || entry["sha256"] != storage::sha256(bytes.as_slice())? {
            return Err(
                "package profile bytes differ from transferred installed observation".into(),
            );
        }
    }
    if !files.is_empty() {
        return Err("package profile contains unrecorded bytes".into());
    }
    for path in [
        "profile.json",
        "managed/installed.json",
        "managed-plugins.json",
        "among-plugins.json",
    ] {
        if !declared.iter().any(|entry| entry["path"] == path) {
            return Err("package profile omits a required observation".into());
        }
    }
    let lifecycle = &record["lifecycle"];
    if lifecycle["architecture"] != record["architecture"]
        || lifecycle["source_revision"] != super::release::SOURCE
        || lifecycle["catalogue_revision"] != super::release::CATALOGUE
    {
        return Err("package profile lifecycle provenance differs".into());
    }
    for field in [
        "actual_public_install",
        "explicit_review_cancel_accept",
        "three_mapped_outputs",
        "disabled_then_uninstalled",
        "same_pty_real_input",
    ] {
        if lifecycle[field] != true {
            return Err("package profile full lifecycle was not accepted".into());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
