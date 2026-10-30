//! immutable release inputs for native checks, not an alternative plugin installer.
//! only the application may turn these packages into installed selections/grants.

use crate::{process::OwnedProcess, storage, Result};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::Duration;

#[cfg(test)]
pub(crate) mod fixture;
mod observation;
pub use observation::select;

pub const REPOSITORY: u64 = 1431055733;
pub const PUBLISHER: u64 = 339567100;

fn text<'a>(value: &'a Value, field: &str) -> Result<&'a str> {
    value[field]
        .as_str()
        .ok_or_else(|| format!("release input lacks string {field}").into())
}

fn number(value: &Value, field: &str) -> Result<u64> {
    value[field]
        .as_u64()
        .ok_or_else(|| format!("release input lacks unsigned {field}").into())
}

fn fetch(root: &Path, url: &str, name: &str, limit: u64, asset: bool) -> Result<PathBuf> {
    let destination = storage::safe_path(root, Path::new(name))?;
    let output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&destination)?;
    let log = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(destination.with_extension("curl.log"))?;
    let environment = BTreeMap::from([("PATH".into(), "/usr/bin:/bin".into())]);
    let maximum = limit.to_string();
    let mut arguments = vec![
        "-q",
        "--fail",
        "--silent",
        "--show-error",
        "--proto",
        "=https",
        "--proto-redir",
        "=https",
        "--connect-timeout",
        "10",
        "--max-time",
        "60",
        "--max-filesize",
        &maximum,
    ];
    if asset {
        arguments.extend([
            "--location",
            "--max-redirs",
            "3",
            "--header",
            "Accept: application/octet-stream",
        ]);
    }
    arguments.push(url);
    let mut child = OwnedProcess::spawn_split(
        Path::new("/usr/bin/curl"),
        &arguments,
        &environment,
        root,
        &output,
        &log,
    )?;
    if !child.wait(Duration::from_secs(65))?.success() || destination.metadata()?.len() > limit {
        return Err(format!("public release download failed; retained {}", name).into());
    }
    Ok(destination)
}

fn regular_bytes(path: &Path, maximum: u64) -> Result<Vec<u8>> {
    // Inspect the descriptor we read, not a pathname that can change after stat.
    // NONBLOCK also keeps a substituted FIFO from trapping a check before fstat.
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(nix::libc::O_NOFOLLOW | nix::libc::O_CLOEXEC | nix::libc::O_NONBLOCK)
        .open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.len() > maximum {
        return Err("release input is non-regular or exceeds its byte limit".into());
    }
    let mut bytes = Vec::new();
    (&mut file).take(maximum + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > maximum {
        return Err("release input grew beyond its byte limit while being read".into());
    }
    Ok(bytes)
}

pub fn checked_bytes(path: &Path, size: u64, digest: &str) -> Result<Vec<u8>> {
    if size > 32 * 1024 * 1024 {
        return Err("release input exceeds the exact-byte check limit".into());
    }
    let bytes = regular_bytes(path, size)?;
    if bytes.len() as u64 != size || storage::sha256(bytes.as_slice())? != digest {
        return Err("release input size/SHA256 differs from its pinned declaration".into());
    }
    Ok(bytes)
}

fn metadata(root: &Path, endpoint: &str, name: &str) -> Result<Value> {
    let url = format!("https://api.github.com/{endpoint}");
    storage::parse_json(&fs::read(fetch(root, &url, name, 1024 * 1024, false)?)?)
}

pub fn catalogue(root: &Path, inputs: &Value) -> Result<Value> {
    let size = number(inputs, "catalogue_size")?;
    if size == 0 || size > 1024 * 1024 {
        return Err("release catalogue exceeds its exact byte bound".into());
    }
    storage::parse_json(&checked_bytes(
        &storage::safe_path(root, Path::new("among-plugins.json"))?,
        size,
        text(inputs, "catalogue_sha256")?,
    )?)
}

fn proof_at(root: &Path, name: &str) -> Result<Value> {
    let inputs = storage::parse_json(&regular_bytes(
        &storage::safe_path(root, Path::new(name))?,
        256 * 1024,
    )?)?;
    let catalogue = catalogue(root, &inputs)?;
    observation::validate(
        &inputs,
        &catalogue,
        &crate::platform::discover()?.architecture,
    )?;
    Ok(inputs)
}

pub fn profile_inputs(profile: &Path) -> Result<Value> {
    proof_at(profile, "release-inputs.json")
}

pub fn observed_asset<'a>(inputs: &'a Value, name: &str) -> Result<&'a Value> {
    observation::asset(&inputs["release_snapshot"], name)
}

