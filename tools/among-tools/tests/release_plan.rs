//! The release plan reads pull request titles, chooses the version and rewrites
//! the repository. These tests pin the version rules, the changelog text, the
//! refusal to drop commits silently and whole preparations in synthetic Git
//! repositories, including the state after the release pull request itself.

mod release_fixture;

use among_tools::release_plan::{
    changelog_section, changelog_text, civil_date, metainfo_with_release, next_version,
    parse_commit, prepare, write, Commit,
};
use among_tools::{version_locations, Result};
use release_fixture::{commit_all, empty_commit, git, repository, write as write_file};
use std::fs;

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
        "Revert \"feat: x\"",
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
fn a_changelog_whose_first_release_starts_the_file_gets_the_new_section_in_front() {
    let old = "## 0.1.0 - 2026-01-01\n\n### Features\n\n- first\n";
    let newer = changelog_section("0.2.0", "2026-11-03", &commits(&["fix: later"]));
    let both = changelog_text(Some(old), &newer);
    assert!(both.starts_with("## 0.2.0 - 2026-11-03\n"), "{both}");
    assert!(both.find("## 0.2.0").unwrap_or(usize::MAX) < both.find("## 0.1.0").unwrap_or(0));
    assert_eq!(both.matches("## 0.1.0").count(), 1);
}

