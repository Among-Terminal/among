//! The decision to tag a release commit carries the repository token's
//! authority, so each branch of it is pinned here against synthetic
//! repositories. Merging a release commit only validates it (wrong title or
//! version, missing or empty highlights, an existing tag at another commit and
//! a version that is not above the highest stable tag are refused, and no tag
//! is ever created by a push). A candidate is tagged on the release commit by a
//! manual decision, and the stable tag only on a commit that already carries a
//! candidate of the same version.

mod release_fixture;

use among_tools::release_tag::{decide_candidate, decide_push, decide_stable, Action, Decision};
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

fn refusal(result: Result<Decision>, why: &str) -> Result<String> {
    Ok(result.err().ok_or(why)?.to_string())
}

fn summary(decision: &Decision) -> (Action, &str, &str) {
    (
        decision.action,
        decision.tag.as_str(),
        decision.sha.as_str(),
    )
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
fn a_valid_release_commit_is_validated_and_never_tagged() -> Result<()> {
    let root = repository("0.1.1", "0.1.1")?;
    for subject in ["chore(release): 0.2.0 (#9)", "chore(release): 0.2.0"] {
        let sha = release_commit(&root, "0.2.0", subject)?;
        let decision = decide_push(&root, &sha)?;
        assert_eq!(
            summary(&decision),
            (Action::Validated, "v0.2.0", sha.as_str())
        );
        assert_eq!(
            decision.render(),
            format!("action=validated\ntag=v0.2.0\nsha={sha}\n")
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
    let error = refusal(decide_push(&root, &sha), "validated a mismatch")?;
    assert!(
        error.contains("title says 0.3.0 but Cargo.toml says 0.2.0"),
        "{error}"
    );
    Ok(())
}

#[test]
fn highlights_must_exist_and_not_be_empty_on_a_pushed_release_commit() -> Result<()> {
    let root = repository("0.1.1", "0.1.1")?;
    write_version_files(&root, "0.2.0")?;
    let sha = commit_all(&root, "chore(release): 0.2.0 (#9)")?;
    let error = refusal(decide_push(&root, &sha), "validated without highlights")?;
    assert!(error.contains("release-notes/v0.2.0.md"), "{error}");
    write(&root, ".github/release-notes/v0.2.0.md", " \n\n")?;
    let sha = commit_all(&root, "chore(release): 0.2.0 (#10)")?;
    let error = refusal(decide_push(&root, &sha), "validated empty highlights")?;
    assert!(error.contains("is empty"), "{error}");
    Ok(())
}

#[test]
fn a_pushed_stable_tag_is_only_accepted_at_the_same_commit() -> Result<()> {
    let root = repository("0.1.1", "0.1.1")?;
    let sha = release_commit(&root, "0.2.0", "chore(release): 0.2.0 (#9)")?;
    git(&root, &["tag", "v0.2.0"])?;
    assert_eq!(decide_push(&root, &sha)?.action, Action::Validated);

    // The same version released again at another commit must not move the tag.
    let other = release_commit(&root, "0.2.0", "chore(release): 0.2.0 (#11)")?;
    let error = refusal(decide_push(&root, &other), "tag accepted at another commit")?;
    assert!(error.contains("already exists"), "{error}");
    Ok(())
}

#[test]
fn a_pushed_version_not_above_the_highest_stable_tag_is_refused() -> Result<()> {
    let root = repository("0.1.1", "0.1.1")?;
    git(&root, &["tag", "v0.3.0"])?;
    git(&root, &["tag", "v0.9.0-rc.1"])?;
    let sha = release_commit(&root, "0.2.0", "chore(release): 0.2.0 (#9)")?;
    let error = refusal(decide_push(&root, &sha), "validated a lower version")?;
    assert!(
        error.contains("not above the highest stable tag 0.3.0"),
        "{error}"
    );

    // A candidate tag above the version does not count as a stable tag.
    let root = repository("0.1.1", "0.1.1")?;
    git(&root, &["tag", "v0.2.0-rc.1"])?;
    let sha = release_commit(&root, "0.2.0", "chore(release): 0.2.0 (#9)")?;
    assert_eq!(decide_push(&root, &sha)?.action, Action::Validated);
    Ok(())
}

#[test]
fn a_candidate_is_tagged_on_the_release_commit_without_a_stable_tag() -> Result<()> {
    let root = repository("0.1.1", "0.1.1")?;
    let release = release_commit(&root, "0.2.0", "chore(release): 0.2.0 (#9)")?;
    let decision = decide_candidate(&root, "1", None)?;
    assert_eq!(
        summary(&decision),
        (Action::Create, "v0.2.0-rc.1", release.as_str())
    );
    assert_eq!(
        decision.render(),
        format!("action=create\ntag=v0.2.0-rc.1\nsha={release}\n")
    );
    // A candidate is not tied to the earlier tag: no v0.2.0 exists.
    assert!(git(&root, &["tag", "--list", "v0.2.0"])?.is_empty());

    git(&root, &["tag", "v0.2.0-rc.1"])?;
    assert_eq!(decide_candidate(&root, "1", None)?.action, Action::Exists);
    // The same number may not move to another commit.
    let later = release_commit(&root, "0.2.0", "chore(release): 0.2.0 (#10)")?;
    let error = refusal(decide_candidate(&root, "1", None), "rc moved")?;
    assert!(error.contains("already exists"), "{error}");
    // The next number goes on the new release commit of the same version.
    assert_eq!(
        summary(&decide_candidate(&root, "2", None)?),
        (Action::Create, "v0.2.0-rc.2", later.as_str())
    );

    for bad in ["", "0", "01", "-1", "1a", "1.0", " 1"] {
        assert!(decide_candidate(&root, bad, None).is_err(), "'{bad}'");
    }
    Ok(())
}

#[test]
fn a_candidate_names_an_older_release_commit_through_the_input() -> Result<()> {
    let root = repository("0.1.1", "0.1.1")?;
    let release = release_commit(&root, "0.2.0", "chore(release): 0.2.0 (#9)")?;
    empty_commit(&root, "fix: later (#10)")?;

    let error = refusal(
        decide_candidate(&root, "1", None),
        "tagged a commit that is not a release commit",
    )?;
    assert!(error.contains("is not a release commit"), "{error}");

    let decision = decide_candidate(&root, "1", Some(&release))?;
    assert_eq!(
        summary(&decision),
        (Action::Create, "v0.2.0-rc.1", release.as_str())
    );
    let short = decide_candidate(&root, "1", Some(&release[..7]))?;
    assert_eq!(short.sha, release);
    // An empty input means "not given".
    assert!(decide_candidate(&root, "1", Some("")).is_err());

    let ordinary = empty_commit(&root, "fix: another (#11)")?;
    let error = refusal(
        decide_candidate(&root, "1", Some(&ordinary)),
        "named a commit that is not a release commit",
    )?;
    assert!(error.contains("is not a release commit"), "{error}");

    for bad in [
        "HEAD",
        "--all",
        "main",
        "ABCDEF1",
        "abc",
        &format!("{release}0"),
    ] {
        let error = refusal(
            decide_candidate(&root, "1", Some(bad)),
            "accepted a revision",
        )?;
        assert!(error.contains("hexadecimal"), "'{bad}': {error}");
    }
    Ok(())
}

#[test]
fn a_candidate_whose_release_commit_is_not_in_main_is_refused() -> Result<()> {
    let root = repository("0.1.1", "0.1.1")?;
    git(&root, &["checkout", "-q", "-b", "side"])?;
    let foreign = release_commit(&root, "0.2.0", "chore(release): 0.2.0 (#9)")?;
    git(&root, &["checkout", "-q", "main"])?;
    empty_commit(&root, "fix: other (#3)")?;
    let error = refusal(
        decide_candidate(&root, "1", Some(&foreign)),
        "tagged a foreign commit",
    )?;
    assert!(error.contains("not contained in main"), "{error}");
    Ok(())
}

#[test]
fn a_candidate_repeats_every_check_of_a_release_commit() -> Result<()> {
    let root = repository("0.1.1", "0.1.1")?;
    write_version_files(&root, "0.2.0")?;
    commit_all(&root, "chore(release): 0.2.0 (#9)")?;
    let error = refusal(
        decide_candidate(&root, "1", None),
        "candidate without highlights",
    )?;
    assert!(error.contains("release-notes/v0.2.0.md"), "{error}");
    write(&root, ".github/release-notes/v0.2.0.md", "\n")?;
    commit_all(&root, "chore(release): 0.2.0 (#10)")?;
    let error = refusal(decide_candidate(&root, "1", None), "empty highlights")?;
    assert!(error.contains("is empty"), "{error}");
    write(&root, ".github/release-notes/v0.3.0.md", "Highlights.\n")?;
    commit_all(&root, "chore(release): 0.3.0 (#11)")?;
    let error = refusal(decide_candidate(&root, "1", None), "title mismatch")?;
    assert!(
        error.contains("title says 0.3.0 but Cargo.toml says 0.2.0"),
        "{error}"
    );
    Ok(())
}

#[test]
fn no_candidate_is_made_once_the_stable_release_exists_or_is_surpassed() -> Result<()> {
    let root = repository("0.1.1", "0.1.1")?;
    let release = release_commit(&root, "0.2.0", "chore(release): 0.2.0 (#9)")?;
    git(&root, &["tag", "v0.2.0", &release])?;
    let error = refusal(decide_candidate(&root, "1", None), "candidate after stable")?;
    assert!(
        error.contains("only made before the stable release"),
        "{error}"
    );

    let root = repository("0.1.1", "0.1.1")?;
    git(&root, &["tag", "v0.3.0"])?;
    release_commit(&root, "0.2.0", "chore(release): 0.2.0 (#9)")?;
    let error = refusal(decide_candidate(&root, "1", None), "candidate below stable")?;
    assert!(
        error.contains("not above the highest stable tag 0.3.0"),
        "{error}"
    );
    Ok(())
}

#[test]
fn the_stable_tag_needs_a_candidate_on_the_same_commit() -> Result<()> {
    let root = repository("0.1.1", "0.1.1")?;
    release_commit(&root, "0.2.0", "chore(release): 0.2.0 (#9)")?;
    let error = refusal(decide_stable(&root, None), "stable without a candidate")?;
    assert!(
        error.contains("no release candidate v0.2.0-rc.N"),
        "{error}"
    );

    // Tags that look similar but are not candidates of this commit and version.
    git(&root, &["tag", "v0.2.0-rc.0"])?;
    git(&root, &["tag", "v0.2.0-rc.01"])?;
    git(&root, &["tag", "v0.2.0-rc.x"])?;
    git(&root, &["tag", "v0.3.0-rc.1"])?;
    assert!(decide_stable(&root, None).is_err());

    // A candidate on an earlier commit does not accept a later commit.
    git(&root, &["tag", "v0.2.0-rc.1"])?;
    let later = release_commit(&root, "0.2.0", "chore(release): 0.2.0 (#10)")?;
    assert!(decide_stable(&root, None).is_err());

    git(&root, &["tag", "v0.2.0-rc.2", &later])?;
    let decision = decide_stable(&root, None)?;
    assert_eq!(
        summary(&decision),
        (Action::Create, "v0.2.0", later.as_str())
    );
    assert_eq!(
        decision.render(),
        format!("action=create\ntag=v0.2.0\nsha={later}\n")
    );
    Ok(())
}

#[test]
fn the_stable_tag_is_idempotent_and_never_moves() -> Result<()> {
    let root = repository("0.1.1", "0.1.1")?;
    let release = release_commit(&root, "0.2.0", "chore(release): 0.2.0 (#9)")?;
    git(&root, &["tag", "v0.2.0-rc.1"])?;
    git(&root, &["tag", "v0.2.0"])?;
    assert_eq!(
        summary(&decide_stable(&root, None)?),
        (Action::Exists, "v0.2.0", release.as_str())
    );

    let later = release_commit(&root, "0.2.0", "chore(release): 0.2.0 (#10)")?;
    git(&root, &["tag", "v0.2.0-rc.2", &later])?;
    let error = refusal(decide_stable(&root, None), "stable moved")?;
    assert!(error.contains("already exists"), "{error}");
    Ok(())
}

#[test]
fn the_stable_tag_names_an_older_release_commit_and_checks_the_version_order() -> Result<()> {
    let root = repository("0.1.1", "0.1.1")?;
    let release = release_commit(&root, "0.2.0", "chore(release): 0.2.0 (#9)")?;
    git(&root, &["tag", "v0.2.0-rc.1"])?;
    empty_commit(&root, "fix: later (#10)")?;
    let error = refusal(decide_stable(&root, None), "stable on a plain commit")?;
    assert!(error.contains("is not a release commit"), "{error}");
    assert_eq!(
        summary(&decide_stable(&root, Some(&release))?),
        (Action::Create, "v0.2.0", release.as_str())
    );
    assert!(decide_stable(&root, Some("--all")).is_err());

    // A higher stable tag exists: the older release can no longer be made.
    git(&root, &["tag", "v0.3.0"])?;
    let error = refusal(decide_stable(&root, Some(&release)), "stable below stable")?;
    assert!(
        error.contains("not above the highest stable tag 0.3.0"),
        "{error}"
    );
    Ok(())
}

#[test]
fn the_stable_tag_repeats_every_check_of_a_release_commit() -> Result<()> {
    let root = repository("0.1.1", "0.1.1")?;
    write_version_files(&root, "0.2.0")?;
    commit_all(&root, "chore(release): 0.2.0 (#9)")?;
    git(&root, &["tag", "v0.2.0-rc.1"])?;
    let error = refusal(decide_stable(&root, None), "stable without highlights")?;
    assert!(error.contains("release-notes/v0.2.0.md"), "{error}");

    let root = repository("0.1.1", "0.1.1")?;
    git(&root, &["checkout", "-q", "-b", "side"])?;
    let foreign = release_commit(&root, "0.2.0", "chore(release): 0.2.0 (#9)")?;
    git(&root, &["tag", "v0.2.0-rc.1", &foreign])?;
    git(&root, &["checkout", "-q", "main"])?;
    empty_commit(&root, "fix: other (#3)")?;
    let error = refusal(
        decide_stable(&root, Some(&foreign)),
        "stable on a foreign commit",
    )?;
    assert!(error.contains("not contained in main"), "{error}");
    Ok(())
}
