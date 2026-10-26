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

pub const SOURCE: &str = "aa1ec272d4ed6326338f3de0e5fbf30bbe181e39";
pub const CATALOGUE: &str = "86f784e6a0eb794cf070edb676207b7964b8317c";
pub const CATALOGUE_SHA256: &str =
    "510bdef3b532ed266d10fd925bf15a0d7d536c1889f2454e8d9a7cb446cf8a5b";
const TAG: &str = "v0.1.0-rc.1";
const TAG_OBJECT: &str = "31b3ad165bb6a685dfe49fcde082949e12e5887b";
const REPOSITORY: u64 = 1431055733;
const PUBLISHER: u64 = 339567100;
const RELEASE: u64 = 424255611;
const SOURCE_ASSET: u64 = 678182756;
const SOURCE_SIZE: u64 = 11598339;
const SOURCE_SHA256: &str = "1d7284490f0a336f9bc3ab76d43cbaf0d9bea576346595a8133d536a1600836c";

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

fn fetch(root: &Path, url: &str, name: &str, limit: u64, redirects: bool) -> Result<PathBuf> {
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
    if redirects {
        arguments.extend(["--location", "--max-redirs", "3"]);
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

pub fn checked_bytes(path: &Path, size: u64, digest: &str) -> Result<Vec<u8>> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_file() || metadata.len() != size || size > 32 * 1024 * 1024 {
        return Err("release input has the wrong file type or exact size".into());
    }
    let bytes = fs::read(path)?;
    if storage::sha256(bytes.as_slice())? != digest {
        return Err("release input SHA256 differs from its pinned declaration".into());
    }
    Ok(bytes)
}

fn metadata(root: &Path, endpoint: &str, name: &str) -> Result<Value> {
    let url = format!("https://api.github.com/{endpoint}");
    storage::parse_json(&fs::read(fetch(root, &url, name, 1024 * 1024, false)?)?)
}

fn asset<'a>(release: &'a Value, id: u64, size: u64, sha256: &str) -> Result<&'a Value> {
    let matches = release["assets"]
        .as_array()
        .ok_or("release assets absent")?
        .iter()
        .filter(|value| value["id"] == id)
        .collect::<Vec<_>>();
    let [selected] = matches.as_slice() else {
        return Err("release asset identity is missing or duplicated".into());
    };
    if number(selected, "size")? != size
        || text(selected, "digest")? != format!("sha256:{sha256}")
        || selected["state"] != "uploaded"
    {
        return Err("release asset size/digest/state differs from catalogue".into());
    }
    let name = text(selected, "name")?;
    storage::relative_path(Path::new(name))?;
    if name.contains('/')
        || text(selected, "browser_download_url")?
            != format!(
                "https://github.com/Among-Terminal/among-plugins/releases/download/{TAG}/{name}"
            )
    {
        return Err("release asset is not a direct member of the pinned public release".into());
    }
    Ok(selected)
}

pub fn select<'a>(catalogue: &'a Value, architecture: &str) -> Result<Vec<(&'a Value, &'a Value)>> {
    if !matches!(architecture, "amd64" | "arm64") || catalogue["schema_version"] != 1 {
        return Err("unsupported native release-input platform or catalogue schema".into());
    }
    let plugins = catalogue["plugins"]
        .as_array()
        .ok_or("catalogue plugins absent")?;
    let mut ids = BTreeSet::new();
    let mut selections = Vec::new();
    for plugin in plugins {
        let id = text(plugin, "id")?;
        if !matches!(id, "clock" | "system-status" | "pane-pwd") || !ids.insert(id) {
            return Err("official catalogue plugin set changed or is duplicated".into());
        }
        let versions = plugin["versions"]
            .as_array()
            .ok_or("catalogue versions absent")?;
        let [version] = versions.as_slice() else {
            return Err("expected one pinned plugin version".into());
        };
        if version["manifest"]["id"] != id
            || version["manifest"]["version"] != "0.1.0-rc.1"
            || version["revision"] != SOURCE
            || version["release_id"] != RELEASE
        {
            return Err(
                "binary revision/version/release binding differs from the pinned source".into(),
            );
        }
        let artifacts = version["artifacts"]
            .as_array()
            .ok_or("catalogue artifacts absent")?;
        if artifacts.len() != 2
            || artifacts
                .iter()
                .map(|a| text(a, "architecture"))
                .collect::<Result<BTreeSet<_>>>()?
                != BTreeSet::from(["amd64", "arm64"])
        {
            return Err("native architecture entries are missing or duplicated".into());
        }
        let selected = artifacts
            .iter()
            .find(|a| a["architecture"] == architecture)
            .ok_or("no package for the actual native architecture")?;
        if selected["os"] != "linux"
            || selected["runtime"] != json!({"kind":"native","libc":"glibc","minimum":"2.34"})
            || selected["format"] != "tar.gz"
            || selected["dependencies"] != json!([])
        {
            return Err("native package runtime contract changed".into());
        }
        selections.push((version, selected));
    }
    if ids != BTreeSet::from(["clock", "pane-pwd", "system-status"]) {
        return Err(
            "official catalogue does not contain exactly the three approved plugins".into(),
        );
    }
    Ok(selections)
}

