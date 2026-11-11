//! immutable release inputs for native checks, not an alternative plugin installer.
//! only the application may turn these packages into installed selections/grants.

use crate::{process::OwnedProcess, storage, Result};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::Duration;

#[cfg(test)]
pub(crate) mod fixture;
mod observation;
pub use observation::select;
pub mod published;

pub const REPOSITORY: u64 = 1431055733;
pub const PUBLISHER: u64 = 339567100;
// Synthetic facts of a CI-built candidate; no such release exists.
const CANDIDATE_RELEASE: u64 = 900_000;
const CANDIDATE_TAG: &str = "v0.2.0";
const FETCH_WAIT: Duration = Duration::from_secs(65);
const CURL_LOG_TAIL: u64 = 4096;
const CURL_LOG_TEXT: usize = 8192;

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
    fetch_with(
        Path::new("/usr/bin/curl"),
        FETCH_WAIT,
        root,
        url,
        name,
        limit,
        asset,
    )
}

fn fetch_with(
    program: &Path,
    wait: Duration,
    root: &Path,
    url: &str,
    name: &str,
    limit: u64,
    asset: bool,
) -> Result<PathBuf> {
    let destination = storage::safe_path(root, Path::new(name))?;
    let output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&destination)?;
    // The log is named after the whole file name: replacing the extension would
    // give among-plugins.json and among-plugins.sig the same log.
    let mut log_name = destination
        .file_name()
        .ok_or("download target has no file name")?
        .to_owned();
    log_name.push(".curl.log");
    let log_path = destination.with_file_name(log_name);
    let log = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&log_path)?;
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
    let mut child =
        OwnedProcess::spawn_split(program, &arguments, &environment, root, &output, &log)?;
    let outcome = match child.wait(wait) {
        Err(error) => DownloadOutcome::WaitFailed {
            error: error.to_string(),
        },
        Ok(status) => {
            let measured = destination.metadata();
            let length = measured.as_ref().ok().map(|metadata| metadata.len());
            let size_error = measured.err().map(|error| error.to_string());
            if status.success() {
                match length {
                    Some(length) if length <= limit => return Ok(destination),
                    Some(length) => DownloadOutcome::Oversize { length },
                    None => DownloadOutcome::SizeUnavailable {
                        error: size_error.unwrap_or_else(|| "unknown".into()),
                    },
                }
            } else {
                DownloadOutcome::Exit {
                    status: status.to_string(),
                    length,
                    size_error,
                }
            }
        }
    };
    Err(download_failure(name, &log_path, limit, wait, &outcome).into())
}

enum DownloadOutcome {
    WaitFailed {
        error: String,
    },
    Exit {
        status: String,
        length: Option<u64>,
        size_error: Option<String>,
    },
    Oversize {
        length: u64,
    },
    SizeUnavailable {
        error: String,
    },
}

struct LogTail {
    text: String,
    read: u64,
    omitted: u64,
}

fn download_failure(
    name: &str,
    log_path: &Path,
    limit: u64,
    wait: Duration,
    outcome: &DownloadOutcome,
) -> String {
    let mut message = format!("public release download failed; retained {name}");
    match outcome {
        DownloadOutcome::WaitFailed { error } => {
            message.push_str(&format!(
                "; process wait failed with wait limit {wait:?}: {error}"
            ));
        }
        DownloadOutcome::Exit {
            status,
            length,
            size_error,
        } => {
            message.push_str(&format!("; curl exited with {status}"));
            match length {
                Some(length) if *length > limit => message.push_str(&format!(
                    "; downloaded {length} bytes, over the {limit} byte limit"
                )),
                Some(_) => {}
                None => {
                    if let Some(error) = size_error {
                        message.push_str(&format!("; downloaded size unavailable: {error}"));
                    }
                }
            }
        }
        DownloadOutcome::Oversize { length } => message.push_str(&format!(
            "; downloaded {length} bytes, over the {limit} byte limit"
        )),
        DownloadOutcome::SizeUnavailable { error } => {
            message.push_str(&format!("; downloaded size unavailable: {error}"));
        }
    }
    message.push_str(&format!("; log {}", log_path.display()));
    match read_log_tail(log_path) {
        Ok(tail) if tail.text.is_empty() => {
            if tail.read == 0 {
                message.push_str("; curl log is empty");
            } else {
                message.push_str("; curl log has no readable text");
            }
        }
        Ok(tail) => {
            let omitted = if tail.omitted > 0 {
                format!(", first {} bytes omitted", tail.omitted)
            } else {
                String::new()
            };
            message.push_str(&format!(
                "; stderr tail (last {} bytes{}):\n{}",
                tail.read,
                omitted,
                bounded_lines(&tail.text)
            ));
        }
        Err(error) => message.push_str(&format!("; curl log unavailable: {error}")),
    }
    message.replace("##[", "##\\[")
}

