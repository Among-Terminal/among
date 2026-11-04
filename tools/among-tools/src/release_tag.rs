//! Decides, from a checkout, what the release tag workflow may do: validate a
//! release commit that was pushed to `main`, tag a release candidate on it, or
//! tag the stable release on a commit that already has an accepted candidate.
//!
//! Merging a release pull request creates no tag. The tags are made by two
//! manual runs, so the owner decides each one: first the candidate
//! `vX.Y.Z-rc.N` for testing, and only after the owner accepts it the stable
//! `vX.Y.Z` on the very same commit.
//!
//! The workflow creates tags with the repository token, so the decision is made
//! here, in a tested command run without any write permission, and the workflow
//! only carries it out.

use crate::release_plan::{git, highest_stable_version, notes_path};
use crate::{version_locations, Result};
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Not a release commit; nothing to do.
    Skip,
    /// A valid release commit was pushed; it gets no tag until a manual run.
    Validated,
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
            Action::Validated => "validated",
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

/// A release commit that passed every check: its title, `Cargo.toml` and
/// highlights agree on `version`.
struct Release {
    version: String,
    sha: String,
}

/// Checks that `sha`, whose title names `version`, carries that version in
/// `Cargo.toml` and a non-empty highlights file.
fn validate_release(root: &Path, sha: &str, version: &str) -> Result<Release> {
    let cargo = cargo_version_at(root, sha)?;
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
    Ok(Release {
        version: version.to_string(),
        sha: sha.to_string(),
    })
}

/// Fails unless `version` is above every stable tag, so a release can never be
/// tagged below or at one that was already made.
fn require_above_highest_stable(root: &Path, version: &str) -> Result<()> {
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
    Ok(())
}

/// The release commit a manual run acts on: the commit named by the
/// `release_commit` input, or else `HEAD`. It must be a valid release commit
/// that `HEAD` (the tip of `main`) contains.
fn manual_release(root: &Path, named: Option<&str>) -> Result<Release> {
    let head = full_sha(root, "HEAD")?;
    let sha = match named.filter(|text| !text.is_empty()) {
        None => head.clone(),
        Some(text) => {
            let valid = (7..=40).contains(&text.len())
                && text
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte));
            if !valid {
                return Err(format!(
                    "release_commit '{text}' must be 7 to 40 lowercase hexadecimal digits"
                )
                .into());
            }
            full_sha(root, text)?
        }
    };
    if git(root, &["merge-base", "--is-ancestor", &sha, &head]).is_err() {
        return Err(format!("{sha} is not contained in main").into());
    }
    let subject = git(root, &["log", "-1", "--format=%s", &sha])?;
    let version = release_title_version(subject.trim()).ok_or_else(|| {
        format!(
            "{sha} is not a release commit (its title is '{}'); name the release commit in release_commit",
            subject.trim()
        )
    })?;
    validate_release(root, &sha, version)
}

/// Decision for a push to `main` whose head is `sha`. A push never creates a
/// tag: it only validates a release commit, so that a broken one fails its own
/// run, and the tag is made later by an explicit candidate or stable run.
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
    let release = validate_release(root, &sha, version)?;
    let tag = format!("v{version}");
    match tag_commit(root, &tag) {
        Some(existing) if existing == sha => {}
        Some(existing) => {
            return Err(format!("{tag} already exists at {existing}, not at {sha}").into())
        }
        None => require_above_highest_stable(root, &release.version)?,
    }
    Ok(Decision {
        action: Action::Validated,
        tag,
        sha,
    })
}

/// Decision for release candidate `number` of a release commit of `main`: the
/// tag `vX.Y.Z-rc.N` goes on the release commit itself, `HEAD` or the one named
/// in `release_commit`. Once the stable tag of that version exists, a candidate
/// is refused: the stable tag is made from an accepted candidate, never before.
pub fn decide_candidate(
    root: &Path,
    number: &str,
    release_commit: Option<&str>,
) -> Result<Decision> {
    let valid = !number.is_empty()
        && number.bytes().all(|byte| byte.is_ascii_digit())
        && !number.starts_with('0');
    if !valid {
        return Err(format!(
            "candidate '{number}' must be a positive integer without leading zeros"
        )
        .into());
    }
    let release = manual_release(root, release_commit)?;
    let stable = format!("v{}", release.version);
    let tag = format!("{stable}-rc.{number}");
    let sha = release.sha;
    match tag_commit(root, &tag) {
        Some(existing) if existing == sha => Ok(Decision {
            action: Action::Exists,
            tag,
            sha,
        }),
        Some(existing) => Err(format!("{tag} already exists at {existing}, not at {sha}").into()),
        None => {
            if tag_commit(root, &stable).is_some() {
                return Err(format!(
                    "{stable} already exists; a candidate is only made before the stable release"
                )
                .into());
            }
            require_above_highest_stable(root, &release.version)?;
            Ok(Decision {
                action: Action::Create,
                tag,
                sha,
            })
        }
    }
}

/// Candidate tags `vX.Y.Z-rc.N` (positive `N`) that point at `sha`.
fn candidates_at(root: &Path, version: &str, sha: &str) -> Result<Vec<String>> {
    let prefix = format!("v{version}-rc.");
    let listed = git(
        root,
        &["tag", "--points-at", sha, "--list", &format!("{prefix}*")],
    )?;
    Ok(listed
        .lines()
        .filter(|name| {
            name.strip_prefix(&prefix).is_some_and(|number| {
                !number.is_empty()
                    && number.bytes().all(|byte| byte.is_ascii_digit())
                    && !number.starts_with('0')
            })
        })
        .map(str::to_string)
        .collect())
}

/// Decision for the stable tag of a release commit of `main`, `HEAD` or the one
/// named in `release_commit`. It is made only on a commit that already carries
/// a candidate tag of the same version, so the stable bytes are the tested
/// bytes.
pub fn decide_stable(root: &Path, release_commit: Option<&str>) -> Result<Decision> {
    let release = manual_release(root, release_commit)?;
    let tag = format!("v{}", release.version);
    let sha = release.sha;
    if candidates_at(root, &release.version, &sha)?.is_empty() {
        return Err(format!(
            "no release candidate v{}-rc.N points at {sha}; make and accept a candidate on this commit first",
            release.version
        )
        .into());
    }
    match tag_commit(root, &tag) {
        Some(existing) if existing == sha => Ok(Decision {
            action: Action::Exists,
            tag,
            sha,
        }),
        Some(existing) => Err(format!("{tag} already exists at {existing}, not at {sha}").into()),
        None => {
            require_above_highest_stable(root, &release.version)?;
            Ok(Decision {
                action: Action::Create,
                tag,
                sha,
            })
        }
    }
}