pub fn verify(root: &Path) -> Result<Value> {
    let root = root.canonicalize()?;
    let catalogue = storage::parse_json(&checked_bytes(
        &root.join("among-plugins.json"),
        634525,
        CATALOGUE_SHA256,
    )?)?;
    let inputs = storage::parse_json(&fs::read(root.join("inputs.json"))?)?;
    let architecture = match std::env::consts::ARCH {
        "x86_64" => "amd64",
        "aarch64" => "arm64",
        _ => return Err("unsupported native architecture".into()),
    };
    if inputs["schema_version"] != 1
        || inputs["kind"] != "published_release_inputs_not_installed"
        || inputs["catalogue_revision"] != CATALOGUE
        || inputs["catalogue_sha256"] != CATALOGUE_SHA256
        || inputs["source_revision"] != SOURCE
        || inputs["release_id"] != RELEASE
        || inputs["repository_id"] != REPOSITORY
        || inputs["publisher_id"] != PUBLISHER
        || inputs["tag"] != TAG
        || inputs["tag_object"] != TAG_OBJECT
        || inputs["architecture"] != architecture
        || inputs["plugins"].as_array().map(Vec::len) != Some(3)
    {
        return Err("prepared inputs have stale or substituted provenance".into());
    }
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
            || plugin["revision"] != SOURCE
            || plugin["release_id"] != RELEASE
            || plugin["asset_id"] != artifact["asset_id"]
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
    }
    if inputs["source_asset_id"] != SOURCE_ASSET || inputs["source_sha256"] != SOURCE_SHA256 {
        return Err("prepared shared source binding was substituted".into());
    }
    checked_bytes(&root.join("source.tar.gz"), SOURCE_SIZE, SOURCE_SHA256)?;
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

