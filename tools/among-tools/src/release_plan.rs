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

/// User-visible commits grouped by kind, in the order both the changelog and
/// the AppStream entry list them. A breaking change appears only under
/// "Breaking changes".
fn groups(commits: &[Commit]) -> Vec<(&'static str, Vec<&Commit>)> {
    let relevant = || commits.iter().filter(|commit| !commit.is_release_commit());
    [
        ("Breaking changes", "", true),
        ("Features", "feat", false),
        ("Bug fixes", "fix", false),
        ("Performance", "perf", false),
    ]
    .into_iter()
    .map(|(title, kind, breaking)| {
        let members: Vec<&Commit> = relevant()
            .filter(|commit| {
                if breaking {
                    commit.breaking
                } else {
                    !commit.breaking && commit.kind == kind
                }
            })
            .collect();
        (title, members)
    })
    .filter(|(_, members)| !members.is_empty())
    .collect()
}

/// The release section of the changelog, without a trailing blank line.
pub fn changelog_section(version: &str, date: &str, commits: &[Commit]) -> String {
    let mut section = format!("## {version} - {date}\n");
    for (title, members) in groups(commits) {
        let _ = write!(section, "\n### {title}\n\n");
        for commit in members {
            let _ = writeln!(section, "- {}", item(commit));
        }
    }
    section
}

const CHANGELOG_HEADER: &str =
    "# Changelog\n\nUser-visible changes of the application, newest first. \
Written by `among-tools release-prepare` from the titles of the merged pull requests.\n";

