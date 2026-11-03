//! The decision to tag a release commit carries the repository token's
//! authority, so each branch of it is pinned here against synthetic
//! repositories: a wrong title or version, a missing or empty highlights file,
//! an existing tag at the same or another commit, and a version that is not
//! above the highest stable tag.

mod release_fixture;

use among_tools::release_tag::{decide_candidate, decide_push, Action};
use among_tools::Result;
use release_fixture::{commit_all, empty_commit, git, repository, write, write_version_files};
use std::path::Path;

/// Puts a release commit for `version` on top, with highlights.
fn release_commit(root: &Path, version: &str, subject: &str) -> Result<String> {
    write_version_files(root, version)?;
    write(
        root,
        &format!(".github/release-notes/v{version}.md"),
        "Highlights.\n",
    )?;
    commit_all(root, subject)
}

#[test]
fn an_ordinary_commit_is_skipped() -> Result<()> {
    let root = repository("0.1.1", "0.1.1")?;
    let sha = empty_commit(&root, "fix: something (#3)")?;
    let decision = decide_push(&root, &sha)?;
    assert_eq!((decision.action, decision.tag.as_str()), (Action::Skip, ""));
    for subject in [
        "chore(release): 02.0.0",
        "chore(release): 0.2.0 extra",
        "chore(release): 0.2",
        "chore(release): 0.2.0 (#x)",
        "chore: release 0.2.0",
        "fix(release): 0.2.0",
    ] {
        let sha = empty_commit(&root, subject)?;
        assert_eq!(decide_push(&root, &sha)?.action, Action::Skip, "{subject}");
    }
    Ok(())
}

#[test]
fn a_release_commit_with_matching_version_and_highlights_is_tagged() -> Result<()> {
    let root = repository("0.1.1", "0.1.1")?;
    for subject in ["chore(release): 0.2.0 (#9)", "chore(release): 0.2.0"] {
        let sha = release_commit(&root, "0.2.0", subject)?;
        let decision = decide_push(&root, &sha)?;
        assert_eq!(
            (
                decision.action,
                decision.tag.as_str(),
                decision.sha.as_str()
            ),
            (Action::Create, "v0.2.0", sha.as_str())
        );
        assert_eq!(
            decision.render(),
            format!("action=create\ntag=v0.2.0\nsha={sha}\n")
        );
    }
    Ok(())
}

#[test]
fn a_title_that_disagrees_with_cargo_toml_is_an_error() -> Result<()> {
    let root = repository("0.1.1", "0.1.1")?;
    write_version_files(&root, "0.2.0")?;
    write(&root, ".github/release-notes/v0.3.0.md", "Highlights.\n")?;
    let sha = commit_all(&root, "chore(release): 0.3.0 (#9)")?;
    let error = decide_push(&root, &sha).err().ok_or("tagged a mismatch")?;
    assert!(
        error
            .to_string()
            .contains("title says 0.3.0 but Cargo.toml says 0.2.0"),
        "{error}"
    );
    Ok(())
}

#[test]
fn highlights_must_exist_and_not_be_empty_before_the_tag_is_made() -> Result<()> {
    let root = repository("0.1.1", "0.1.1")?;
    write_version_files(&root, "0.2.0")?;
    let sha = commit_all(&root, "chore(release): 0.2.0 (#9)")?;
    let error = decide_push(&root, &sha)
        .err()
        .ok_or("tagged without highlights")?;
    assert!(
        error.to_string().contains("release-notes/v0.2.0.md"),
        "{error}"
    );
    write(&root, ".github/release-notes/v0.2.0.md", " \n\n")?;
    let sha = commit_all(&root, "chore(release): 0.2.0 (#10)")?;
    assert!(decide_push(&root, &sha).is_err());
    Ok(())
}

