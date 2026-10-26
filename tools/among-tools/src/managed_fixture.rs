//! synthetic runtime-check setup, never a repository installation or consent
//! observation. callers use an isolated harness, not the user's plugin store.

use crate::{storage, Result};
use serde_json::json;
use std::{
    fs,
    os::unix::fs::{DirBuilderExt, MetadataExt},
    path::{Path, PathBuf},
};

pub(crate) fn install(base: &Path, id: &str, program: &[u8], features: &[&str]) -> Result<PathBuf> {
    let metadata = base.symlink_metadata()?;
    if !metadata.is_dir()
        || metadata.uid() != nix::unistd::geteuid().as_raw()
        || metadata.mode() & 0o077 != 0
        || id.is_empty()
        || !id
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        || program.len() > 8 * 1024 * 1024
    {
        return Err(
            "synthetic managed fixture requires an owned private base and bounded program".into(),
        );
    }
    let manifest = serde_json::to_vec(
        &json!({"schema_version":1,"id":id,"version":"0.2.0-test","name":id,
        "description":"Synthetic managed runtime-check fixture; not an installed public Release",
        "entrypoint":"plugin","api":{"major":1,"minor":2},"required_features":features}),
    )?;
    let generation = storage::sha256(program)?;
    let token = storage::sha256(base.as_os_str().as_encoded_bytes())?;
    let root = base.join("data/among/managed");
    let package = root.join(format!("generations/12/{id}/{generation}"));
    let config = base.join("config/among");
    if root.join("installed.json").symlink_metadata().is_ok()
        || package.symlink_metadata().is_ok()
        || config
            .join("managed-plugins.json")
            .symlink_metadata()
            .is_ok()
    {
        return Err(
            "synthetic fixture must not replace existing selection, generation or settings".into(),
        );
    }
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&package)?;
    let source = json!({"publisher_id":34,"catalogue_revision":"a".repeat(40),"catalogue_sha256":"b".repeat(64)});
    let receipt = json!({"repository_id":12,"owner":"example","repository":"runtime-fixtures","plugin_id":id,
        "revision":"a".repeat(40),"manifest_sha256":storage::sha256(manifest.as_slice())?,"package_sha256":generation,
        "release_id":1,"asset_id":2,"generation":generation,"installation":token,"source":source});
    storage::atomic_write(&package, Path::new("plugin"), program, 0o700)?;
    storage::atomic_write(&package, Path::new("among-plugin.json"), &manifest, 0o600)?;
    storage::atomic_write(
        &package,
        Path::new(".among-receipt.json"),
        &serde_json::to_vec(&receipt)?,
        0o600,
    )?;
    storage::atomic_write(&package, Path::new(".among-lease"), b"", 0o600)?;
    storage::atomic_write(
        &root,
        Path::new("installed.json"),
        &serde_json::to_vec(&json!({"version":1,"installations":[receipt]}))?,
        0o600,
    )?;
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&config)?;
    storage::atomic_write(
        &config,
        Path::new("managed-plugins.json"),
        &serde_json::to_vec(&json!({"version":1,"plugins":{
            (format!("github:12/{id}")):{"enabled":true,"installation":token,"review":{"repository_id":12,"publisher_id":34,"owner":"example","repository":"runtime-fixtures","features":features}}
        }}))?,
        0o600,
    )?;
    println!("Synthetic managed runtime fixture github:12/{id}: declared manifest, identity-bound selection and explicit fixture grants; not public installation/access acceptance");
    Ok(config.join("managed-plugins.json"))
}
