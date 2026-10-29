use super::{number, text, PUBLISHER, REPOSITORY};
use crate::{storage, Result};
use serde_json::{json, Value};
use std::collections::BTreeSet;

pub(super) fn hex(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn asset_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 255
        && !matches!(name, "." | "..")
        && !name.contains(['/', '\\'])
        && name.chars().all(|character| !character.is_control())
}

pub(super) fn tag_name(tag: &str) -> bool {
    !tag.is_empty()
        && tag.len() <= 256
        && tag.split('/').count() <= 16
        && !tag.contains("..")
        && tag.split('/').all(|part| {
            !part.is_empty()
                && !part.starts_with('.')
                && !part.ends_with('.')
                && !part.ends_with(".lock")
                && part
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
        })
}

pub(super) fn snapshot(release: &Value) -> Result<Value> {
    if release["draft"] != false
        || release["prerelease"] != false
        || release["immutable"] != true
        || number(release, "id")? == 0
        || !tag_name(text(release, "tag_name")?)
    {
        return Err("published inputs require an ordinary stable immutable release".into());
    }
    let entries = release["assets"]
        .as_array()
        .ok_or("release assets absent")?;
    if entries.is_empty() || entries.len() > 64 {
        return Err("release needs a complete inventory of 1..=64 assets".into());
    }
    let mut ids = BTreeSet::new();
    let mut names = BTreeSet::new();
    let mut assets = Vec::new();
    for entry in entries {
        let id = number(entry, "id")?;
        let name = text(entry, "name")?;
        let digest = text(entry, "digest")?
            .strip_prefix("sha256:")
            .filter(|value| hex(value, 64))
            .ok_or("every release asset needs a lowercase SHA256 digest")?;
        if id == 0
            || !ids.insert(id)
            || !asset_name(name)
            || !names.insert(name)
            || entry["state"] != "uploaded"
        {
            return Err("release asset identity/name/state is invalid or duplicated".into());
        }
        assets.push(json!({"id":id,"name":name,"size":number(entry,"size")?,"sha256":digest}));
    }
    if !names.contains("among-plugins.json") {
        return Err("release lacks its unique among-plugins.json asset".into());
    }
    assets.sort_by(|left, right| left["name"].as_str().cmp(&right["name"].as_str()));
    Ok(json!({"id":release["id"],"tag":release["tag_name"],"assets":assets}))
}

pub(super) fn validate_snapshot(value: &Value) -> Result<()> {
    let entries = value["assets"].as_array().ok_or("stored assets absent")?;
    if entries.is_empty() || entries.len() > 64 {
        return Err("stored release inventory exceeds its bounds".into());
    }
    let assets = entries
        .iter()
        .map(|entry| {
            Ok(
                json!({"id":entry["id"],"name":entry["name"],"size":entry["size"],
                "state":"uploaded","digest":format!("sha256:{}",text(entry,"sha256")?)}),
            )
        })
        .collect::<Result<Vec<_>>>()?;
    let canonical = snapshot(&json!({"id":value["id"],"tag_name":value["tag"],
        "draft":false,"prerelease":false,"immutable":true,"assets":assets}))?;
    if canonical != *value {
        return Err("stored release snapshot is not complete canonical data".into());
    }
    Ok(())
}

pub(super) fn asset<'a>(snapshot: &'a Value, name: &str) -> Result<&'a Value> {
    snapshot["assets"]
        .as_array()
        .ok_or("stored release inventory absent")?
        .iter()
        .find(|entry| entry["name"] == name)
        .ok_or_else(|| "exact asset name is absent from the observed release".into())
}