pub fn verify(root: &Path) -> Result<Value> {
    let root = root.canonicalize()?;
    let inputs = proof_at(&root, "inputs.json")?;
    let catalogue = catalogue(&root, &inputs)?;
    let architecture = match std::env::consts::ARCH {
        "x86_64" => "amd64",
        "aarch64" => "arm64",
        _ => return Err("unsupported native architecture".into()),
    };
    for (version, artifact) in select(&catalogue, architecture)? {
        let id = text(&version["manifest"], "id")?;
        let matches = inputs["plugins"]
            .as_array()
            .ok_or("prepared plugins absent")?
            .iter()
            .filter(|plugin| plugin["id"] == id)
            .collect::<Vec<_>>();
        let [plugin] = matches.as_slice() else {
            return Err("prepared plugin missing or duplicated".into());
        };
        let directory = storage::safe_path(&root, Path::new(id))?;
        let entrypoint = text(&version["manifest"], "entrypoint")?;
        let binary = storage::safe_path(&directory, Path::new(entrypoint))?;
        if plugin["version"] != version["manifest"]["version"]
            || plugin["revision"] != inputs["source_revision"]
            || plugin["release_id"] != inputs["release_id"]
            || plugin["asset_id"] != observed_asset(&inputs, text(artifact, "asset")?)?["id"]
            || plugin["sha256"] != artifact["sha256"]
            || plugin["manifest_sha256"] != version["manifest_sha256"]
            || plugin["binary"] != json!(binary)
            || plugin["package"] != json!(root.join(format!("{id}.tar.gz")))
            || plugin["architecture"] != architecture
            || plugin["entrypoint"] != entrypoint
        {
            return Err("prepared plugin paths or release declarations were substituted".into());
        }
        checked_bytes(
            &root.join(format!("{id}.tar.gz")),
            number(artifact, "size")?,
            text(artifact, "sha256")?,
        )?;
        for member in artifact["members"]
            .as_array()
            .ok_or("member declarations absent")?
        {
            let path = storage::safe_path(&directory, Path::new(text(member, "path")?))?;
            checked_bytes(&path, number(member, "size")?, text(member, "sha256")?)?;
            if member["path"] == entrypoint
                && (plugin["binary_sha256"] != member["sha256"]
                    || path.metadata()?.permissions().mode() & 0o100 == 0)
            {
                return Err("prepared native entrypoint differs from its declared member".into());
            }
        }
        if storage::parse_json(&regular_bytes(
            &directory.join("among-plugin.json"),
            256 * 1024,
        )?)? != version["manifest"]
        {
            return Err("prepared manifest differs from the release catalogue".into());
        }
        let source =
            storage::parse_json(&regular_bytes(&directory.join("SOURCE.json"), 64 * 1024)?)?;
        if source["repository_id"] != inputs["repository_id"]
            || source["revision"] != inputs["source_revision"]
            || source["source_archive"] != inputs["source_archive"]
            || source["source_sha256"] != inputs["source_sha256"]
        {
            return Err("package source delivery disagrees with the observed release".into());
        }
    }
    checked_bytes(
        &root.join("source.tar.gz"),
        number(&inputs, "source_size")?,
        text(&inputs, "source_sha256")?,
    )?;
    Ok(inputs)
}

fn unpack(root: &Path, archive: &Path, plugin: &str, artifact: &Value) -> Result<PathBuf> {
    let target = root.join(plugin);
    fs::DirBuilder::new().mode(0o700).create(&target)?;
    let expanded = root.join(format!("{plugin}.tar"));
    let output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&expanded)?;
    let log = File::create(root.join(format!("{plugin}-gzip.log")))?;
    let environment = BTreeMap::from([("PATH".into(), "/usr/bin:/bin".into())]);
    let mut child = OwnedProcess::spawn_split(
        Path::new("/usr/bin/gzip"),
        &["-dc", archive.to_str().ok_or("package path is not UTF-8")?],
        &environment,
        root,
        &output,
        &log,
    )?;
    if !child.wait(Duration::from_secs(10))?.success()
        || expanded.metadata()?.len() > 16 * 1024 * 1024
    {
        return Err("pinned package decompression failed or exceeded its evidence budget".into());
    }
    let mut declared = BTreeMap::new();
    for member in artifact["members"]
        .as_array()
        .ok_or("member declarations absent")?
    {
        if declared
            .insert(text(member, "path")?.to_string(), member)
            .is_some()
        {
            return Err("duplicate package member declaration".into());
        }
    }
    if declared.is_empty() || declared.len() > 4096 {
        return Err("invalid package member count".into());
    }
    let mut seen = BTreeSet::new();
    let mut archive = tar::Archive::new(File::open(expanded)?);
    for entry in archive.entries()? {
        let mut entry = entry?;
        let path = entry.path()?.into_owned();
        storage::relative_path(&path)?;
        let name = path.to_str().ok_or("non-UTF8 package member")?;
        let member = declared.get(name).ok_or("undeclared package member")?;
        let size = number(member, "size")?;
        let executable = member["executable"]
            .as_bool()
            .ok_or("member executable flag absent")?;
        if !seen.insert(name.to_string())
            || !entry.header().entry_type().is_file()
            || entry.size() != size
            || size > 8 * 1024 * 1024
            || entry.header().mode()? & 0o7777 != if executable { 0o755 } else { 0o644 }
        {
            return Err("package member is duplicated, non-regular or has wrong size/mode".into());
        }
        let mut bytes = Vec::new();
        (&mut entry).take(size + 1).read_to_end(&mut bytes)?;
        if bytes.len() as u64 != size
            || storage::sha256(bytes.as_slice())? != text(member, "sha256")?
        {
            return Err("package member SHA256/size differs from catalogue".into());
        }
        let destination = storage::safe_path(&target, &path)?;
        fs::create_dir_all(destination.parent().ok_or("member parent absent")?)?;
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(if executable { 0o700 } else { 0o600 })
            .open(destination)?;
        file.write_all(&bytes)?;
    }
    if seen.len() != declared.len() {
        return Err("package omitted declared members".into());
    }
    Ok(target)
}

