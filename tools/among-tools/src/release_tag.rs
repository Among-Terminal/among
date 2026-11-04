//! Decides, from a checkout, whether a commit on `main` is a release commit
//! that must get a `v*` tag, and whether a release candidate may be tagged.
//!
//! The release tag workflow creates tags with the repository token, so the
//! decision is made here, in a tested command run without any write
//! permission, and the workflow only carries it out.

use crate::release_plan::{git, highest_stable_version, notes_path};
use crate::{version_locations, Result};
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Not a release commit; nothing to do.
    Skip,
    /// The tag does not exist: create it.
    Create,
    /// The tag already points at the commit: only make sure the draft started.
    Exists,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decision {
    pub action: Action,
    pub tag: String,
    pub sha: String,
}

impl Decision {
    /// `key=value` lines for `$GITHUB_OUTPUT`.
    pub fn render(&self) -> String {
        let action = match self.action {
            Action::Skip => "skip",
            Action::Create => "create",
            Action::Exists => "exists",
        };
        format!("action={action}\ntag={}\nsha={}\n", self.tag, self.sha)
    }
}

/// The version of a commit title `chore(release): X.Y.Z` or
/// `chore(release): X.Y.Z (#N)`.
fn release_title_version(subject: &str) -> Option<&str> {
    let rest = subject.strip_prefix("chore(release): ")?;
    let (version, pull_request) = match rest.split_once(' ') {
        Some((version, pull_request)) => (version, Some(pull_request)),
        None => (rest, None),
    };
    if let Some(suffix) = pull_request {
        let number = suffix.strip_prefix("(#")?.strip_suffix(')')?;
        if number.is_empty() || !number.bytes().all(|byte| byte.is_ascii_digit()) {
            return None;
        }
    }
    version_locations::triple(version).ok()?;
    Some(version)
}

fn full_sha(root: &Path, revision: &str) -> Result<String> {
    let sha = git(
        root,
        &["rev-parse", "--verify", &format!("{revision}^{{commit}}")],
    )?
    .trim()
    .to_string();
    if sha.len() != 40 {
        return Err(format!("{revision} does not name a commit").into());
    }
    Ok(sha)
}

fn tag_commit(root: &Path, tag: &str) -> Option<String> {
    git(
        root,
        &[
            "rev-parse",
            "-q",
            "--verify",
            &format!("refs/tags/{tag}^{{commit}}"),
        ],
    )
    .ok()
    .map(|text| text.trim().to_string())
    .filter(|sha| !sha.is_empty())
}

fn cargo_version_at(root: &Path, sha: &str) -> Result<String> {
    let text = git(root, &["show", &format!("{sha}:Cargo.toml")])?;
    version_locations::cargo_version(&text)
}

/// Decision for a push to `main` whose head is `sha`.
pub fn decide_push(root: &Path, sha: &str) -> Result<Decision> {
    let sha = full_sha(root, sha)?;
    let subject = git(root, &["log", "-1", "--format=%s", &sha])?;
    let Some(version) = release_title_version(subject.trim()) else {
        return Ok(Decision {
            action: Action::Skip,
            tag: String::new(),
            sha,
        });
    };
    let tag = format!("v{version}");
    let cargo = cargo_version_at(root, &sha)?;
    if cargo != version {
        return Err(format!("the title says {version} but Cargo.toml says {cargo}").into());
    }
    let notes = git(root, &["show", &format!("{sha}:{}", notes_path(version))]).map_err(|_| {
        format!(
            "the release commit has no {}; a stable tag needs its highlights",
            notes_path(version)
        )
    })?;
    if notes.trim().is_empty() {
        return Err(format!("{} is empty", notes_path(version)).into());
    }
    match tag_commit(root, &tag) {
        Some(existing) if existing == sha => Ok(Decision {
            action: Action::Exists,
            tag,
            sha,
        }),
        Some(existing) => Err(format!("{tag} already exists at {existing}, not at {sha}").into()),
        None => {
            let wanted = version_locations::triple(version)?;
            if let Some(highest) = highest_stable_version(root)? {
                if highest >= wanted {
                    return Err(format!(
                        "{version} is not above the highest stable tag {}.{}.{}",
                        highest.0, highest.1, highest.2
                    )
                    .into());
                }
            }
            Ok(Decision {
                action: Action::Create,
                tag,
                sha,
            })
        }
    }
}

/// Decision for a release candidate `number` of the version in `Cargo.toml` at
/// `HEAD` (the tip of `main`): the stable tag of that version must exist and
/// `main` must contain its commit.
pub fn decide_candidate(root: &Path, number: &str) -> Result<Decision> {
    let valid = !number.is_empty()
        && number.bytes().all(|byte| byte.is_ascii_digit())
        && !number.starts_with('0');
    if !valid {
        return Err(format!(
            "candidate '{number}' must be a positive integer without leading zeros"
        )
        .into());
    }
    let head = full_sha(root, "HEAD")?;
    let version = cargo_version_at(root, &head)?;
    version_locations::triple(&version)?;
    let stable = format!("v{version}");
    let sha = tag_commit(root, &stable)
        .ok_or_else(|| format!("{stable} does not exist; merge the release pull request first"))?;
    if git(root, &["merge-base", "--is-ancestor", &sha, &head]).is_err() {
        return Err(format!("{sha} is not contained in main").into());
    }
    let tag = format!("{stable}-rc.{number}");
    match tag_commit(root, &tag) {
        Some(existing) if existing == sha => Ok(Decision {
            action: Action::Exists,
            tag,
            sha,
        }),
        Some(existing) => Err(format!("{tag} already exists at {existing}, not at {sha}").into()),
        None => Ok(Decision {
            action: Action::Create,
            tag,
            sha,
        }),
    }
}