pub(super) fn validate_chain(inputs: &Value) -> Result<()> {
    let binding = &inputs["release_binding"];
    let chain = inputs["tag_chain"].as_array().ok_or("tag chain absent")?;
    if chain.is_empty()
        || chain.len() > 9
        || binding["tag"] != inputs["release_snapshot"]["tag"]
        || binding["object"] != chain[0][1]
        || binding["commit"] != inputs["source_revision"]
        || chain
            .last()
            .and_then(|entry| entry.get(0))
            .and_then(Value::as_str)
            != Some("commit")
        || chain.last().and_then(|entry| entry.get(1)) != Some(&binding["commit"])
    {
        return Err("prepared complete tag binding is inconsistent".into());
    }
    let mut seen = BTreeSet::new();
    for (index, entry) in chain.iter().enumerate() {
        let pair = entry
            .as_array()
            .filter(|pair| pair.len() == 2)
            .ok_or("invalid tag link")?;
        let kind = pair[0].as_str().ok_or("tag link kind absent")?;
        let sha = pair[1]
            .as_str()
            .filter(|sha| hex(sha, 40))
            .ok_or("invalid tag link SHA")?;
        if !seen.insert(sha)
            || kind
                != if index + 1 == chain.len() {
                    "commit"
                } else {
                    "tag"
                }
        {
            return Err("prepared tag chain is cyclic or has an unsupported link".into());
        }
    }
    Ok(())
}

pub fn select<'a>(catalogue: &'a Value, architecture: &str) -> Result<Vec<(&'a Value, &'a Value)>> {
    if !matches!(architecture, "amd64" | "arm64") || catalogue["schema_version"] != 2 {
        return Err("unsupported native release-input platform or catalogue schema".into());
    }
    let plugins = catalogue["plugins"]
        .as_array()
        .ok_or("catalogue plugins absent")?;
    let mut ids = BTreeSet::new();
    let mut names = BTreeSet::new();
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
            return Err("release catalogue must declare exactly one version per plugin".into());
        };
        if version.get("release_id").is_some()
            || version["manifest"]["id"] != id
            || !hex(text(version, "revision")?, 40)
            || !hex(text(version, "manifest_sha256")?, 64)
            || text(&version["manifest"], "version")?.is_empty()
            || text(&version["manifest"], "version")?.len() > 128
        {
            return Err(
                "catalogue embeds API authority or lacks exact source/manifest facts".into(),
            );
        }
        storage::relative_path(std::path::Path::new(text(
            &version["manifest"],
            "entrypoint",
        )?))?;
        let artifacts = version["artifacts"]
            .as_array()
            .ok_or("catalogue artifacts absent")?;
        if artifacts.len() != 2
            || artifacts
                .iter()
                .map(|entry| text(entry, "architecture"))
                .collect::<Result<BTreeSet<_>>>()?
                != BTreeSet::from(["amd64", "arm64"])
        {
            return Err("native architecture entries are missing or duplicated".into());
        }
        for artifact in artifacts {
            let name = text(artifact, "asset")?;
            let minimum = text(&artifact["runtime"], "minimum")?;
            if artifact.get("asset_id").is_some()
                || !asset_name(name)
                || !names.insert(name)
                || artifact["os"] != "linux"
                || artifact["runtime"]["kind"] != "native"
                || artifact["runtime"]["libc"] != "glibc"
                || minimum.is_empty()
                || !minimum
                    .split('.')
                    .all(|part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit()))
                || artifact["format"] != "tar.gz"
                || artifact["dependencies"] != json!([])
                || number(artifact, "size")? == 0
                || number(artifact, "size")? > 64 * 1024 * 1024
                || !hex(text(artifact, "sha256")?, 64)
            {
                return Err("native package runtime/name/digest contract is invalid".into());
            }
            let members = artifact["members"]
                .as_array()
                .ok_or("package members absent")?;
            if members.is_empty() || members.len() > 2048 {
                return Err("package member count exceeds its bound".into());
            }
            let mut paths = BTreeSet::new();
            let mut expanded = 0u64;
            for member in members {
                let path = text(member, "path")?;
                storage::relative_path(std::path::Path::new(path))?;
                expanded = expanded
                    .checked_add(number(member, "size")?)
                    .ok_or("expanded size overflow")?;
                if !paths.insert(path)
                    || !hex(text(member, "sha256")?, 64)
                    || !member["executable"].is_boolean()
                    || expanded > 256 * 1024 * 1024
                {
                    return Err("package member declarations are invalid or duplicated".into());
                }
            }
            for required in [
                "among-plugin.json",
                "SOURCE.json",
                text(&version["manifest"], "entrypoint")?,
            ] {
                if !paths.contains(required) {
                    return Err("package omits its manifest, source record or entrypoint".into());
                }
            }
            let manifest = members
                .iter()
                .find(|member| member["path"] == "among-plugin.json")
                .ok_or("manifest absent")?;
            if manifest["sha256"] != version["manifest_sha256"] {
                return Err("manifest member digest differs from the catalogue".into());
            }
        }
        let selected = artifacts
            .iter()
            .find(|entry| entry["architecture"] == architecture)
            .ok_or("no package for the actual native architecture")?;
        selections.push((version, selected));
    }
    if ids != BTreeSet::from(["clock", "pane-pwd", "system-status"]) {
        return Err("official catalogue needs exactly the three approved plugins".into());
    }
    Ok(selections)
}