fn repository(root: &Path, name: &str) -> Result<()> {
    let repository = metadata(root, "repositories/1431055733", name)?;
    if repository["id"] != REPOSITORY
        || repository["owner"]["id"] != PUBLISHER
        || repository["private"] != false
        || repository["full_name"] != "Among-Terminal/among-plugins"
    {
        return Err("official repository identity changed".into());
    }
    Ok(())
}

fn latest(root: &Path, name: &str) -> Result<Value> {
    observation::snapshot(&metadata(
        root,
        "repositories/1431055733/releases/latest",
        name,
    )?)
}

fn resolve(root: &Path, snapshot: &Value, pass: &str) -> Result<(Value, Value)> {
    let tag = text(snapshot, "tag")?;
    let reference = metadata(
        root,
        &format!("repositories/{REPOSITORY}/git/ref/tags/{tag}"),
        &format!("{pass}-ref.json"),
    )?;
    if reference["ref"] != format!("refs/tags/{tag}") {
        return Err("release reference is not its exact tag".into());
    }
    let mut object = reference["object"].clone();
    let head = text(&object, "sha")?.to_owned();
    let mut seen = BTreeSet::new();
    let mut chain = Vec::new();
    for depth in 0..=8 {
        let kind = text(&object, "type")?;
        let sha = text(&object, "sha")?;
        if !observation::hex(sha, 40) || !seen.insert(sha.to_owned()) {
            return Err("release tag chain is malformed or cyclic".into());
        }
        chain.push(json!([kind, sha]));
        if kind == "commit" {
            let commit = metadata(
                root,
                &format!("repositories/{REPOSITORY}/git/commits/{sha}"),
                &format!("{pass}-commit.json"),
            )?;
            if commit["sha"] != sha {
                return Err("release tag commit identity changed".into());
            }
            return Ok((json!({"tag":tag,"object":head,"commit":sha}), json!(chain)));
        }
        if kind != "tag" || depth == 8 {
            return Err("release tag chain is unsupported or exceeds eight annotations".into());
        }
        let annotation = metadata(
            root,
            &format!("repositories/{REPOSITORY}/git/tags/{sha}"),
            &format!("{pass}-tag-{depth}.json"),
        )?;
        if annotation["sha"] != sha {
            return Err("release annotation identity changed".into());
        }
        object = annotation["object"].clone();
    }
    Err("release tag did not resolve to a commit".into())
}

fn download(root: &Path, asset: &Value, name: &str, maximum: u64) -> Result<PathBuf> {
    let size = number(asset, "size")?;
    if size == 0 || size > maximum {
        return Err("published input asset exceeds its transfer bound".into());
    }
    let id = number(asset, "id")?;
    let path = fetch(
        root,
        &format!("https://api.github.com/repositories/{REPOSITORY}/releases/assets/{id}"),
        name,
        size,
        true,
    )?;
    checked_bytes(&path, size, text(asset, "sha256")?)?;
    Ok(path)
}

