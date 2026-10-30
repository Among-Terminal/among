//! pin one exact tag object and peel only same-repository immutable Git objects.
//! re-observation rejects visible drift, not hypothetical later movement or builds.

use super::{github_url, Operation, Selection};
use crate::plugin::inventory::release::ReleaseSnapshot;
use crate::plugin::{
    data::{self, Problem},
    inventory::ReleaseBinding,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

const MAX_TAG_OBJECTS: usize = 8;
const MAX_GIT_BYTES: usize = 64 * 1024;

// GitHub's largest release-list page. A shorter page is the end of the list.
const RELEASE_PAGE: usize = 100;
// The newest prerelease is looked for in the first 300 listed releases only;
// a prerelease further down is not seen, even if it is newer.
const MAX_RELEASE_PAGES: usize = 3;
// One full page carries 100 complete release objects with every asset, so the
// single-document budget is far too small. Measured on Among-Terminal/among on
// 2026-10-30: 97,647 bytes for two releases of 19 assets each, about 2,070
// bytes per asset and 9,800 per release without assets. 100 releases at the
// 64-asset bound are then about 14.2 MB; 16 MiB holds that. Releases with very
// long notes or more assets can exceed it, and such a page is refused, never
// cut short: a truncated list could hide the newest prerelease.
pub(super) const MAX_RELEASE_PAGE_BYTES: usize = 16 * 1024 * 1024;

pub(super) const DEVELOPMENT_ONLY: &str =
    "prerelease sources are available only in plugin development mode";

/// Which release of a registered source is observed. The choice made at
/// inspection is repeated by the pre-commit re-observation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Channel {
    Latest,
    Prerelease,
}

/// GitHub's exact `YYYY-MM-DDTHH:MM:SSZ`, as a tuple ordered like time.
/// Fractions, offsets, lowercase separators and impossible dates are refused
/// rather than guessed at, because they decide which prerelease is newest.
pub(super) type CreatedAt = (u32, u32, u32, u32, u32, u32);

