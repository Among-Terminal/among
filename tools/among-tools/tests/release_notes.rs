//! The release description is what a user reads to pick a file. These tests
//! hold it to the staged files: every one linked once at this tag's download
//! URL, no link to a file the release lacks, and no stable release without
//! written highlights.

use among_tools::{release_assets, release_notes, storage, Result};
use std::{fs, path::PathBuf};

const REPOSITORY: &str = "Among-Terminal/among";
const RUN: &str = "https://github.com/Among-Terminal/among/actions/runs/1";

fn commit() -> String {
    "c".repeat(40)
}

/// A staged directory as `stage-release-assets` leaves it; the description
/// reads only `SHA256SUMS`.
fn staged(names: &[String]) -> Result<(PathBuf, PathBuf)> {
    let root = storage::private_directory(&std::env::temp_dir(), "among-release-notes")?;
    let assets = root.join("assets");
    fs::create_dir(&assets)?;
    let sums = names
        .iter()
        .map(|name| format!("{}  {name}\n", "a".repeat(64)))
        .collect::<String>();
    fs::write(assets.join("SHA256SUMS"), sums)?;
    let highlights = root.join("highlights");
    fs::create_dir(&highlights)?;
    Ok((root, highlights))
}

fn all() -> Vec<String> {
    release_assets::attached_names().into_iter().collect()
}

#[test]
fn a_release_candidate_without_highlights_links_every_file_once_at_its_tag() -> Result<()> {
    let (root, highlights) = staged(&all())?;
    let tag = "v0.1.0-rc.13";
    let body = release_notes::render(
        &root.join("assets"),
        REPOSITORY,
        tag,
        &commit(),
        RUN,
        &highlights,
    )?;
    assert!(body.starts_with("## Download\n"), "{body}");
    for name in all() {
        let url =
            format!("(https://github.com/Among-Terminal/among/releases/download/{tag}/{name})");
        assert_eq!(body.matches(&url).count(), 1, "{name}");
    }
    assert_eq!(body.matches("/releases/download/").count(), all().len());
    assert!(!body.contains("/releases/latest"));
    assert!(body.contains(&format!("commit/{}", commit())));
    assert!(body.contains(RUN));
    assert!(body.contains("sha256sum --check --ignore-missing SHA256SUMS"));
    assert!(body.contains("under X11 (Xvfb with Openbox)"));
    assert!(body.contains("GNOME and Wayland sessions\nare not part of these checks"));
    fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn a_stable_tag_needs_written_highlights_and_starts_with_them() -> Result<()> {
    let (root, highlights) = staged(&all())?;
    let assets = root.join("assets");
    let render =
        |tag: &str| release_notes::render(&assets, REPOSITORY, tag, &commit(), RUN, &highlights);
    assert!(render("v0.1.0").is_err(), "stable tag without highlights");
    fs::write(highlights.join("v0.1.0.md"), "  \n")?;
    assert!(
        render("v0.1.0").is_err(),
        "stable tag with blank highlights"
    );
    fs::write(highlights.join("v0.1.0.md"), "The first stable release.\n")?;
    let body = render("v0.1.0")?;
    assert!(body.starts_with("The first stable release.\n\n## Download\n"));
    assert!(body.contains("/releases/download/v0.1.0/among-terminal-amd64.AppImage)"));
    fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn a_staged_set_other_than_the_attached_names_is_refused() -> Result<()> {
    let mut missing = all();
    missing.retain(|name| name != "among-terminal-fedora43-arm64.rpm");
    let mut extra = all();
    extra.push("among-terminal-fedora42-amd64.rpm".into());
    let mut repeated = all();
    repeated.push(repeated[0].clone());
    for (case, names) in [
        ("missing", missing),
        ("extra", extra),
        ("repeated", repeated),
    ] {
        let (root, highlights) = staged(&names)?;
        assert!(
            release_notes::render(
                &root.join("assets"),
                REPOSITORY,
                "v0.1.0-rc.13",
                &commit(),
                RUN,
                &highlights
            )
            .is_err(),
            "{case}"
        );
        fs::remove_dir_all(root)?;
    }
    Ok(())
}

#[test]
fn highlights_that_link_a_release_file_again_are_refused() -> Result<()> {
    let (root, highlights) = staged(&all())?;
    fs::write(
        highlights.join("v0.1.0.md"),
        "Get [the AppImage](https://github.com/Among-Terminal/among/releases/download/v0.1.0/among-terminal-amd64.AppImage).\n",
    )?;
    assert!(release_notes::render(
        &root.join("assets"),
        REPOSITORY,
        "v0.1.0",
        &commit(),
        RUN,
        &highlights
    )
    .is_err());
    fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn the_output_file_must_be_new() -> Result<()> {
    let (root, highlights) = staged(&all())?;
    let output = root.join("notes.md");
    let write = || {
        release_notes::write(
            &root.join("assets"),
            REPOSITORY,
            "v0.1.0-rc.13",
            &commit(),
            RUN,
            &highlights,
            &output,
        )
    };
    write()?;
    assert!(fs::read_to_string(&output)?.starts_with("## Download"));
    assert!(write().is_err());
    fs::remove_dir_all(root)?;
    Ok(())
}
