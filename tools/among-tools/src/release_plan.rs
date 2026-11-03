//! Prepares the release of the application from the squash-merged pull request
//! titles since the last stable tag.
//!
//! Pull requests merge by squash, so each commit on `main` carries one
//! Conventional Commit title. This module reads those titles, chooses the next
//! version, rewrites every version location (`version_locations`), adds the
//! release to `CHANGELOG.md` and to the AppStream release list, and renders the
//! text of the release pull request. The operator opens that pull request with
//! their own credentials, so the required checks run on it like on any other.
//! Nothing here tags, publishes or talks to GitHub.

use crate::{storage, version_locations, Result};
use std::fmt::Write as _;
use std::path::Path;
use std::process::Command;

const CHANGELOG: &str = "CHANGELOG.md";
const TYPES: [&str; 9] = [
    "feat", "fix", "docs", "test", "ci", "build", "perf", "refactor", "chore",
];

/// One pull request title, parsed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Commit {
    pub kind: String,
    pub scope: Option<String>,
    pub breaking: bool,
    pub description: String,
    pub pull_request: Option<u64>,
}

impl Commit {
    fn is_release_commit(&self) -> bool {
        self.kind == "chore" && self.scope.as_deref() == Some("release")
    }
}

/// Parses `type(scope)!: description (#N)`. `None` when the subject is not a
/// Conventional Commit of an allowed type. A `BREAKING CHANGE:` footer in the
/// body also marks a breaking change.
pub fn parse_commit(subject: &str, body: &str) -> Option<Commit> {
    let (head, rest) = subject.split_once(": ")?;
    let (head, bang) = match head.strip_suffix('!') {
        Some(head) => (head, true),
        None => (head, false),
    };
    let (kind, scope) = match head.split_once('(') {
        Some((kind, scope)) => (kind, Some(scope.strip_suffix(')')?.to_string())),
        None => (head, None),
    };
    if !TYPES.contains(&kind) || scope.as_deref().is_some_and(str::is_empty) {
        return None;
    }
    let rest = rest.trim();
    let (description, pull_request) = match rest.rsplit_once(" (#") {
        Some((description, number)) => match number.strip_suffix(')').map(str::parse::<u64>) {
            Some(Ok(number)) => (description.trim(), Some(number)),
            _ => (rest, None),
        },
        None => (rest, None),
    };
    if description.is_empty() {
        return None;
    }
    let footer = body
        .lines()
        .any(|line| line.starts_with("BREAKING CHANGE:") || line.starts_with("BREAKING-CHANGE:"));
    Some(Commit {
        kind: kind.to_string(),
        scope,
        breaking: bang || footer,
        description: description.to_string(),
        pull_request,
    })
}

/// The next version, or `None` when no commit is user-facing. Before 1.0 a
/// breaking change and a feature raise the minor version; a fix or a
/// performance change raises the patch version. From 1.0 a breaking change
/// raises the major version.
pub fn next_version(base: (u64, u64, u64), commits: &[Commit]) -> Option<(u64, u64, u64)> {
    let relevant = || commits.iter().filter(|commit| !commit.is_release_commit());
    let breaking = relevant().any(|commit| commit.breaking);
    let feature = relevant().any(|commit| commit.kind == "feat");
    let fix = relevant().any(|commit| commit.kind == "fix" || commit.kind == "perf");
    let (major, minor, patch) = base;
    if breaking && major >= 1 {
        Some((major + 1, 0, 0))
    } else if breaking || feature {
        Some((major, minor + 1, 0))
    } else if fix {
        Some((major, minor, patch + 1))
    } else {
        None
    }
}

fn item(commit: &Commit) -> String {
    let mut text = String::new();
    if let Some(scope) = &commit.scope {
        let _ = write!(text, "{scope}: ");
    }
    text.push_str(&commit.description);
    if let Some(number) = commit.pull_request {
        let _ = write!(text, " (#{number})");
    }
    text
}