/// `existing` changelog text with `section` added as the newest release. A
/// file whose first release starts at the very first line, without any header,
/// gets the section in front of it.
pub fn changelog_text(existing: Option<&str>, section: &str) -> String {
    match existing {
        None => format!("{CHANGELOG_HEADER}\n{section}"),
        Some(text) if text.starts_with("## ") => format!("{section}\n{text}"),
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
/// The older entries stay as the history of earlier versions. The notes are
/// grouped like the changelog: one paragraph and list per kind of change.
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
    let grouped = groups(commits);
    if grouped.is_empty() {
        return Err("a release needs at least one user-visible change".into());
    }
    let mut entry = format!(
        "{indent}  <release version=\"{version}\" date=\"{date}\" type=\"stable\">\n\
{indent}    <description>\n"
    );
    for (title, members) in grouped {
        let _ = writeln!(entry, "{indent}      <p>{title}</p>");
        let _ = writeln!(entry, "{indent}      <ul>");
        for commit in members {
            let _ = writeln!(
                entry,
                "{indent}        <li>{}</li>",
                escape_xml(&item(commit))
            );
        }
        let _ = writeln!(entry, "{indent}      </ul>");
    }
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

pub(crate) fn git(root: &Path, args: &[&str]) -> Result<String> {
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

fn stable_tags(listing: &str) -> Vec<(String, (u64, u64, u64))> {
    listing
        .lines()
        .filter_map(|tag| {
            let version = version_locations::triple(tag.strip_prefix('v')?).ok()?;
            Some((tag.to_string(), version))
        })
        .collect()
}

/// The newest stable tag (`vMAJOR.MINOR.PATCH`) reachable from `HEAD`.
/// Release candidates (`-rc.N`) are not releases of their own.
pub fn last_stable_tag(root: &Path) -> Result<(String, (u64, u64, u64))> {
    stable_tags(&git(root, &["tag", "--merged", "HEAD", "--list", "v*"])?)
        .into_iter()
        .max_by_key(|(_, version)| *version)
        .ok_or_else(|| "no stable v* tag is reachable from HEAD".into())
}

/// The highest stable version among all tags of the repository, if any.
pub fn highest_stable_version(root: &Path) -> Result<Option<(u64, u64, u64)>> {
    Ok(stable_tags(&git(root, &["tag", "--list", "v*"])?)
        .into_iter()
        .map(|(_, version)| version)
        .max())
}

/// A commit whose title is not a Conventional Commit of an allowed type. Such
/// a commit cannot influence the version or the changelog.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Untitled {
    pub sha: String,
    pub subject: String,
}

/// Commits after `tag`, merge commits excluded: the titled ones, and the ones
/// that were left out because their title does not parse.
pub fn commits_since(root: &Path, tag: &str) -> Result<(Vec<Commit>, Vec<Untitled>)> {
    let log = git(
        root,
        &[
            "log",
            "--no-merges",
            "--format=%H%x1f%s%x1f%b%x1e",
            &format!("{tag}..HEAD"),
        ],
    )?;
    let mut titled = Vec::new();
    let mut untitled = Vec::new();
    for record in log
        .split('\u{1e}')
        .filter(|record| !record.trim().is_empty())
    {
        let mut fields = record.trim_start_matches('\n').splitn(3, '\u{1f}');
        let (Some(sha), Some(subject), Some(body)) = (fields.next(), fields.next(), fields.next())
        else {
            return Err("unreadable git log record".into());
        };
        match parse_commit(subject.trim(), body) {
            Some(commit) => titled.push(commit),
            None => untitled.push(Untitled {
                sha: sha.trim().to_string(),
                subject: subject.trim().to_string(),
            }),
        }
    }
    Ok((titled, untitled))
}

/// What a release would contain.
pub struct Plan {
    pub tag: String,
    pub base: (u64, u64, u64),
    pub next: Option<(u64, u64, u64)>,
    pub commits: Vec<Commit>,
    pub untitled: Vec<Untitled>,
}

pub fn plan(root: &Path) -> Result<Plan> {
    let (tag, base) = last_stable_tag(root)?;
    let (commits, untitled) = commits_since(root, &tag)?;
    Ok(Plan {
        next: next_version(base, &commits),
        tag,
        base,
        commits,
        untitled,
    })
}

/// The release pull request: its title, its body and the files it changes.
pub struct Prepared {
    pub version: String,
    pub title: String,
    pub body: String,
    pub files: Vec<(String, String)>,
}

/// The untitled commits named by `accepted` (SHA prefixes of at least seven
/// hexadecimal digits). Every untitled commit must be named and every name must
/// match one, so that a commit is never dropped without a person saying so.
fn acknowledge<'a>(untitled: &'a [Untitled], accepted: &[String]) -> Result<Vec<&'a Untitled>> {
    for name in accepted {
        let valid = name.len() >= 7 && name.bytes().all(|byte| byte.is_ascii_hexdigit());
        if !valid
            || untitled
                .iter()
                .filter(|commit| commit.sha.starts_with(&name.to_ascii_lowercase()))
                .count()
                != 1
        {
            return Err(format!(
                "--accept-untitled names {name}, which is not exactly one commit without a Conventional Commit title"
            )
            .into());
        }
    }
    let missing: Vec<String> = untitled
        .iter()
        .filter(|commit| {
            !accepted
                .iter()
                .any(|name| commit.sha.starts_with(&name.to_ascii_lowercase()))
        })
        .map(|commit| {
            format!(
                "  {} {}",
                &commit.sha[..12.min(commit.sha.len())],
                commit.subject
            )
        })
        .collect();
    if !missing.is_empty() {
        return Err(format!(
            "these commits have no Conventional Commit title and would be left out of the release:\n{}\n\
Name them with --accept-untitled <sha>,<sha>... to leave them out on purpose",
            missing.join("\n")
        )
        .into());
    }
    Ok(untitled.iter().collect())
}

