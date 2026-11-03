//! The release plan reads pull request titles, chooses the version and rewrites
//! the repository. These tests pin the version rules, the changelog text and a
//! whole preparation in a real Git repository.

use among_tools::release_plan::{
    changelog_section, changelog_text, civil_date, metainfo_with_release, next_version,
    parse_commit, prepare, write, Commit,
};
use among_tools::{storage, version_locations, Result};
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

fn commit(subject: &str) -> Commit {
    parse_commit(subject, "").unwrap_or_else(|| Commit {
        kind: "invalid".into(),
        scope: None,
        breaking: false,
        description: subject.into(),
        pull_request: None,
    })
}

fn commits(subjects: &[&str]) -> Vec<Commit> {
    subjects.iter().map(|subject| commit(subject)).collect()
}

#[test]
fn titles_are_parsed_with_scope_breaking_mark_and_pull_request_number() {
    let parsed = parse_commit("feat(plugins)!: drop the old catalogue (#12)", "");
    assert_eq!(
        parsed,
        Some(Commit {
            kind: "feat".into(),
            scope: Some("plugins".into()),
            breaking: true,
            description: "drop the old catalogue".into(),
            pull_request: Some(12),
        })
    );
    let footer = parse_commit(
        "fix: change the socket",
        "Why.\n\nBREAKING CHANGE: new path\n",
    );
    assert!(footer.is_some_and(|commit| commit.breaking));
    for rejected in [
        "Merge pull request #5 from x/y",
        "Update thing",
        "feature: x",
        "feat: ",
        "feat(): x",
        "feat(a: x",
    ] {
        assert!(parse_commit(rejected, "").is_none(), "{rejected}");
    }
}

#[test]
fn before_one_point_zero_features_and_breaking_changes_raise_the_minor_fixes_the_patch() {
    let base = (0, 1, 1);
    assert_eq!(
        next_version(base, &commits(&["feat: a", "fix: b"])),
        Some((0, 2, 0))
    );
    assert_eq!(next_version(base, &commits(&["fix!: a"])), Some((0, 2, 0)));
    assert_eq!(next_version(base, &commits(&["docs!: a"])), Some((0, 2, 0)));
    assert_eq!(
        next_version(base, &commits(&["fix: a", "perf: b"])),
        Some((0, 1, 2))
    );
    assert_eq!(next_version(base, &commits(&["perf: b"])), Some((0, 1, 2)));
}

#[test]
fn from_one_point_zero_a_breaking_change_raises_the_major() {
    assert_eq!(
        next_version((1, 4, 2), &commits(&["feat!: a", "feat: b"])),
        Some((2, 0, 0))
    );
    assert_eq!(
        next_version((1, 4, 2), &commits(&["feat: b", "fix: c"])),
        Some((1, 5, 0))
    );
    assert_eq!(
        next_version((1, 4, 2), &commits(&["fix: c"])),
        Some((1, 4, 3))
    );
}

#[test]
fn commits_without_user_visible_change_make_no_release() {
    let quiet = commits(&[
        "docs: a",
        "test: b",
        "ci: c",
        "chore: d",
        "build: e",
        "refactor: f",
    ]);
    assert_eq!(next_version((0, 1, 1), &quiet), None);
    assert_eq!(next_version((0, 1, 1), &[]), None);
    // The release commit itself never starts another release.
    assert_eq!(
        next_version((0, 1, 1), &commits(&["chore(release): 0.2.0 (#9)"])),
        None
    );
}

#[test]
fn the_changelog_groups_user_visible_changes_and_hides_the_rest() {
    let list = commits(&[
        "feat(panes): drag panes (#3)",
        "fix: keep focus (#4)",
        "perf: faster redraw",
        "refactor!: rename the socket (#5)",
        "docs: words",
        "chore(release): 0.1.0 (#1)",
    ]);
    let section = changelog_section("0.2.0", "2026-11-03", &list);
    assert_eq!(
        section,
        "## 0.2.0 - 2026-11-03\n\n### Breaking changes\n\n- rename the socket (#5)\n\n\
### Features\n\n- panes: drag panes (#3)\n\n### Bug fixes\n\n- keep focus (#4)\n\n\
### Performance\n\n- faster redraw\n"
    );
    let first = changelog_text(None, &section);
    assert!(first.starts_with("# Changelog\n"));
    let newer = changelog_section("0.3.0", "2026-12-01", &commits(&["fix: later"]));
    let both = changelog_text(Some(&first), &newer);
    let newest = both.find("## 0.3.0").unwrap_or(usize::MAX);
    let oldest = both.find("## 0.2.0").unwrap_or(0);
    assert!(newest < oldest, "{both}");
    assert_eq!(both.matches("# Changelog").count(), 1);
}