/// The release section of the changelog, without a trailing blank line.
pub fn changelog_section(version: &str, date: &str, commits: &[Commit]) -> String {
    let relevant: Vec<&Commit> = commits
        .iter()
        .filter(|commit| !commit.is_release_commit())
        .collect();
    let groups: [(&str, Vec<&Commit>); 4] = [
        (
            "Breaking changes",
            relevant.iter().copied().filter(|c| c.breaking).collect(),
        ),
        (
            "Features",
            relevant
                .iter()
                .copied()
                .filter(|c| !c.breaking && c.kind == "feat")
                .collect(),
        ),
        (
            "Bug fixes",
            relevant
                .iter()
                .copied()
                .filter(|c| !c.breaking && c.kind == "fix")
                .collect(),
        ),
        (
            "Performance",
            relevant
                .iter()
                .copied()
                .filter(|c| !c.breaking && c.kind == "perf")
                .collect(),
        ),
    ];
    let mut section = format!("## {version} - {date}\n");
    for (title, members) in groups {
        if members.is_empty() {
            continue;
        }
        let _ = write!(section, "\n### {title}\n\n");
        for commit in members {
            let _ = writeln!(section, "- {}", item(commit));
        }
    }
    section
}

/// `existing` changelog text with `section` added as the newest release.
pub fn changelog_text(existing: Option<&str>, section: &str) -> String {
    const HEADER: &str = "# Changelog\n\nUser-visible changes of the application, newest first. \
Written by `among-tools release-prepare` from the titles of the merged pull requests.\n";
    match existing {
        None => format!("{HEADER}\n{section}"),
        Some(text) => match text.find("\n## ") {
            Some(position) => format!(
                "{}\n{section}\n{}",
                &text[..position],
                &text[position + 1..]
            ),
            None => format!("{}\n{section}", text.trim_end_matches('\n')),
        },
    }
}

fn escape_xml(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// `metainfo` with a stable release entry added in front of the existing ones.
/// The older entries stay as the history of earlier versions.
pub fn metainfo_with_release(
    metainfo: &str,
    version: &str,
    date: &str,
    commits: &[Commit],
) -> Result<String> {
    let mut lines: Vec<&str> = metainfo.split_inclusive('\n').collect();
    let position = lines
        .iter()
        .position(|line| line.trim() == "<releases>")
        .ok_or("AppStream metainfo has no <releases> line")?;
    let indent = &lines[position][..lines[position].len() - lines[position].trim_start().len()];
    let mut entry = format!(
        "{indent}  <release version=\"{version}\" date=\"{date}\" type=\"stable\">\n\
{indent}    <description>\n"
    );
    let items: Vec<String> = commits
        .iter()
        .filter(|commit| {
            !commit.is_release_commit()
                && (commit.breaking
                    || commit.kind == "feat"
                    || commit.kind == "fix"
                    || commit.kind == "perf")
        })
        .map(|commit| escape_xml(&item(commit)))
        .collect();
    if items.is_empty() {
        return Err("a release needs at least one user-visible change".into());
    }
    let _ = writeln!(entry, "{indent}      <ul>");
    for text in items {
        let _ = writeln!(entry, "{indent}        <li>{text}</li>");
    }
    let _ = writeln!(entry, "{indent}      </ul>");
    let _ = writeln!(entry, "{indent}    </description>");
    let _ = writeln!(entry, "{indent}  </release>");
    lines.insert(position + 1, &entry);
    Ok(lines.concat())
}

/// `YYYY-MM-DD` of a Unix time, UTC.
pub fn civil_date(unix_seconds: u64) -> String {
    // Days-to-civil conversion for the proleptic Gregorian calendar.
    let days = (unix_seconds / 86_400) as i64 + 719_468;
    let era = days.div_euclid(146_097);
    let day_of_era = days.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted_month = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * shifted_month + 2) / 5 + 1;
    let month = if shifted_month < 10 {
        shifted_month + 3
    } else {
        shifted_month - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}")
}