pub fn prepare(destination: &Path) -> Result<PathBuf> {
    fs::DirBuilder::new().mode(0o700).create(destination)?;
    let root = destination.canonicalize()?;
    let architecture = crate::platform::discover()?.architecture;
    repository(&root, "repository.json")?;
    let snapshot = latest(&root, "latest.json")?;
    let catalogue_asset = observation::asset(&snapshot, "among-plugins.json")?;
    let catalogue_path = download(&root, catalogue_asset, "among-plugins.json", 1024 * 1024)?;
    let catalogue = storage::parse_json(&checked_bytes(
        &catalogue_path,
        number(catalogue_asset, "size")?,
        text(catalogue_asset, "sha256")?,
    )?)?;
    let (binding, chain) = resolve(&root, &snapshot, "initial")?;
    let revision = text(&binding, "commit")?;
    let source_name = format!("source-{revision}.tar.gz");
    let source = observation::asset(&snapshot, &source_name)?;
    let selections = select(&catalogue, &architecture)?;
    // Both native declarations must bind even though this runner transfers one.
    for (version, _) in &selections {
        if version["revision"] != revision {
            return Err("catalogue source differs from the resolved release tag".into());
        }
        for artifact in version["artifacts"].as_array().ok_or("artifacts absent")? {
            let observed = observation::asset(&snapshot, text(artifact, "asset")?)?;
            if observed["size"] != artifact["size"] || observed["sha256"] != artifact["sha256"] {
                return Err("catalogue package differs from the observed release asset".into());
            }
        }
    }
    let mut plugins = Vec::new();
    for (version, artifact) in selections {
        let id = text(&version["manifest"], "id")?;
        let selected = observation::asset(&snapshot, text(artifact, "asset")?)?;
        let package = download(&root, selected, &format!("{id}.tar.gz"), 64 * 1024 * 1024)?;
        let directory = unpack(&root, &package, id, artifact)?;
        let members = artifact["members"].as_array().ok_or("members absent")?;
        let manifest_member = members
            .iter()
            .find(|member| member["path"] == "among-plugin.json")
            .ok_or("manifest member absent")?;
        let manifest = checked_bytes(
            &directory.join("among-plugin.json"),
            number(manifest_member, "size")?,
            text(version, "manifest_sha256")?,
        )?;
        if storage::parse_json(&manifest)? != version["manifest"] {
            return Err("package manifest differs from catalogue".into());
        }
        let entrypoint = text(&version["manifest"], "entrypoint")?;
        let binary = storage::safe_path(&directory, Path::new(entrypoint))?;
        let member = members
            .iter()
            .find(|member| member["path"] == entrypoint)
            .ok_or("binary member absent")?;
        let bytes = checked_bytes(&binary, number(member, "size")?, text(member, "sha256")?)?;
        let machine = if architecture == "amd64" {
            62u16
        } else {
            183u16
        };
        if bytes.len() < 20
            || &bytes[..6] != b"\x7fELF\x02\x01"
            || u16::from_le_bytes([bytes[18], bytes[19]]) != machine
            || binary.metadata()?.permissions().mode() & 0o100 == 0
        {
            return Err("release entrypoint is not executable native ELF for this runner".into());
        }
        plugins.push(json!({"id":id,"version":version["manifest"]["version"],"revision":revision,
            "release_id":snapshot["id"],"asset":selected["name"],"asset_id":selected["id"],"package":package,
            "sha256":artifact["sha256"],"manifest_sha256":version["manifest_sha256"],
            "entrypoint":entrypoint,"binary":binary,"binary_sha256":member["sha256"],"architecture":architecture}));
    }
    download(&root, source, "source.tar.gz", 32 * 1024 * 1024)?;
    let inputs = json!({"schema_version":2,"kind":"published_release_inputs_not_installed",
        "repository_id":REPOSITORY,"publisher_id":PUBLISHER,"catalogue_revision":revision,
        "catalogue_sha256":catalogue_asset["sha256"],"catalogue_size":catalogue_asset["size"],
        "source_revision":revision,"release_id":snapshot["id"],"release_snapshot":snapshot,
        "release_binding":binding,"tag_chain":chain,"architecture":architecture,"plugins":plugins,
        "source_archive":source_name,"source_asset_id":source["id"],"source_size":source["size"],"source_sha256":source["sha256"]});
    observation::validate(&inputs, &catalogue, &architecture)?;
    repository(&root, "repository-final.json")?;
    let final_snapshot = latest(&root, "latest-final.json")?;
    let (final_binding, final_chain) = resolve(&root, &final_snapshot, "final")?;
    if inputs["release_snapshot"] != final_snapshot
        || inputs["release_binding"] != final_binding
        || inputs["tag_chain"] != final_chain
    {
        return Err(
            "official source/latest/assets/tag changed during preparation; review again".into(),
        );
    }
    let bytes = serde_json::to_vec_pretty(&inputs)?;
    if bytes.len() > 256 * 1024 {
        return Err("prepared release observation exceeds its byte bound".into());
    }
    storage::atomic_write(&root, Path::new("inputs.json"), &bytes, 0o600)?;
    verify(&root)?;
    println!(
        "Release inputs: {} (not installation or runtime acceptance)",
        root.display()
    );
    Ok(root)
}