/// Computes the release and returns the new text of every file it changes.
/// Writes nothing. `accepted` names the untitled commits that are left out on
/// purpose; they are recorded in the pull request body.
///
/// The version is the larger of the one the titles call for and the one the
/// files already say: if the files were bumped ahead of the last tag, that
/// version is released even when the titles alone would call for less.
pub fn prepare(root: &Path, date: &str, accepted: &[String]) -> Result<Option<Prepared>> {
    let plan = plan(root)?;
    let Some(computed) = plan.next else {
        return Ok(None);
    };
    let left_out = acknowledge(&plan.untitled, accepted)?;
    let current = version_locations::check(root)?;
    let (major, minor, patch) = computed.max(version_locations::triple(&current)?);
    let version = format!("{major}.{minor}.{patch}");
    let existing = std::fs::read_to_string(root.join(CHANGELOG)).ok();
    if existing.as_deref().is_some_and(|text| {
        text.contains(&format!("\n## {version} - "))
            || text.starts_with(&format!("## {version} - "))
    }) {
        return Err(format!(
            "release {version} is already prepared (CHANGELOG.md has its section); tag it, or wait for new changes"
        )
        .into());
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
    files.push((
        CHANGELOG.to_string(),
        changelog_text(existing.as_deref(), &section),
    ));
    let mut body = format!(
        "Release {version}, computed from the pull request titles since {}.\n\n{section}\n",
        plan.tag
    );
    if !left_out.is_empty() {
        body.push_str("Commits without a Conventional Commit title, left out on purpose:\n\n");
        for commit in left_out {
            let _ = writeln!(
                body,
                "- {} {}",
                &commit.sha[..12.min(commit.sha.len())],
                commit.subject
            );
        }
        body.push('\n');
    }
    let _ = writeln!(
        body,
        "Before merging, add `{}` with the release highlights; the checks fail without it. \
Merging this pull request tags the merge commit `v{version}` and starts the release draft. \
The draft is published by hand.",
        notes_path(&version)
    );
    Ok(Some(Prepared {
        title: format!("chore(release): {version}"),
        version,
        body,
        files,
    }))
}

/// Where the hand-written highlights of a stable release live.
pub fn notes_path(version: &str) -> String {
    format!(".github/release-notes/v{version}.md")
}

fn ensure_clean_working_branch(root: &Path) -> Result<()> {
    let branch = git(root, &["rev-parse", "--abbrev-ref", "HEAD"])?;
    if matches!(branch.trim(), "main" | "HEAD") {
        return Err(format!(
            "refusing to prepare a release on {}: create a working branch first",
            branch.trim()
        )
        .into());
    }
    if !git(root, &["status", "--porcelain", "--untracked-files=no"])?
        .trim()
        .is_empty()
    {
        return Err("refusing to prepare a release in a tree with uncommitted changes".into());
    }
    Ok(())
}

/// Writes the files of a prepared release, all or none: every target is
/// resolved first, every new text goes to a temporary file next to its target,
/// and only when all of them exist are they renamed into place.
pub fn write(root: &Path, prepared: &Prepared) -> Result<()> {
    ensure_clean_working_branch(root)?;
    let mut targets = Vec::new();
    for (path, text) in &prepared.files {
        let target = if root.join(path).exists() {
            storage::safe_path(root, Path::new(path))?
        } else {
            storage::relative_path(Path::new(path))?;
            root.canonicalize()?.join(path)
        };
        let name = target.file_name().ok_or("release file has no name")?;
        let temporary = target.with_file_name(format!(".{}.release-tmp", name.to_string_lossy()));
        targets.push((target, temporary, text));
    }
    let staged = (|| -> Result<()> {
        for (target, temporary, text) in &targets {
            std::fs::write(temporary, text)?;
            if let Ok(metadata) = std::fs::metadata(target) {
                std::fs::set_permissions(temporary, metadata.permissions())?;
            }
        }
        Ok(())
    })();
    if let Err(error) = staged {
        for (_, temporary, _) in &targets {
            let _ = std::fs::remove_file(temporary);
        }
        return Err(error);
    }
    for (target, temporary, _) in &targets {
        std::fs::rename(temporary, target)?;
    }
    Ok(())
}
