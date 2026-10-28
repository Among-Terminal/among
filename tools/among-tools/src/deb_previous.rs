//! The published Debian package that installed acceptance upgrades from.
//!
//! Users upgrade from the release they already have, not from a repacked copy
//! of the candidate. `packaging/ci/deb-upgrade/previous-release.json` pins that release:
//! its stable tag and, for every Debian target, the attached file name, its
//! SHA-256 and the package version inside it. Release CI downloads the file
//! from the public release and refuses any byte or metadata difference, so a
//! replaced or re-uploaded asset cannot pass as the published one.

use crate::release_assets::{package_asset, ARCHITECTURES, DEB_TARGETS};
use crate::{deb, storage, Result};
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::Path;

pub const PIN: &str = "packaging/ci/deb-upgrade/previous-release.json";
const REPOSITORY: &str = "Among-Terminal/among";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Package {
    pub tag: String,
    pub asset: String,
    pub sha256: String,
    pub version: String,
}

impl Package {
    pub fn url(&self) -> String {
        format!(
            "https://github.com/{REPOSITORY}/releases/download/{}/{}",
            self.tag, self.asset
        )
    }
}

/// Every Debian slot, keyed `<target>-<architecture>`, from the pin bytes.
pub fn parse(bytes: &[u8]) -> Result<BTreeMap<String, Package>> {
    let pin = storage::parse_json(bytes)?;
    let object = pin
        .as_object()
        .ok_or("previous release pin is not an object")?;
    if object.len() != 3 || pin["schema"] != 1 {
        return Err("previous release pin must hold exactly schema, tag and packages".into());
    }
    let tag = pin["tag"].as_str().ok_or("previous release tag missing")?;
    // Only a stable release is something users actually have installed.
    deb::stable_version(tag)?;
    let packages = pin["packages"]
        .as_object()
        .ok_or("previous release packages missing")?;
    let mut result = BTreeMap::new();
    for target in DEB_TARGETS {
        for architecture in ARCHITECTURES {
            let slot = format!("{target}-{architecture}");
            let entry = packages
                .get(&slot)
                .ok_or(format!("previous release pin lacks {slot}"))?;
            result.insert(
                slot.clone(),
                package(entry, tag, target, architecture, &slot)?,
            );
        }
    }
    if packages.len() != result.len() {
        return Err("previous release pin names an unknown Debian slot".into());
    }
    Ok(result)
}

fn package(
    entry: &Value,
    tag: &str,
    target: &str,
    architecture: &str,
    slot: &str,
) -> Result<Package> {
    if entry.as_object().map(|fields| fields.len()) != Some(3) {
        return Err(format!("{slot}: expected exactly asset, sha256 and version").into());
    }
    let field = |name: &str| {
        entry[name]
            .as_str()
            .map(str::to_string)
            .ok_or(format!("{slot}: {name} missing"))
    };
    let (asset, sha256, version) = (field("asset")?, field("sha256")?, field("version")?);
    // The names and versions follow from the tag; a pin that disagrees was
    // edited by hand or copied from another release.
    if asset != package_asset(target, architecture, "deb") {
        return Err(format!("{slot}: asset {asset} is not the release name of this slot").into());
    }
    if version != deb::package_version(tag, target)? {
        return Err(format!("{slot}: version {version} does not follow from {tag}").into());
    }
    if sha256.len() != 64
        || !sha256
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
    {
        return Err(format!("{slot}: sha256 is not 64 lowercase hex digits").into());
    }
    Ok(Package {
        tag: tag.to_string(),
        asset,
        sha256,
        version,
    })
}

/// The pinned package for one slot, read from the source tree.
pub fn read(root: &Path, target: &str, architecture: &str) -> Result<Package> {
    let bytes = std::fs::read(storage::safe_path(root, Path::new(PIN))?)?;
    parse(&bytes)?
        .remove(&format!("{target}-{architecture}"))
        .ok_or_else(|| format!("previous release pin lacks {target}-{architecture}").into())
}