fn read_log_tail(path: &Path) -> std::result::Result<LogTail, String> {
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(nix::libc::O_NOFOLLOW | nix::libc::O_CLOEXEC | nix::libc::O_NONBLOCK)
        .open(path)
        .map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                "missing".to_owned()
            } else {
                format!("unreadable: {error}")
            }
        })?;
    let metadata = file
        .metadata()
        .map_err(|error| format!("unreadable: {error}"))?;
    if !metadata.is_file() {
        return Err("not a regular file".to_owned());
    }
    read_tail(&mut file, metadata.len())
}

fn read_tail<R: Read + Seek>(reader: &mut R, length: u64) -> std::result::Result<LogTail, String> {
    let start = length.saturating_sub(CURL_LOG_TAIL);
    reader
        .seek(SeekFrom::Start(start))
        .map_err(|error| format!("unreadable: {error}"))?;
    let mut bytes = Vec::new();
    let mut limited = (&mut *reader).take(CURL_LOG_TAIL);
    limited
        .read_to_end(&mut bytes)
        .map_err(|error| format!("unreadable: {error}"))?;
    let mut offset = 0;
    while bytes.get(offset).is_some_and(|byte| byte & 0xC0 == 0x80) {
        offset += 1;
    }
    let mut text = String::new();
    for character in String::from_utf8_lossy(&bytes[offset..]).chars() {
        match character {
            '\n' | '\t' => text.push(character),
            control if control.is_control() => text.extend(control.escape_unicode()),
            other => text.push(other),
        }
    }
    Ok(LogTail {
        text,
        read: bytes.len() as u64,
        omitted: start + offset as u64,
    })
}

fn bounded_lines(text: &str) -> String {
    let mut output = String::new();
    for line in text.split('\n') {
        if !output.is_empty() {
            output.push('\n');
        }
        output.push_str("  | ");
        output.push_str(line);
    }
    if output.len() <= CURL_LOG_TEXT {
        return output;
    }
    let mut start = output.len() - CURL_LOG_TEXT;
    while !output.is_char_boundary(start) {
        start += 1;
    }
    let mut bounded = String::from("  | ... (diagnostic truncated)\n  | ");
    bounded.push_str(&output[start..]);
    bounded
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
    check_repository(&repository)
}

fn check_repository(repository: &Value) -> Result<()> {
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
    resolve_with(root, snapshot, pass, metadata)
}

