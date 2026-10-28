//! Release API observations, not catalogue-supplied authority. The complete
//! inventory participates in the pre-commit comparison, not just one package.

use crate::plugin::{data::Problem, inventory::ReleaseBinding};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeSet;

pub(super) const MAX_RELEASE_ASSETS: usize = 64;

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ReleaseAsset {
    pub id: u64,
    pub name: String,
    pub size: u64,
    pub sha256: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ReleaseSnapshot {
    pub id: u64,
    pub tag: String,
    assets: Vec<ReleaseAsset>,
}

impl ReleaseSnapshot {
    pub fn from_value(value: &Value) -> Result<Self, Problem> {
        if value["draft"].as_bool() != Some(false)
            || value["prerelease"].as_bool() != Some(false)
            || value["immutable"].as_bool() != Some(true)
        {
            return Err(Problem(
                "ordinary installation requires a published stable immutable release",
            ));
        }
        let id = value["id"]
            .as_u64()
            .filter(|id| *id > 0)
            .ok_or(Problem("release lacks a positive API identity"))?;
        let tag = value["tag_name"]
            .as_str()
            .filter(|tag| ReleaseBinding::supported_tag(tag))
            .ok_or(Problem(
                "selected release has no supported exact tag name; branch fallback is forbidden",
            ))?;
        let entries = value["assets"]
            .as_array()
            .filter(|entries| !entries.is_empty() && entries.len() <= MAX_RELEASE_ASSETS)
            .ok_or(Problem(
                "release asset inventory must contain 1..=64 complete entries",
            ))?;
        let mut ids = BTreeSet::new();
        let mut names = BTreeSet::new();
        let mut assets = Vec::with_capacity(entries.len());
        for entry in entries {
            let id = entry["id"]
                .as_u64()
                .filter(|id| *id > 0 && ids.insert(*id))
                .ok_or(Problem(
                    "release asset API identity is missing or duplicated",
                ))?;
            let name = entry["name"]
                .as_str()
                .filter(|name| asset_name(name) && names.insert(*name))
                .ok_or(Problem("release asset name is unsafe or duplicated"))?;
            if entry["state"].as_str() != Some("uploaded") {
                return Err(Problem("release asset is not completely uploaded"));
            }
            let size = entry["size"]
                .as_u64()
                .ok_or(Problem("release asset size is missing or invalid"))?;
            let sha256 = entry["digest"]
                .as_str()
                .and_then(|digest| digest.strip_prefix("sha256:"))
                .filter(|digest| {
                    digest.len() == 64
                        && digest
                            .bytes()
                            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
                })
                .ok_or(Problem(
                    "every release asset needs sha256 followed by 64 lowercase hex digits",
                ))?;
            assets.push(ReleaseAsset {
                id,
                name: name.into(),
                size,
                sha256: sha256.into(),
            });
        }
        if assets
            .iter()
            .filter(|asset| asset.name == "among-plugins.json")
            .count()
            != 1
        {
            return Err(Problem(
                "release must contain exactly one among-plugins.json asset",
            ));
        }
        // API array order is not part of the selected content identity.
        assets.sort_by(|left, right| left.name.cmp(&right.name));
        Ok(Self {
            id,
            tag: tag.into(),
            assets,
        })
    }

    // Persist only the bounded canonical observation, not irrelevant API fields.
    // Re-admit it through the same validation before offline use.
    pub fn validate(&self) -> Result<(), Problem> {
        if self.assets.is_empty() || self.assets.len() > MAX_RELEASE_ASSETS {
            return Err(Problem("stored release inventory exceeds its bounds"));
        }
        let assets = self
            .assets
            .iter()
            .map(|asset| {
                serde_json::json!({
                    "id":asset.id,"name":asset.name,"size":asset.size,"state":"uploaded",
                    "digest":format!("sha256:{}",asset.sha256)
                })
            })
            .collect::<Vec<_>>();
        let canonical = Self::from_value(&serde_json::json!({
            "id":self.id,"tag_name":self.tag,"draft":false,"prerelease":false,
            "immutable":true,"assets":assets
        }))?;
        if &canonical != self {
            return Err(Problem("stored release inventory is not canonical"));
        }
        Ok(())
    }

    pub fn asset(&self, name: &str) -> Result<&ReleaseAsset, Problem> {
        self.assets
            .iter()
            .find(|asset| asset.name == name)
            .ok_or(Problem(
                "exact asset name does not belong to the selected release",
            ))
    }

    pub fn asset_by_id(&self, id: u64) -> Result<&ReleaseAsset, Problem> {
        self.assets
            .iter()
            .find(|asset| asset.id == id)
            .ok_or(Problem(
                "receipt asset identity is absent from its release snapshot",
            ))
    }
}

pub(crate) fn asset_name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 255
        && !matches!(value, "." | "..")
        && !value.contains(['/', '\\'])
        && value.chars().all(|character| !character.is_control())
}

#[cfg(test)]
mod tests;