pub fn prepare(destination: &Path) -> Result<PathBuf> {
    fs::DirBuilder::new().mode(0o700).create(destination)?;
    let root = destination.canonicalize()?;
    let architecture = match std::env::consts::ARCH {
        "x86_64" => "amd64",
        "aarch64" => "arm64",
        _ => return Err("unsupported native architecture".into()),
    };
    let repository = metadata(&root, "repositories/1431055733", "repository.json")?;
    if repository["id"] != REPOSITORY
        || repository["owner"]["id"] != PUBLISHER
        || repository["private"] != false
        || repository["full_name"] != "Among-Terminal/among-plugins"
    {
        return Err("official repository identity changed".into());
    }
    let release = metadata(
        &root,
        "repositories/1431055733/releases/424255611",
        "release.json",
    )?;
    if release["id"] != RELEASE
        || release["tag_name"] != TAG
        || release["draft"] != false
        || release["prerelease"] != true
        || release["assets"].as_array().map(Vec::len) != Some(7)
    {
        return Err("pinned public plugin prerelease identity/state changed".into());
    }
    let reference = metadata(
        &root,
        &format!("repositories/{REPOSITORY}/git/ref/tags/{TAG}"),
        "tag-ref.json",
    )?;
    let tag = metadata(
        &root,
        &format!("repositories/{REPOSITORY}/git/tags/{TAG_OBJECT}"),
        "tag-object.json",
    )?;
    if reference["ref"] != format!("refs/tags/{TAG}")
        || reference["object"]["type"] != "tag"
        || reference["object"]["sha"] != TAG_OBJECT
        || tag["sha"] != TAG_OBJECT
        || tag["tag"] != TAG
        || tag["object"]["type"] != "commit"
        || tag["object"]["sha"] != SOURCE
        || tag["verification"]["verified"] != true
    {
        return Err(
            "signed annotated release tag no longer resolves to the exact binary source".into(),
        );
    }
    let catalogue_path = fetch(&root, &format!("https://raw.githubusercontent.com/Among-Terminal/among-plugins/{CATALOGUE}/among-plugins.json"), "among-plugins.json", 634525, false)?;
    let catalogue =
        storage::parse_json(&checked_bytes(&catalogue_path, 634525, CATALOGUE_SHA256)?)?;
    let mut plugins = Vec::new();
    for (version, artifact) in select(&catalogue, architecture)? {
        // Validate both native members even though this runner executes only its own.
        for declared in version["artifacts"].as_array().ok_or("artifacts absent")? {
            asset(
                &release,
                number(declared, "asset_id")?,
                number(declared, "size")?,
                text(declared, "sha256")?,
            )?;
        }
        let selected = asset(
            &release,
            number(artifact, "asset_id")?,
            number(artifact, "size")?,
            text(artifact, "sha256")?,
        )?;
        let id = text(&version["manifest"], "id")?;
        let package = fetch(
            &root,
            text(selected, "browser_download_url")?,
            &format!("{id}.tar.gz"),
            number(artifact, "size")?,
            true,
        )?;
        checked_bytes(
            &package,
            number(artifact, "size")?,
            text(artifact, "sha256")?,
        )?;
        let directory = unpack(&root, &package, id, artifact)?;
        let manifest = checked_bytes(
            &directory.join("among-plugin.json"),
            number(
                artifact["members"]
                    .as_array()
                    .ok_or("members absent")?
                    .iter()
                    .find(|m| m["path"] == "among-plugin.json")
                    .ok_or("manifest member absent")?,
                "size",
            )?,
            text(version, "manifest_sha256")?,
        )?;
        if storage::parse_json(&manifest)? != version["manifest"] {
            return Err("package manifest differs from catalogue".into());
        }
        let entrypoint = text(&version["manifest"], "entrypoint")?;
        let binary = storage::safe_path(&directory, Path::new(entrypoint))?;
        let binary_member = artifact["members"]
            .as_array()
            .ok_or("members absent")?
            .iter()
            .find(|m| m["path"] == entrypoint)
            .ok_or("binary member absent")?;
        let bytes = checked_bytes(
            &binary,
            number(binary_member, "size")?,
            text(binary_member, "sha256")?,
        )?;
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
            return Err(
                "release binary is not an executable ELF for the actual native runner".into(),
            );
        }
        plugins.push(json!({"id":id,"version":version["manifest"]["version"],"revision":SOURCE,
            "release_id":RELEASE,"asset_id":selected["id"],"package":package,"sha256":artifact["sha256"],
            "manifest_sha256":version["manifest_sha256"],"entrypoint":entrypoint,"binary":binary,
            "binary_sha256":binary_member["sha256"],"architecture":architecture}));
    }
    let source = asset(&release, SOURCE_ASSET, SOURCE_SIZE, SOURCE_SHA256)?;
    let source_path = fetch(
        &root,
        text(source, "browser_download_url")?,
        "source.tar.gz",
        SOURCE_SIZE,
        true,
    )?;
    checked_bytes(&source_path, SOURCE_SIZE, SOURCE_SHA256)?;
    let inputs = json!({"schema_version":1,"kind":"published_release_inputs_not_installed",
        "repository_id":REPOSITORY,"publisher_id":PUBLISHER,"catalogue_revision":CATALOGUE,
        "catalogue_sha256":CATALOGUE_SHA256,"source_revision":SOURCE,"release_id":RELEASE,
        "tag":TAG,"tag_object":TAG_OBJECT,"architecture":architecture,"plugins":plugins,
        "source_asset_id":SOURCE_ASSET,"source_sha256":SOURCE_SHA256});
    storage::atomic_write(
        &root,
        Path::new("inputs.json"),
        &serde_json::to_vec_pretty(&inputs)?,
        0o600,
    )?;
    println!(
        "Release inputs: {} (not installation or runtime acceptance)",
        root.display()
    );
    Ok(root)
}