fn git(root: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new("/usr/bin/git")
        .args(args)
        .current_dir(root)
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .output()?;
    if !output.status.success() {
        return Err(format!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        )
        .into());
    }
    Ok(String::from_utf8(output.stdout)?)
}

/// The newest stable tag (`vMAJOR.MINOR.PATCH`) reachable from `HEAD`.
/// Release candidates (`-rc.N`) are not releases of their own.
pub fn last_stable_tag(root: &Path) -> Result<(String, (u64, u64, u64))> {
    git(root, &["tag", "--merged", "HEAD", "--list", "v*"])?
        .lines()
        .filter_map(|tag| {
            let version = version_locations::triple(tag.strip_prefix('v')?).ok()?;
            Some((tag.to_string(), version))
        })
        .max_by_key(|(_, version)| *version)
        .ok_or_else(|| "no stable v* tag is reachable from HEAD".into())
}

/// Titles of the commits after `tag`, merge commits excluded.
pub fn commits_since(root: &Path, tag: &str) -> Result<Vec<Commit>> {
    let log = git(
        root,
        &[
            "log",
            "--no-merges",
            "--format=%x1e%s%x1f%b",
            &format!("{tag}..HEAD"),
        ],
    )?;
    Ok(log
        .split('\u{1e}')
        .filter_map(|record| {
            let (subject, body) = record.split_once('\u{1f}')?;
            parse_commit(subject.trim(), body)
        })
        .collect())
}

/// What a release would contain.
pub struct Plan {
    pub tag: String,
    pub base: (u64, u64, u64),
    pub next: Option<(u64, u64, u64)>,
    pub commits: Vec<Commit>,
}

pub fn plan(root: &Path) -> Result<Plan> {
    let (tag, base) = last_stable_tag(root)?;
    let commits = commits_since(root, &tag)?;
    Ok(Plan {
        next: next_version(base, &commits),
        tag,
        base,
        commits,
    })
}

/// The release pull request: its title, its body and the files it changes.
pub struct Prepared {
    pub version: String,
    pub title: String,
    pub body: String,
    pub files: Vec<(String, String)>,
}

/// Computes the release and returns the new text of every file it changes.
/// Writes nothing.
pub fn prepare(root: &Path, date: &str) -> Result<Option<Prepared>> {
    let plan = plan(root)?;
    let Some((major, minor, patch)) = plan.next else {
        return Ok(None);
    };
    let version = format!("{major}.{minor}.{patch}");
    let current = version_locations::check(root)?;
    if version_locations::triple(&current)? >= (major, minor, patch) {
        return Err(
            format!("the files already say {current}; the next release is {version}").into(),
        );
    }
    let mut files: Vec<(String, String)> = version_locations::rewrite_all(root, &version)?
        .into_iter()
        .map(|(path, text)| (path.to_string(), text))
        .collect();
    for (path, text) in &mut files {
        if path == version_locations::METAINFO {
            *text = metainfo_with_release(text, &version, date, &plan.commits)?;
        }
    }
    let section = changelog_section(&version, date, &plan.commits);
    let existing = std::fs::read_to_string(root.join(CHANGELOG)).ok();
    files.push((
        CHANGELOG.to_string(),
        changelog_text(existing.as_deref(), &section),
    ));
    let body = format!(
        "Release {version}, computed from the pull request titles since {}.\n\n{section}\n\
Merging this pull request tags the merge commit `v{version}` and starts the release draft. \
The draft is published by hand.\n",
        plan.tag
    );
    Ok(Some(Prepared {
        title: format!("chore(release): {version}"),
        version,
        body,
        files,
    }))
}

/// Writes the files of a prepared release.
pub fn write(root: &Path, prepared: &Prepared) -> Result<()> {
    for (path, text) in &prepared.files {
        let target = if root.join(path).exists() {
            storage::safe_path(root, Path::new(path))?
        } else {
            root.join(path)
        };
        std::fs::write(target, text)?;
    }
    Ok(())
}