pub(super) fn created_at(value: &str) -> Option<CreatedAt> {
    let bytes = value.as_bytes();
    if bytes.len() != 20
        || bytes[4] != b'-'
        || bytes[7] != b'-'
        || bytes[10] != b'T'
        || bytes[13] != b':'
        || bytes[16] != b':'
        || bytes[19] != b'Z'
    {
        return None;
    }
    let number = |start: usize, end: usize| {
        let digits = &bytes[start..end];
        digits.iter().all(u8::is_ascii_digit).then(|| {
            digits
                .iter()
                .fold(0, |total, digit| total * 10 + u32::from(digit - b'0'))
        })
    };
    let (year, month, day) = (number(0, 4)?, number(5, 7)?, number(8, 10)?);
    let (hour, minute, second) = (number(11, 13)?, number(14, 16)?, number(17, 19)?);
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let days = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if leap => 29,
        2 => 28,
        _ => return None,
    };
    (day >= 1 && day <= days && hour < 24 && minute < 60 && second < 60)
        .then_some((year, month, day, hour, minute, second))
}

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

    pub(super) async fn observe(
        &self,
        repository: u64,
        channel: Channel,
    ) -> Result<ReleaseSnapshot, Problem> {
        match channel {
            Channel::Latest => self.latest_release(repository).await,
            Channel::Prerelease => self.newest_prerelease(repository).await,
        }
    }

    async fn release_page(&self, repository: u64, page: usize) -> Result<Value, Problem> {
        let mut url = github_url(
            "api.github.com",
            &["repositories", &repository.to_string(), "releases"],
        )?;
        url.query_pairs_mut()
            .append_pair("per_page", &RELEASE_PAGE.to_string())
            .append_pair("page", &page.to_string());
        // The only accepted redirect target is the exact page asked for, and
        // `get` refuses any redirect carrying a query, so a list page cannot be
        // redirected at all.
        let requested = url.clone();
        let (bytes, _) = self
            .get(url, |next| next == &requested, MAX_RELEASE_PAGE_BYTES)
            .await
            .map_err(|problem| {
                if problem.0 == "public repository or its among-plugins.json metadata was not found"
                {
                    Problem("repository release list is unavailable; nothing was changed")
                } else {
                    problem
                }
            })?;
        data::parse_unique(&bytes, MAX_RELEASE_PAGE_BYTES)
    }

    // GitHub documents an order only for `releases/latest`, not for the list,
    // so every page up to the bound is read and the order of entries is never
    // trusted.
    pub(super) async fn newest_prerelease(
        &self,
        repository: u64,
    ) -> Result<ReleaseSnapshot, Problem> {
        let mut seen = std::collections::BTreeSet::new();
        let mut newest: Option<(CreatedAt, u64, String)> = None;
        for page in 1..=MAX_RELEASE_PAGES {
            self.check()?;
            let listed = self.release_page(repository, page).await?;
            let entries = listed
                .as_array()
                .filter(|entries| entries.len() <= RELEASE_PAGE)
                .ok_or(Problem(
                    "release list page is not an array of at most 100 releases",
                ))?;
            for entry in entries {
                let id = entry["id"]
                    .as_u64()
                    .filter(|id| *id > 0)
                    .ok_or(Problem("listed release lacks a positive API identity"))?;
                // A release created while pages are read shifts later pages
                // and repeats an entry; the list is then not one observation.
                if !seen.insert(id) {
                    return Err(Problem(
                        "release list changed while it was read; refresh again",
                    ));
                }
                let (Some(draft), Some(prerelease)) =
                    (entry["draft"].as_bool(), entry["prerelease"].as_bool())
                else {
                    return Err(Problem(
                        "listed release lacks its draft or prerelease state",
                    ));
                };
                if draft || !prerelease {
                    continue;
                }
                let raw = entry["created_at"].as_str().unwrap_or_default();
                let time = created_at(raw).ok_or(Problem(
                    "prerelease creation time is not an exact UTC timestamp; refusing to guess the newest",
                ))?;
                if newest
                    .as_ref()
                    .is_none_or(|(best, best_id, _)| (time, id) > (*best, *best_id))
                {
                    newest = Some((time, id, raw.into()));
                }
            }
            if entries.len() < RELEASE_PAGE {
                break;
            }
        }
        let (_, id, listed_at) = newest.ok_or(Problem(
            "no published prerelease within the first 300 releases; nothing was changed",
        ))?;
        // The chosen release is read again as one document. GitHub does not
        // document that list entries carry a complete asset inventory, and this
        // way the admitted document has the same single-release byte budget
        // and endpoint shape as `releases/latest`.
        let release = self
            .release_json(
                repository,
                &["releases", &id.to_string()],
                data::MAX_STATIC_BYTES,
            )
            .await
            .map_err(|problem| {
                if problem.0 == "selected release tag or immutable Git object is unavailable; installation refused" {
                    Problem("newest prerelease disappeared while it was read; refresh again")
                } else {
                    problem
                }
            })?;
        if release["id"].as_u64() != Some(id)
            || release["created_at"].as_str() != Some(listed_at.as_str())
        {
            return Err(Problem(
                "prerelease document disagrees with its listed identity or creation time",
            ));
        }
        ReleaseSnapshot::from_prerelease_value(&release)
    }

    pub(super) async fn resolve_release(
        &self,
        selected: &Selection,
    ) -> Result<ResolvedRelease, Problem> {
        let channel = if selected.release.snapshot.is_prerelease() {
            Channel::Prerelease
        } else {
            Channel::Latest
        };
        let snapshot = self.observe(selected.repository_id, channel).await?;
        let release = self.resolve_tag(selected.repository_id, snapshot).await?;
        if release.snapshot != selected.release.snapshot {
            return Err(Problem(match channel {
                Channel::Latest => "latest release snapshot changed; refresh and review again",
                Channel::Prerelease => {
                    "newest prerelease snapshot changed; refresh and review again"
                }
            }));
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
