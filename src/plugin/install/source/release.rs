//! pin one exact tag object and peel only same-repository immutable Git objects.
//! re-observation rejects visible drift, not hypothetical later movement or builds.

use super::super::release_snapshot::ReleaseSnapshot;
use super::{github_url, Operation, Selection};
use crate::plugin::{
    data::{self, Problem},
    inventory::ReleaseBinding,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

const MAX_TAG_OBJECTS: usize = 8;
const MAX_GIT_BYTES: usize = 64 * 1024;

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(in crate::plugin::install) struct ResolvedRelease {
    pub binding: ReleaseBinding,
    pub snapshot: ReleaseSnapshot,
    chain: Vec<(String, String)>,
}

impl ResolvedRelease {
    #[cfg(test)]
    pub(super) fn fixture(snapshot: ReleaseSnapshot, commit: String) -> Self {
        Self {
            binding: ReleaseBinding {
                tag: snapshot.tag.clone(),
                object: commit.clone(),
                commit: commit.clone(),
            },
            snapshot,
            chain: vec![("commit".into(), commit)],
        }
    }

    pub fn validate(&self) -> Result<(), Problem> {
        self.snapshot.validate()?;
        if self.binding.tag != self.snapshot.tag
            || self.chain.is_empty()
            || self.chain.len() > MAX_TAG_OBJECTS + 1
            || self.chain.first().map(|(_, sha)| sha) != Some(&self.binding.object)
            || self.chain.last() != Some(&("commit".into(), self.binding.commit.clone()))
        {
            return Err(Problem("cached release tag binding is inconsistent"));
        }
        let mut seen = std::collections::BTreeSet::new();
        for (index, (kind, sha)) in self.chain.iter().enumerate() {
            object(&serde_json::json!({"type":kind,"sha":sha}))?;
            if !seen.insert(sha) || (index + 1 < self.chain.len() && kind != "tag") {
                return Err(Problem("cached release tag chain is inconsistent"));
            }
        }
        Ok(())
    }
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

    pub(super) async fn latest_release(&self, repository: u64) -> Result<ReleaseSnapshot, Problem> {
        let release = self
            .release_json(
                repository,
                &["releases", "latest"],
                data::MAX_STATIC_BYTES,
            )
            .await.map_err(|problem| if problem.0 == "selected release tag or immutable Git object is unavailable; installation refused" {
                Problem("no installable latest release is available; refresh and review when one is published")
            } else { problem })?;
        ReleaseSnapshot::from_value(&release)
    }

    pub(super) async fn resolve_release(
        &self,
        selected: &Selection,
    ) -> Result<ResolvedRelease, Problem> {
        let snapshot = self.latest_release(selected.repository_id).await?;
        let release = self.resolve_tag(selected.repository_id, snapshot).await?;
        if release.snapshot != selected.release.snapshot {
            return Err(Problem(
                "latest release snapshot changed; refresh and review again",
            ));
        }
        Ok(release)
    }

    pub(super) async fn resolve_tag(
        &self,
        repository: u64,
        snapshot: ReleaseSnapshot,
    ) -> Result<ResolvedRelease, Problem> {
        let tag = snapshot.tag.as_str();
        let mut suffix = vec!["git", "ref", "tags"];
        suffix.extend(tag.split('/'));
        let reference = self
            .release_json(repository, &suffix, MAX_GIT_BYTES)
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
                    .release_json(repository, &["git", "commits", &current.1], MAX_GIT_BYTES)
                    .await?;
                if commit["sha"].as_str() != Some(current.1.as_str()) {
                    return Err(Problem(
                        "resolved Git commit identity disagrees with the exact tag target",
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
                .release_json(repository, &["git", "tags", &current.1], MAX_GIT_BYTES)
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