#[test]
fn the_metainfo_gets_a_new_stable_entry_in_front_and_escapes_text() -> Result<()> {
    let xml = "<component>\n  <releases>\n    <release version=\"0.1.0\" date=\"2026-01-01\" type=\"stable\"/>\n  </releases>\n</component>\n";
    let rendered = metainfo_with_release(
        xml,
        "0.2.0",
        "2026-11-03",
        &commits(&["feat: a <b> & c (#7)"]),
    )?;
    let document = roxmltree::Document::parse(&rendered)?;
    let releases: Vec<_> = document
        .descendants()
        .filter(|node| node.has_tag_name("release"))
        .collect();
    assert_eq!(releases.len(), 2);
    assert_eq!(releases[0].attribute("version"), Some("0.2.0"));
    assert_eq!(releases[0].attribute("type"), Some("stable"));
    assert_eq!(releases[1].attribute("version"), Some("0.1.0"));
    assert!(rendered.contains("a &lt;b&gt; &amp; c (#7)"));
    assert!(metainfo_with_release(xml, "0.2.0", "2026-11-03", &commits(&["docs: x"])).is_err());
    Ok(())
}

#[test]
fn dates_are_civil_utc() {
    assert_eq!(civil_date(0), "1970-01-01");
    assert_eq!(civil_date(1_782_000_000), "2026-06-21");
    assert_eq!(civil_date(1_804_000_000), "2027-03-02");
    assert_eq!(civil_date(951_782_400), "2000-02-29");
}

fn git(root: &Path, args: &[&str]) -> Result<()> {
    let status = Command::new("/usr/bin/git")
        .args(args)
        .current_dir(root)
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@example.invalid")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@example.invalid")
        .status()?;
    assert!(status.success(), "git {args:?}");
    Ok(())
}

/// A Git repository holding copies of the real version files, tagged `v0.1.1`.
fn repository_with_tag() -> Result<PathBuf> {
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let root = storage::private_directory(&std::env::temp_dir(), "among-release-plan")?;
    for file in version_locations::LOCATIONS
        .iter()
        .map(|location| location.path)
        .chain(["packaging/debian/changelog"])
    {
        let target = root.join(file);
        fs::create_dir_all(target.parent().ok_or("no parent")?)?;
        fs::copy(source.join(file), target)?;
    }
    git(&root, &["init", "-q", "-b", "main"])?;
    git(&root, &["add", "."])?;
    git(&root, &["commit", "-q", "-m", "chore: start"])?;
    git(&root, &["tag", "v0.1.1"])?;
    Ok(root)
}

#[test]
fn a_release_is_prepared_from_titles_since_the_stable_tag_and_passes_the_check() -> Result<()> {
    let root = repository_with_tag()?;
    // A candidate tag is not a release: the base stays v0.1.1.
    git(
        &root,
        &[
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            "docs: no release by itself",
        ],
    )?;
    git(&root, &["tag", "v9.9.9-rc.1"])?;
    git(
        &root,
        &[
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            "fix: keep focus (#4)",
        ],
    )?;
    // The files already say 0.1.2, so a fix-only release (0.1.2) has nothing to bump.
    assert!(prepare(&root, "2026-11-03").is_err());

    git(
        &root,
        &[
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            "feat(panes): drag (#5)",
        ],
    )?;
    let prepared = prepare(&root, "2026-11-03")?.ok_or("no release")?;
    assert_eq!(prepared.version, "0.2.0");
    assert_eq!(prepared.title, "chore(release): 0.2.0");
    assert!(prepared.body.contains("keep focus (#4)") && prepared.body.contains("drag (#5)"));
    write(&root, &prepared)?;

    assert_eq!(version_locations::check(&root)?, "0.2.0");
    let changelog = fs::read_to_string(root.join("CHANGELOG.md"))?;
    assert!(changelog.contains("## 0.2.0 - 2026-11-03"));
    let metainfo = fs::read_to_string(root.join(version_locations::METAINFO))?;
    assert!(metainfo.contains("<release version=\"0.2.0\" date=\"2026-11-03\" type=\"stable\">"));
    // Preparing again on the same tree has nothing new to release.
    assert!(prepare(&root, "2026-11-03").is_err());
    Ok(())
}

#[test]
fn nothing_is_prepared_without_a_user_visible_change() -> Result<()> {
    let root = repository_with_tag()?;
    git(&root, &["commit", "-q", "--allow-empty", "-m", "ci: tweak"])?;
    assert!(prepare(&root, "2026-11-03")?.is_none());
    Ok(())
}