#[test]
fn an_existing_tag_is_only_accepted_at_the_same_commit() -> Result<()> {
    let root = repository("0.1.1", "0.1.1")?;
    let sha = release_commit(&root, "0.2.0", "chore(release): 0.2.0 (#9)")?;
    git(&root, &["tag", "v0.2.0"])?;
    let decision = decide_push(&root, &sha)?;
    assert_eq!(
        (decision.action, decision.tag.as_str()),
        (Action::Exists, "v0.2.0")
    );

    // The same version released again at another commit must not move the tag.
    let other = release_commit(&root, "0.2.0", "chore(release): 0.2.0 (#11)")?;
    let error = decide_push(&root, &other)
        .err()
        .ok_or("tag accepted at another commit")?;
    assert!(error.to_string().contains("already exists"), "{error}");
    Ok(())
}

#[test]
fn a_version_not_above_the_highest_stable_tag_is_refused() -> Result<()> {
    let root = repository("0.1.1", "0.1.1")?;
    git(&root, &["tag", "v0.3.0"])?;
    git(&root, &["tag", "v0.9.0-rc.1"])?;
    let sha = release_commit(&root, "0.2.0", "chore(release): 0.2.0 (#9)")?;
    let error = decide_push(&root, &sha)
        .err()
        .ok_or("tagged a lower version")?;
    assert!(
        error
            .to_string()
            .contains("not above the highest stable tag 0.3.0"),
        "{error}"
    );

    let root = repository("0.1.1", "0.2.0")?;
    let sha = release_commit(&root, "0.2.0", "chore(release): 0.2.0 (#9)")?;
    // v0.2.0 exists on another commit: refused as an existing tag; a candidate
    // tag above it does not count as a stable tag.
    assert!(decide_push(&root, &sha).is_err());
    let root = repository("0.1.1", "0.1.1")?;
    git(&root, &["tag", "v0.2.0-rc.1"])?;
    let sha = release_commit(&root, "0.2.0", "chore(release): 0.2.0 (#9)")?;
    assert_eq!(decide_push(&root, &sha)?.action, Action::Create);
    Ok(())
}

#[test]
fn a_candidate_tags_the_stable_release_commit_of_main() -> Result<()> {
    let root = repository("0.1.1", "0.1.1")?;
    let release = release_commit(&root, "0.2.0", "chore(release): 0.2.0 (#9)")?;
    let missing = decide_candidate(&root, "1")
        .err()
        .ok_or("no stable tag yet")?;
    assert!(
        missing
            .to_string()
            .contains("merge the release pull request first"),
        "{missing}"
    );

    git(&root, &["tag", "v0.2.0"])?;
    empty_commit(&root, "fix: later (#10)")?;
    let decision = decide_candidate(&root, "1")?;
    assert_eq!(
        (
            decision.action,
            decision.tag.as_str(),
            decision.sha.as_str()
        ),
        (Action::Create, "v0.2.0-rc.1", release.as_str())
    );

    git(&root, &["tag", "v0.2.0-rc.1", "v0.2.0"])?;
    assert_eq!(decide_candidate(&root, "1")?.action, Action::Exists);
    git(&root, &["tag", "-f", "v0.2.0-rc.2", "HEAD"])?;
    let error = decide_candidate(&root, "2").err().ok_or("rc moved")?;
    assert!(error.to_string().contains("already exists"), "{error}");

    for bad in ["", "0", "01", "-1", "1a", "1.0", " 1"] {
        assert!(decide_candidate(&root, bad).is_err(), "'{bad}'");
    }
    Ok(())
}

#[test]
fn a_candidate_whose_stable_commit_is_not_in_main_is_refused() -> Result<()> {
    let root = repository("0.1.1", "0.1.1")?;
    git(&root, &["checkout", "-q", "-b", "side"])?;
    release_commit(&root, "0.2.0", "chore(release): 0.2.0 (#9)")?;
    git(&root, &["tag", "v0.2.0"])?;
    git(&root, &["checkout", "-q", "main"])?;
    // main moved on without the release commit but with the same Cargo version.
    write_version_files(&root, "0.2.0")?;
    commit_all(&root, "feat: other (#3)")?;
    let error = decide_candidate(&root, "1")
        .err()
        .ok_or("tagged a foreign commit")?;
    assert!(
        error.to_string().contains("not contained in main"),
        "{error}"
    );
    Ok(())
}
