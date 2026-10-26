//! replay only a profile captured after actual UI installation and explicit
//! review. preserve its receipts/consent bytes, never manufacture either here.

use crate::{plugin_install::release, storage, Result};
use serde_json::Value;
use std::fs::{self, OpenOptions};
use std::io::Read;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

pub(crate) fn bounded(path: &Path, maximum: u64) -> Result<Vec<u8>> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK)
        .open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.len() > maximum {
        return Err("runtime replay input exceeds its regular-file bound".into());
    }
    let mut bytes = Vec::new();
    file.take(maximum + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 != metadata.len() {
        return Err("runtime replay input changed during read".into());
    }
    Ok(bytes)
}

fn text<'a>(value: &'a Value, name: &str) -> Result<&'a str> {
    value[name]
        .as_str()
        .ok_or_else(|| format!("runtime replay lacks string {name}").into())
}

fn checked(profile: &Path, path: &str, record: &Value, size: &str, sha: &str) -> Result<Vec<u8>> {
    let size = record[size]
        .as_u64()
        .filter(|size| *size <= 8 * 1024 * 1024)
        .ok_or("invalid runtime replay size")?;
    release::checked_bytes(
        &storage::safe_path(profile, Path::new(path))?,
        size,
        text(record, sha)?,
    )
}

pub fn replay(profile: &Path, destination: &Path) -> Result<()> {
    let metadata = storage::parse_json(&bounded(
        &storage::safe_path(profile, Path::new("profile.json"))?,
        64 * 1024,
    )?)?;
    let architecture = crate::platform::discover()?.architecture;
    if metadata["kind"] != "observed_published_installation_profile"
        || metadata["source_revision"] != release::SOURCE
        || metadata["catalogue_revision"] != release::CATALOGUE
        || metadata["architecture"] != architecture
    {
        return Err("runtime replay provenance or actual architecture differs".into());
    }
    let installed = checked(
        profile,
        "managed/installed.json",
        &metadata,
        "installed_size",
        "installed_sha256",
    )?;
    let settings = checked(
        profile,
        "managed-plugins.json",
        &metadata,
        "settings_size",
        "settings_sha256",
    )?;
    let selections = storage::parse_json(&installed)?;
    let consent = storage::parse_json(&settings)?;
    let receipts = selections["installations"]
        .as_array()
        .ok_or("runtime replay lacks installed receipts")?;
    if selections["version"] != 1 || consent["version"] != 1 || receipts.len() != 3 {
        return Err("runtime replay needs the three actual selected installations".into());
    }
    let catalogue = release::checked_bytes(
        &storage::safe_path(profile, Path::new("among-plugins.json"))?,
        634525,
        release::CATALOGUE_SHA256,
    )?;
    let catalogue = storage::parse_json(&catalogue)?;
    let mut files: Vec<(PathBuf, Vec<u8>, u32)> = Vec::new();
    for (version, artifact) in release::select(&catalogue, &architecture)? {
        let id = text(&version["manifest"], "id")?;
        let matching: Vec<_> = receipts
            .iter()
            .filter(|receipt| receipt["plugin_id"] == id)
            .collect();
        let [receipt] = matching.as_slice() else {
            return Err("runtime replay identity missing or duplicated".into());
        };
        let source = &receipt["source"];
        let binding = &source["release_binding"];
        let key = format!("github:1431055733/{id}");
        let local = &consent["plugins"][&key];
        if receipt["repository_id"] != 1431055733u64
            || receipt["release_id"] != 424255611
            || receipt["revision"] != release::SOURCE
            || receipt["asset_id"] != artifact["asset_id"]
            || receipt["manifest_sha256"] != version["manifest_sha256"]
            || receipt["package_sha256"] != artifact["sha256"]
            || source["catalogue_revision"] != release::CATALOGUE
            || source["catalogue_sha256"] != release::CATALOGUE_SHA256
            || source["publisher_id"] != 339567100u64
            || binding["commit"] != release::SOURCE
            || binding["tag"] != "v0.1.0-rc.1"
            || binding["object"] != "31b3ad165bb6a685dfe49fcde082949e12e5887b"
            || local["enabled"] != true
            || !receipt["installation"].is_string()
            || local["installation"] != receipt["installation"]
            || local["review"]["repository_id"] != 1431055733u64
            || local["review"]["publisher_id"] != 339567100u64
            || local["review"]["features"] != version["manifest"]["required_features"]
        {
            return Err(
                "runtime replay receipt/access differs from the observed public selection".into(),
            );
        }
        let generation = text(receipt, "generation")?;
        if generation.len() != 64
            || !generation
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err("invalid runtime replay generation".into());
        }
        let relative = PathBuf::from(format!("generations/1431055733/{id}/{generation}"));
        let package = storage::safe_path(profile, &Path::new("managed").join(&relative))?;
        let seal = bounded(
            &storage::safe_path(&package, Path::new(".among-receipt.json"))?,
            64 * 1024,
        )?;
        if storage::parse_json(&seal)? != **receipt {
            return Err("runtime replay seal disagrees with selection".into());
        }
        let mut resources = vec![
            "among-plugin.json",
            text(&version["manifest"], "entrypoint")?,
        ];
        if let Some(icon) = version["manifest"]["icon"]
            .as_str()
            .filter(|icon| !icon.starts_with("theme:"))
        {
            resources.push(icon);
        }
        for path in resources {
            let member = artifact["members"]
                .as_array()
                .ok_or("runtime replay members absent")?
                .iter()
                .find(|member| member["path"] == path)
                .ok_or("runtime replay resource lacks a declared digest")?;
            let bytes = checked(&package, path, member, "size", "sha256")?;
            files.push((
                relative.join(path),
                bytes,
                if member["executable"] == true {
                    0o700
                } else {
                    0o600
                },
            ));
        }
        files.push((relative.join(".among-receipt.json"), seal, 0o600));
        files.push((
            relative.join(".among-lease"),
            bounded(
                &storage::safe_path(&package, Path::new(".among-lease"))?,
                1024,
            )?,
            0o600,
        ));
    }
    if files.len() > 32
        || files.iter().map(|(_, bytes, _)| bytes.len()).sum::<usize>()
            + installed.len()
            + settings.len()
            > 8 * 1024 * 1024
    {
        return Err("runtime replay profile exceeds its aggregate budget".into());
    }
    let managed = destination.join("data/among/managed");
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&managed)?;
    if managed.join("installed.json").exists() {
        return Err("runtime replay destination is already selected".into());
    }
    for (path, bytes, mode) in files {
        let target = storage::safe_path(&managed, &path)?;
        fs::DirBuilder::new().recursive(true).mode(0o700).create(
            target
                .parent()
                .ok_or("runtime replay target lacks parent")?,
        )?;
        storage::atomic_write(&managed, &path, &bytes, mode)?;
    }
    storage::atomic_write(
        &destination.join("config/among"),
        Path::new("managed-plugins.json"),
        &settings,
        0o600,
    )?;
    storage::atomic_write(&managed, Path::new("installed.json"), &installed, 0o600)?;
    println!("PASS renderer replay consumed native {architecture} public catalogue, exact installed/settings SHA, numeric assets/tag binding and observed explicit grants; selection bytes unchanged");
    Ok(())
}