pub(super) fn validate(inputs: &Value, catalogue: &Value, architecture: &str) -> Result<()> {
    let snapshot = &inputs["release_snapshot"];
    validate_snapshot(snapshot)?;
    validate_chain(inputs)?;
    let catalogue_asset = asset(snapshot, "among-plugins.json")?;
    if inputs["schema_version"] != 2
        || inputs["kind"] != "published_release_inputs_not_installed"
        || inputs["repository_id"] != REPOSITORY
        || inputs["publisher_id"] != PUBLISHER
        || inputs["architecture"] != architecture
        || inputs["release_id"] != snapshot["id"]
        || inputs["catalogue_revision"] != inputs["source_revision"]
        || !hex(text(inputs, "source_revision")?, 40)
        || inputs["catalogue_size"] != catalogue_asset["size"]
        || inputs["catalogue_sha256"] != catalogue_asset["sha256"]
        || number(catalogue_asset, "size")? == 0
        || number(catalogue_asset, "size")? > 1024 * 1024
        || inputs["plugins"].as_array().map(Vec::len) != Some(3)
    {
        return Err("prepared input provenance differs from its release observation".into());
    }
    for (version, selected) in select(catalogue, architecture)? {
        if version["revision"] != inputs["source_revision"] {
            return Err("catalogue revision differs from the complete tag binding".into());
        }
        for artifact in version["artifacts"].as_array().ok_or("artifacts absent")? {
            let observed = asset(snapshot, text(artifact, "asset")?)?;
            if observed["size"] != artifact["size"] || observed["sha256"] != artifact["sha256"] {
                return Err("catalogue package differs from the complete API inventory".into());
            }
        }
        let id = text(&version["manifest"], "id")?;
        let records = inputs["plugins"]
            .as_array()
            .ok_or("prepared plugins absent")?;
        let matching = records
            .iter()
            .filter(|record| record["id"] == id)
            .collect::<Vec<_>>();
        let [record] = matching.as_slice() else {
            return Err("prepared plugin identity is missing or duplicated".into());
        };
        let observed = asset(snapshot, text(selected, "asset")?)?;
        if record["version"] != version["manifest"]["version"]
            || record["revision"] != inputs["source_revision"]
            || record["release_id"] != snapshot["id"]
            || record["asset_id"] != observed["id"]
            || record["asset"] != observed["name"]
            || record["sha256"] != selected["sha256"]
            || record["manifest_sha256"] != version["manifest_sha256"]
            || record["entrypoint"] != version["manifest"]["entrypoint"]
            || record["architecture"] != architecture
        {
            return Err("prepared package facts differ from the selected observed asset".into());
        }
    }
    let source = asset(snapshot, text(inputs, "source_archive")?)?;
    if inputs["source_asset_id"] != source["id"]
        || inputs["source_size"] != source["size"]
        || inputs["source_sha256"] != source["sha256"]
        || source["name"] != format!("source-{}.tar.gz", text(inputs, "source_revision")?)
        || number(source, "size")? == 0
        || number(source, "size")? > 32 * 1024 * 1024
    {
        return Err("prepared shared source differs from its observed release asset".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests;