fn resolve_with(
    root: &Path,
    snapshot: &Value,
    pass: &str,
    load: impl Fn(&Path, &str, &str) -> Result<Value>,
) -> Result<(Value, Value)> {
    let tag = text(snapshot, "tag")?;
    let reference = load(
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
            let commit = load(
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
        let annotation = load(
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

/// Which published release to observe.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Channel {
    Latest,
    Prerelease,
}

// The newest non-draft prerelease by creation time, then numeric ID, within
// three pages. GitHub's timestamps share one fixed format, so they sort as text.
fn newest_prerelease(root: &Path, name: &str) -> Result<Value> {
    let mut newest: Option<(String, u64)> = None;
    for page in 1..=3 {
        let listed = metadata(
            root,
            &format!("repositories/{REPOSITORY}/releases?per_page=100&page={page}"),
            &format!("{name}-page-{page}.json"),
        )?;
        let entries = listed.as_array().ok_or("release list is not an array")?;
        for entry in entries {
            if entry["prerelease"] == true && entry["draft"] == false {
                let candidate = (text(entry, "created_at")?.to_owned(), number(entry, "id")?);
                if newest.as_ref().is_none_or(|best| candidate > *best) {
                    newest = Some(candidate);
                }
            }
        }
        if entries.len() < 100 {
            break;
        }
    }
    let (_, id) = newest.ok_or("no published prerelease was found within three pages")?;
    observation::prerelease_snapshot(&metadata(
        root,
        &format!("repositories/{REPOSITORY}/releases/{id}"),
        name,
    )?)
}

fn observe(root: &Path, name: &str, channel: Channel) -> Result<Value> {
    match channel {
        Channel::Latest => latest(root, name),
        Channel::Prerelease => newest_prerelease(root, name),
    }
}

pub fn prepare(destination: &Path) -> Result<PathBuf> {
    prepare_channel(destination, Channel::Latest)
}

pub fn prepare_channel(destination: &Path, channel: Channel) -> Result<PathBuf> {
    fs::DirBuilder::new().mode(0o700).create(destination)?;
    let root = destination.canonicalize()?;
    let architecture = crate::platform::discover()?.architecture;
    repository(&root, "repository.json")?;
    let snapshot = observe(&root, "latest.json", channel)?;
    let catalogue_asset = observation::asset(&snapshot, "among-plugins.json")?;
    let catalogue_path = download(&root, catalogue_asset, "among-plugins.json", 1024 * 1024)?;
    let catalogue = storage::parse_json(&checked_bytes(
        &catalogue_path,
        number(catalogue_asset, "size")?,
        text(catalogue_asset, "sha256")?,
    )?)?;
    let (binding, chain) = resolve(&root, &snapshot, "initial")?;
    let (inputs, catalogue) = assemble(
        &root,
        &architecture,
        &snapshot,
        catalogue,
        (&binding, &chain),
        |asset, name, maximum| download(&root, asset, name, maximum),
    )?;
    observation::validate(&inputs, &catalogue, &architecture)?;
    repository(&root, "repository-final.json")?;
    let final_snapshot = observe(&root, "latest-final.json", channel)?;
    let (final_binding, final_chain) = resolve(&root, &final_snapshot, "final")?;
    if inputs["release_snapshot"] != final_snapshot
        || inputs["release_binding"] != final_binding
        || inputs["tag_chain"] != final_chain
    {
        return Err(
            "official source/latest/assets/tag changed during preparation; review again".into(),
        );
    }
    seal(&root, &inputs)
}

/// The same input directory as `prepare`, from a complete release candidate
/// built in CI instead of a published release. The release documents are
/// synthetic and marked `candidate`; they still pass the ordinary stable
/// observation, so nothing downstream is relaxed. Host tests serve them over
/// their controlled HTTPS server. This is candidate acceptance, not acceptance
/// of published bytes.
pub fn prepare_candidate(assets: &Path, destination: &Path) -> Result<PathBuf> {
    fs::DirBuilder::new().mode(0o700).create(destination)?;
    let root = destination.canonicalize()?;
    let assets = assets.canonicalize()?;
    let architecture = crate::platform::discover()?.architecture;
    let mut names = fs::read_dir(&assets)?
        .map(|entry| {
            Ok(entry?
                .file_name()
                .into_string()
                .map_err(|_| "non-UTF8 asset name")?)
        })
        .collect::<Result<Vec<_>>>()?;
    names.sort();
    let mut entries = Vec::new();
    for (index, name) in names.iter().enumerate() {
        let bytes = regular_bytes(&assets.join(name), 64 * 1024 * 1024)?;
        entries.push(
            json!({"id":CANDIDATE_RELEASE + 1 + index as u64,"name":name,"size":bytes.len(),
            "state":"uploaded","digest":format!("sha256:{}",storage::sha256(bytes.as_slice())?)}),
        );
    }
    let document = json!({"id":CANDIDATE_RELEASE,"tag_name":CANDIDATE_TAG,"draft":false,
        "prerelease":false,"immutable":true,"created_at":"2026-10-30T00:00:00Z","assets":entries});
    let snapshot = observation::snapshot(&document)?;
    let catalogue_asset = observation::asset(&snapshot, "among-plugins.json")?;
    let catalogue = storage::parse_json(&checked_bytes(
        &assets.join("among-plugins.json"),
        number(catalogue_asset, "size")?,
        text(catalogue_asset, "sha256")?,
    )?)?;
    let revision = catalogue["plugins"][0]["versions"][0]["revision"]
        .as_str()
        .ok_or("candidate catalogue names no revision")?
        .to_owned();
    let binding = json!({"tag":CANDIDATE_TAG,"object":revision,"commit":revision});
    let chain = json!([["commit", revision]]);
    fs::copy(
        assets.join("among-plugins.json"),
        root.join("among-plugins.json"),
    )?;
    let (mut inputs, catalogue) = assemble(
        &root,
        &architecture,
        &snapshot,
        catalogue,
        (&binding, &chain),
        |asset, name, _| {
            let target = storage::safe_path(&root, Path::new(name))?;
            fs::copy(assets.join(text(asset, "name")?), &target)?;
            checked_bytes(&target, number(asset, "size")?, text(asset, "sha256")?)?;
            Ok(target)
        },
    )?;
    inputs["candidate"] = json!(true);
    observation::validate(&inputs, &catalogue, &architecture)?;
    for (name, value) in [
        ("latest.json", document),
        (
            "repository.json",
            json!({"id":REPOSITORY,"private":false,"name":"among-plugins",
                "full_name":"Among-Terminal/among-plugins",
                "owner":{"id":PUBLISHER,"login":"Among-Terminal"}}),
        ),
        (
            "initial-ref.json",
            json!({"ref":format!("refs/tags/{CANDIDATE_TAG}"),"object":{"type":"commit","sha":revision}}),
        ),
        ("initial-commit.json", json!({"sha":revision})),
    ] {
        storage::atomic_write(&root, Path::new(name), &serde_json::to_vec(&value)?, 0o600)?;
    }
    seal(&root, &inputs)
}

fn seal(root: &Path, inputs: &Value) -> Result<PathBuf> {
    let bytes = serde_json::to_vec_pretty(inputs)?;
    if bytes.len() > 256 * 1024 {
        return Err("prepared release observation exceeds its byte bound".into());
    }
    storage::atomic_write(root, Path::new("inputs.json"), &bytes, 0o600)?;
    verify(root)?;
    println!(
        "Release inputs: {} (not installation or runtime acceptance)",
        root.display()
    );
    Ok(root.to_path_buf())
}

// Everything derived from one observed release; `transfer` supplies its bytes.
fn assemble(
    root: &Path,
    architecture: &str,
    snapshot: &Value,
    catalogue: Value,
    (binding, chain): (&Value, &Value),
    transfer: impl Fn(&Value, &str, u64) -> Result<PathBuf>,
) -> Result<(Value, Value)> {
    let catalogue_asset = observation::asset(snapshot, "among-plugins.json")?;
    let revision = text(binding, "commit")?;
    let source_name = format!("source-{revision}.tar.gz");
    let source = observation::asset(snapshot, &source_name)?;
    let selections = select(&catalogue, architecture)?;
    // Both native declarations must bind even though this runner transfers one.
    for (version, _) in &selections {
        if version["revision"] != revision {
            return Err("catalogue source differs from the resolved release tag".into());
        }
        for artifact in version["artifacts"].as_array().ok_or("artifacts absent")? {
            let observed = observation::asset(snapshot, text(artifact, "asset")?)?;
            if observed["size"] != artifact["size"] || observed["sha256"] != artifact["sha256"] {
                return Err("catalogue package differs from the observed release asset".into());
            }
        }
    }
    let mut plugins = Vec::new();
    for (version, artifact) in selections {
        let id = text(&version["manifest"], "id")?;
        let selected = observation::asset(snapshot, text(artifact, "asset")?)?;
        let package = transfer(selected, &format!("{id}.tar.gz"), 64 * 1024 * 1024)?;
        let directory = unpack(root, &package, id, artifact)?;
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
    transfer(source, "source.tar.gz", 32 * 1024 * 1024)?;
    let inputs = json!({"schema_version":2,"kind":"published_release_inputs_not_installed",
        "repository_id":REPOSITORY,"publisher_id":PUBLISHER,"catalogue_revision":revision,
        "catalogue_sha256":catalogue_asset["sha256"],"catalogue_size":catalogue_asset["size"],
        "source_revision":revision,"release_id":snapshot["id"],"release_snapshot":snapshot,
        "release_binding":binding,"tag_chain":chain,"architecture":architecture,"plugins":plugins,
        "source_archive":source_name,"source_asset_id":source["id"],"source_size":source["size"],"source_sha256":source["sha256"]});
    Ok((inputs, catalogue))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::{self, File};
    use std::io::{self, Read, Seek, SeekFrom, Write};
    use std::os::unix::fs::DirBuilderExt;
    use std::path::{Path, PathBuf};
    use std::time::Duration;

    fn fixture() -> Result<PathBuf> {
        let root = crate::scratch::root()?;
        storage::private_directory(&root, "fetch-diagnostic")
    }

    fn fake_curl(root: &Path, body: &str) -> Result<PathBuf> {
        let bin = root.join("bin");
        fs::DirBuilder::new().mode(0o700).create(&bin)?;
        let path = bin.join("curl");
        crate::test_executable::write(&path, format!("#!/bin/sh\n{body}\n").as_bytes())?;
        Ok(path)
    }

    fn failure(result: Result<PathBuf>) -> String {
        match result {
            Ok(path) => format!("unexpected success at {}", path.display()),
            Err(error) => error.to_string(),
        }
    }

    fn run(root: &Path, program: &Path, name: &str, limit: u64, asset: bool) -> Result<PathBuf> {
        fetch_with(
            program,
            Duration::from_secs(2),
            root,
            "https://api.github.com/repositories/1/releases/latest",
            name,
            limit,
            asset,
        )
    }

    #[test]
    fn fetch_keeps_the_exact_curl_policy_argv_for_metadata_and_assets() -> Result<()> {
        let root = fixture()?;
        let program = fake_curl(
            &root,
            "for arg in \"$@\"; do printf '%s\\n' \"$arg\" >> argv.log; done\nprintf 'ok'",
        )?;
        let metadata = run(&root, &program, "repository.json", 1024, false)?;
        assert_eq!(fs::read(&metadata)?, b"ok");
        let observed = fs::read_to_string(root.join("argv.log"))?;
        let expected = [
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
            "1024",
            "https://api.github.com/repositories/1/releases/latest",
        ];
        assert_eq!(observed.lines().collect::<Vec<_>>(), expected.to_vec());
        fs::remove_file(root.join("argv.log"))?;
        let asset = run(&root, &program, "asset.bin", 2048, true)?;
        assert_eq!(fs::read(&asset)?, b"ok");
        let observed = fs::read_to_string(root.join("argv.log"))?;
        let expected = [
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
            "2048",
            "--location",
            "--max-redirs",
            "3",
            "--header",
            "Accept: application/octet-stream",
            "https://api.github.com/repositories/1/releases/latest",
        ];
        assert_eq!(observed.lines().collect::<Vec<_>>(), expected.to_vec());
        Ok(())
    }

    #[test]
    fn nonzero_exit_keeps_the_status_and_a_bounded_utf8_tail() -> Result<()> {
        let root = fixture()?;
        let mut payload = vec![b'a'; 5000];
        payload.extend_from_slice(b"TAIL-MARKER\n");
        fs::write(root.join("payload"), payload)?;
        let program = fake_curl(&root, "cat payload >&2\nexit 22")?;
        let message = failure(run(&root, &program, "repository.json", 1024, false));
        assert!(message.contains("public release download failed; retained repository.json"));
        assert!(message.contains("curl exited with exit status: 22"));
        assert!(message.contains("stderr tail"));
        assert!(message.contains("bytes omitted"));
        assert!(message.contains("TAIL-MARKER"));
        let log = root.join("repository.json.curl.log").display().to_string();
        assert!(message.contains(log.as_str()));
        assert!(message.len() < 16 * 1024);
        Ok(())
    }

    #[test]
    fn process_wait_error_keeps_the_original_timeout_text() -> Result<()> {
        let root = fixture()?;
        let program = fake_curl(&root, "sleep 5\nprintf 'late'")?;
        let message = failure(fetch_with(
            &program,
            Duration::from_millis(200),
            &root,
            "https://api.github.com/repositories/1/releases/latest",
            "repository.json",
            1024,
            false,
        ));
        assert!(message.contains("process wait failed with wait limit 200ms:"));
        assert!(message.contains("owned process timed out; its process group was stopped"));
        assert!(!message.contains("curl exited with"));
        Ok(())
    }

    #[test]
    fn oversize_success_names_the_size_and_the_limit() -> Result<()> {
        let root = fixture()?;
        let program = fake_curl(&root, "head -c 1025 /dev/zero")?;
        let message = failure(run(&root, &program, "repository.json", 1024, false));
        assert!(message.contains("downloaded 1025 bytes, over the 1024 byte limit"));
        assert!(message.contains("curl log is empty"));
        assert!(!message.contains("curl exited with"));
        Ok(())
    }

    #[test]
    fn nonzero_exit_and_oversize_keep_both_facts() -> Result<()> {
        let root = fixture()?;
        let program = fake_curl(&root, "echo err >&2\nhead -c 1025 /dev/zero\nexit 7")?;
        let message = failure(run(&root, &program, "repository.json", 1024, false));
        assert!(message.contains("curl exited with exit status: 7"));
        assert!(message.contains("downloaded 1025 bytes, over the 1024 byte limit"));
        assert!(message.contains("err"));
        Ok(())
    }

    #[test]
    fn missing_log_does_not_replace_the_original_exit() -> Result<()> {
        let root = fixture()?;
        let program = fake_curl(
            &root,
            "log=$(readlink /proc/self/fd/2) || exit 9\nrm -f \"$log\"\nexit 1",
        )?;
        let message = failure(run(&root, &program, "repository.json", 1024, false));
        assert!(message.contains("curl exited with exit status: 1"));
        assert!(message.contains("curl log unavailable: missing"));
        assert!(message.contains("retained repository.json"));
        Ok(())
    }

    #[test]
    fn a_directory_in_place_of_the_log_is_reported_without_hiding_the_exit() -> Result<()> {
        let root = fixture()?;
        let program = fake_curl(
            &root,
            "log=$(readlink /proc/self/fd/2) || exit 9\nrm -f \"$log\"\nmkdir \"$log\"\nexit 1",
        )?;
        let message = failure(run(&root, &program, "repository.json", 1024, false));
        assert!(message.contains("curl exited with exit status: 1"));
        assert!(message.contains("curl log unavailable: not a regular file"));
        Ok(())
    }

    #[test]
    fn an_empty_log_keeps_the_original_exit() -> Result<()> {
        let root = fixture()?;
        let program = fake_curl(&root, "exit 1")?;
        let message = failure(run(&root, &program, "repository.json", 1024, false));
        assert!(message.contains("curl exited with exit status: 1"));
        assert!(message.contains("curl log is empty"));
        Ok(())
    }

    #[test]
    fn untrusted_stderr_cannot_inject_a_workflow_command() -> Result<()> {
        let root = fixture()?;
        fs::write(
            root.join("payload"),
            b"::error::injected\n::set-output name=x::y\n\x1b[31mred\rplain\xffmarker\n",
        )?;
        let program = fake_curl(&root, "cat payload >&2\nexit 1")?;
        let message = failure(run(&root, &program, "repository.json", 1024, false));
        assert!(!message.contains('\u{1b}'));
        assert!(!message.contains('\r'));
        assert!(message.contains("\\u{1b}"));
        assert!(message.contains("\\u{d}"));
        assert!(message.contains("plain\u{fffd}marker"));
        assert!(message.contains("  | ::error::injected"));
        assert!(message.contains("  | ::set-output name=x::y"));
        for line in message.lines() {
            assert!(
                !line.trim_start().starts_with("::"),
                "workflow command line: {line}"
            );
        }
        Ok(())
    }

    #[test]
    fn legacy_delimiters_are_neutralized_in_normal_and_truncated_excerpts() -> Result<()> {
        let normal = fixture()?;
        fs::write(
            normal.join("payload"),
            b"##[error]legacy\n##[stop-commands]token\n",
        )?;
        let program = fake_curl(&normal, "cat payload >&2\nexit 1")?;
        let message = failure(run(&normal, &program, "repository.json", 1024, false));
        assert!(!message.contains("##["));
        assert!(message.contains("##\\[error]legacy"));
        assert!(message.contains("##\\[stop-commands]token"));

        let truncated = fixture()?;
        let mut payload = vec![0x1b; 4000];
        payload.extend_from_slice(b"##[error]late\n");
        fs::write(truncated.join("payload"), payload)?;
        let program = fake_curl(&truncated, "cat payload >&2\nexit 1")?;
        let message = failure(run(&truncated, &program, "repository.json", 1024, false));
        assert!(!message.contains("##["));
        assert!(message.contains("##\\[error]late"));
        assert!(message.contains("(diagnostic truncated)"));
        Ok(())
    }

    #[test]
    fn a_tail_cut_inside_a_multibyte_character_skips_the_continuation_bytes() -> Result<()> {
        let root = fixture()?;
        let mut payload = vec![b'a'; 4090];
        payload.extend_from_slice("🦀".as_bytes());
        payload.extend(std::iter::repeat_n(b'b', 4086));
        payload.extend_from_slice(b"MARKER\n");
        assert_eq!(payload.len(), 8187);
        fs::write(root.join("payload"), payload)?;
        let program = fake_curl(&root, "cat payload >&2\nexit 1")?;
        let message = failure(run(&root, &program, "repository.json", 1024, false));
        assert!(!message.contains('\u{fffd}'));
        assert!(message.contains("MARKER"));
        assert!(message.contains("bytes omitted"));
        Ok(())
    }

    struct Counting<R> {
        inner: R,
        read: usize,
    }

    impl<R: Read> Read for Counting<R> {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            let count = self.inner.read(buffer)?;
            self.read += count;
            Ok(count)
        }
    }

    impl<R: Seek> Seek for Counting<R> {
        fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
            self.inner.seek(position)
        }
    }

    #[test]
    fn a_modest_sparse_log_is_read_only_through_the_tail_limit() -> Result<()> {
        let root = fixture()?;
        let path = root.join("large.log");
        let mut file = File::create(&path)?;
        file.set_len(1024 * 1024)?;
        file.seek(SeekFrom::End(-7))?;
        file.write_all(b"MARKER\n")?;
        file.sync_all()?;
        let mut counting = Counting {
            inner: File::open(&path)?,
            read: 0,
        };
        let tail = read_tail(&mut counting, 1024 * 1024)?;
        assert!(counting.read <= CURL_LOG_TAIL as usize);
        assert!(tail.omitted > 0);
        let block = bounded_lines(&tail.text);
        assert!(block.contains("MARKER"));
        assert!(block.len() <= CURL_LOG_TEXT + 64);
        Ok(())
    }
}