#[test]
fn the_metainfo_gets_a_grouped_stable_entry_in_front_and_escapes_text() -> Result<()> {
    let xml = "<component>\n  <releases>\n    <release version=\"0.1.0\" date=\"2026-01-01\" type=\"stable\"/>\n  </releases>\n</component>\n";
    let list = commits(&["fix: later (#8)", "feat: a <b> & c (#7)", "docs: no"]);
    let rendered = metainfo_with_release(xml, "0.2.0", "2026-11-03", &list)?;
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
    // Features come before fixes whatever the order of the Git log, each under
    // its own heading.
    let features = rendered.find("<p>Features</p>").unwrap_or(usize::MAX);
    let fixes = rendered.find("<p>Bug fixes</p>").unwrap_or(0);
    assert!(features < fixes, "{rendered}");
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

/// A repository tagged v0.1.1 with version files at 0.1.1, on a working branch.
fn at_first_release() -> Result<std::path::PathBuf> {
    let root = repository("0.1.1", "0.1.1")?;
    git(&root, &["checkout", "-q", "-b", "work"])?;
    Ok(root)
}

#[test]
fn a_release_is_prepared_from_titles_since_the_stable_tag_and_needs_its_highlights() -> Result<()> {
    let root = at_first_release()?;
    // A candidate tag is not a release: the base stays v0.1.1.
    empty_commit(&root, "docs: no release by itself")?;
    git(&root, &["tag", "v9.9.9-rc.1"])?;
    empty_commit(&root, "fix: keep focus (#4)")?;
    empty_commit(&root, "feat(panes): drag (#5)")?;
    let prepared = prepare(&root, "2026-11-03", &[])?.ok_or("no release")?;
    assert_eq!(prepared.version, "0.2.0");
    assert_eq!(prepared.title, "chore(release): 0.2.0");
    assert!(prepared.body.contains("keep focus (#4)") && prepared.body.contains("drag (#5)"));
    assert!(prepared.body.contains(".github/release-notes/v0.2.0.md"));
    write(&root, &prepared)?;

    // The files agree, but the prepared release has no highlights yet.
    let error = version_locations::check(&root).err().ok_or("no failure")?;
    assert!(
        error.to_string().contains("release-notes/v0.2.0.md"),
        "{error}"
    );
    write_file(&root, ".github/release-notes/v0.2.0.md", "  \n")?;
    assert!(version_locations::check(&root).is_err(), "empty highlights");
    write_file(&root, ".github/release-notes/v0.2.0.md", "Drag panes.\n")?;
    assert_eq!(version_locations::check(&root)?, "0.2.0");
    let changelog = fs::read_to_string(root.join("CHANGELOG.md"))?;
    assert!(changelog.contains("## 0.2.0 - 2026-11-03"));
    let metainfo = fs::read_to_string(root.join("data/com.github.among-terminal.metainfo.xml"))?;
    assert!(metainfo.contains("<release version=\"0.2.0\" date=\"2026-11-03\" type=\"stable\">"));
    Ok(())
}

#[test]
fn after_the_release_pull_request_the_tool_still_works() -> Result<()> {
    let root = at_first_release()?;
    empty_commit(&root, "feat: drag (#5)")?;
    let prepared = prepare(&root, "2026-11-03", &[])?.ok_or("no release")?;
    write(&root, &prepared)?;
    write_file(&root, ".github/release-notes/v0.2.0.md", "Drag.\n")?;
    commit_all(&root, "chore(release): 0.2.0 (#9)")?;

    // Merged but not yet tagged: the same titles would call for 0.2.0 again.
    let error = prepare(&root, "2026-11-04", &[])
        .err()
        .ok_or("prepared twice")?;
    assert!(error.to_string().contains("already prepared"), "{error}");

    // Tagged: nothing new to release, then a fix releases the next patch.
    git(&root, &["tag", "v0.2.0"])?;
    assert!(prepare(&root, "2026-11-04", &[])?.is_none());
    empty_commit(&root, "fix: later (#10)")?;
    let next = prepare(&root, "2026-11-05", &[])?.ok_or("no release")?;
    assert_eq!(next.version, "0.2.1");
    write(&root, &next)?;
    write_file(&root, ".github/release-notes/v0.2.1.md", "Fixes.\n")?;
    assert_eq!(version_locations::check(&root)?, "0.2.1");
    let changelog = fs::read_to_string(root.join("CHANGELOG.md"))?;
    assert!(
        changelog.find("## 0.2.1").unwrap_or(usize::MAX) < changelog.find("## 0.2.0").unwrap_or(0)
    );
    Ok(())
}

#[test]
fn files_bumped_ahead_of_the_last_tag_are_released_even_with_fixes_only() -> Result<()> {
    // The files say 0.1.2 while the last stable tag is v0.1.1 and only a fix
    // happened: the titles alone call for 0.1.2, which is also what the files
    // say, and a lower computed version never downgrades the files.
    let root = repository("0.1.2", "0.1.1")?;
    git(&root, &["checkout", "-q", "-b", "work"])?;
    empty_commit(&root, "fix: keep focus (#4)")?;
    let prepared = prepare(&root, "2026-11-03", &[])?.ok_or("no release")?;
    assert_eq!(prepared.version, "0.1.2");
    write(&root, &prepared)?;
    write_file(&root, ".github/release-notes/v0.1.2.md", "Fix.\n")?;
    assert_eq!(version_locations::check(&root)?, "0.1.2");

    let ahead = repository("0.3.0", "0.1.1")?;
    git(&ahead, &["checkout", "-q", "-b", "work"])?;
    empty_commit(&ahead, "fix: keep focus (#4)")?;
    let prepared = prepare(&ahead, "2026-11-03", &[])?.ok_or("no release")?;
    assert_eq!(prepared.version, "0.3.0");
    Ok(())
}

#[test]
fn commits_without_a_title_are_never_dropped_silently() -> Result<()> {
    let root = at_first_release()?;
    empty_commit(&root, "feat: drag (#5)")?;
    let first = empty_commit(&root, "Replay the documented commands")?;
    let second = empty_commit(&root, "Test the refusal of an unclosed block")?;

    let error = prepare(&root, "2026-11-03", &[])
        .err()
        .ok_or("dropped silently")?;
    let text = error.to_string();
    assert!(
        text.contains("Replay the documented commands") && text.contains("unclosed block"),
        "{text}"
    );
    assert!(
        text.contains(&first[..12]) && text.contains("--accept-untitled"),
        "{text}"
    );

    // Naming only one of two is not enough, and a name that matches nothing fails.
    assert!(prepare(&root, "2026-11-03", &[first[..10].to_string()]).is_err());
    assert!(prepare(
        &root,
        "2026-11-03",
        &[first[..10].to_string(), "abcdef0123".into()]
    )
    .is_err());
    assert!(prepare(
        &root,
        "2026-11-03",
        &[first[..4].to_string(), second.clone()]
    )
    .is_err());

    let prepared =
        prepare(&root, "2026-11-03", &[first[..10].to_string(), second])?.ok_or("no release")?;
    assert!(prepared.body.contains("left out on purpose"));
    assert!(prepared.body.contains("Replay the documented commands"));
    assert!(prepared.body.contains("unclosed block"));
    Ok(())
}

#[test]
fn writing_is_refused_on_main_and_in_a_dirty_tree() -> Result<()> {
    let root = repository("0.1.1", "0.1.1")?;
    empty_commit(&root, "feat: drag (#5)")?;
    let prepared = prepare(&root, "2026-11-03", &[])?.ok_or("no release")?;
    let error = write(&root, &prepared).err().ok_or("wrote on main")?;
    assert!(error.to_string().contains("working branch"), "{error}");
    assert!(version_locations::check(&root).is_ok());

    git(&root, &["checkout", "-q", "-b", "work"])?;
    write_file(
        &root,
        "Cargo.toml",
        &format!(
            "{}\n# local edit\n",
            fs::read_to_string(root.join("Cargo.toml"))?
        ),
    )?;
    let error = write(&root, &prepared)
        .err()
        .ok_or("wrote into a dirty tree")?;
    assert!(error.to_string().contains("uncommitted"), "{error}");
    Ok(())
}

#[test]
fn a_failure_while_writing_leaves_no_partial_tree() -> Result<()> {
    let root = at_first_release()?;
    empty_commit(&root, "feat: drag (#5)")?;
    let prepared = prepare(&root, "2026-11-03", &[])?.ok_or("no release")?;
    // The last file cannot be staged: its temporary name is a directory.
    fs::create_dir(root.join(".CHANGELOG.md.release-tmp"))?;
    assert!(write(&root, &prepared).is_err());
    for (path, _) in &prepared.files {
        if path != "CHANGELOG.md" {
            let temporary = root.join(path).with_file_name(format!(
                ".{}.release-tmp",
                root.join(path)
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("")
            ));
            assert!(!temporary.exists(), "{path} left a temporary file");
        }
    }
    assert_eq!(version_locations::check(&root)?, "0.1.1");
    assert!(!root.join("CHANGELOG.md").exists());
    Ok(())
}

#[test]
fn nothing_is_prepared_without_a_user_visible_change() -> Result<()> {
    let root = at_first_release()?;
    empty_commit(&root, "ci: tweak")?;
    assert!(prepare(&root, "2026-11-03", &[])?.is_none());
    Ok(())
}
