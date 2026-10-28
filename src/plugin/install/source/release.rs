//! pin one exact tag object and peel only same-repository immutable Git objects.
//! re-observation rejects visible drift, not hypothetical later movement or builds.

use super::super::release_snapshot::ReleaseSnapshot;
use super::{github_url, Operation, Selection};
use crate::plugin::{
    data::{self, Problem},
    inventory::ReleaseBinding,
};
use serde_json::Value;

const MAX_TAG_OBJECTS: usize = 8;
const MAX_GIT_BYTES: usize = 64 * 1024;

#[derive(PartialEq, Eq)]
pub(super) struct ResolvedRelease {
    pub binding: ReleaseBinding,
    snapshot: ReleaseSnapshot,
    chain: Vec<(String, String)>,
}

fn object(value: &Value) -> Result<(String, String), Problem> {
    let kind = value["type"]
        .as_str()
        .filter(|kind| matches!(*kind, "tag" | "commit"))
        .ok_or(Problem(
            "release tag targets an unsupported or ambiguous Git object type",
        ))?;
    let sha = value["sha"]
        .as_str()
        .filter(|sha| {
            sha.len() == 40
                && sha
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        })
        .ok_or(Problem(
            "release tag object lacks an exact supported SHA-1 identity",
        ))?;
    Ok((kind.into(), sha.into()))
}

impl Operation {
    async fn release_json(
        &self,
        repository: u64,
        suffix: &[&str],
        limit: usize,
    ) -> Result<Value, Problem> {
        let repository = repository.to_string();
        let mut components = vec!["repositories", &repository];
        components.extend_from_slice(suffix);
        let url = github_url("api.github.com", &components)?;
        let requested = url.clone();
        let (bytes, _) = self.get(url, |next| next == &requested, limit).await.map_err(|problem| {
            if problem.0 == "public repository or its among-plugins.json metadata was not found" {
                Problem("selected release tag or immutable Git object is unavailable; installation refused")
            } else {problem}
        })?;
        data::parse_unique(&bytes, limit)
    }

    pub(super) async fn resolve_release(
        &self,
        selected: &Selection,
    ) -> Result<ResolvedRelease, Problem> {
        let release = self
            .release_json(
                selected.repository_id,
                &["releases", &selected.version.release_id.to_string()],
                data::MAX_STATIC_BYTES,
            )
            .await?;
        let snapshot = ReleaseSnapshot::from_value(&release)?;
        if snapshot.id != selected.version.release_id {
            return Err(Problem(
                "selected public release identity disagrees with the catalogue",
            ));
        }
        let asset = snapshot.asset_by_id(selected.artifact.asset_id)?;
        if asset.size != selected.artifact.size {
            return Err(Problem("selected release asset identity or size changed"));
        }
        if asset.sha256 != selected.artifact.sha256 {
            return Err(Problem(
                "release asset digest disagrees with selected metadata",
            ));
        }
        let tag = snapshot.tag.as_str();
        let mut suffix = vec!["git", "ref", "tags"];
        suffix.extend(tag.split('/'));
        let reference = self
            .release_json(selected.repository_id, &suffix, MAX_GIT_BYTES)
            .await?;
        if reference["ref"].as_str() != Some(format!("refs/tags/{tag}").as_str())
            || !reference.is_object()
        {
            return Err(Problem(
                "release tag reference is ambiguous or not the exact fully qualified tag",
            ));
        }
        let mut current = object(&reference["object"])?;
        let head = current.1.clone();
        let mut chain: Vec<(String, String)> = Vec::new();
        loop {
            self.check()?;
            if chain.iter().any(|(_, sha)| sha == &current.1) {
                return Err(Problem("release tag annotation cycle refused"));
            }
            chain.push(current.clone());
            if current.0 == "commit" {
                let commit = self
                    .release_json(
                        selected.repository_id,
                        &["git", "commits", &current.1],
                        MAX_GIT_BYTES,
                    )
                    .await?;
                if commit["sha"].as_str() != Some(current.1.as_str()) {
                    return Err(Problem(
                        "resolved Git commit identity disagrees with the exact tag target",
                    ));
                }
                if current.1 != selected.version.revision {
                    return Err(Problem(
                        "catalogue version revision disagrees with the resolved release-tag commit",
                    ));
                }
                return Ok(ResolvedRelease {
                    binding: ReleaseBinding {
                        tag: tag.into(),
                        object: head,
                        commit: current.1,
                    },
                    snapshot,
                    chain,
                });
            }
            if chain.len() > MAX_TAG_OBJECTS {
                return Err(Problem(
                    "release tag annotation depth exceeds eight objects",
                ));
            }
            let annotation = self
                .release_json(
                    selected.repository_id,
                    &["git", "tags", &current.1],
                    MAX_GIT_BYTES,
                )
                .await?;
            if annotation["sha"].as_str() != Some(current.1.as_str()) {
                return Err(Problem(
                    "annotated tag response disagrees with its immutable object identity",
                ));
            }
            current = object(&annotation["object"])?;
        }
    }
}
