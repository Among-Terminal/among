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
    assert!(
        body.starts_with(
            "> Release candidate 13 of v0.1.0, published for testing. It is not the stable release.\n\n## Download\n"
        ),
        "{body}"
    );
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
    // Packages carry no plugins; the notes must point at the built-in source
    // instead of claiming bundled, disabled-by-default plugins.
    assert!(body.contains("No package carries plugins"));
    assert!(body.contains("Official plugins for Among Terminal"));
    assert!(body.contains("Among-Terminal/among-plugins"));
    assert!(!body.contains("Plugins are disabled by default"));
    assert!(!body.contains("focus, status plugins"));
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
    assert!(fs::read_to_string(&output)?.starts_with("> Release candidate 13 of v0.1.0"));
    assert!(write().is_err());
    fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn a_candidate_shows_its_own_highlights_else_those_of_its_version() -> Result<()> {
    let (root, highlights) = staged(&all())?;
    let render = |tag: &str, highlights: &std::path::Path| {
        release_notes::render(
            &root.join("assets"),
            REPOSITORY,
            tag,
            &commit(),
            RUN,
            highlights,
        )
    };
    fs::write(highlights.join("v0.2.0.md"), "Version highlights.\n")?;
    let inherited = render("v0.2.0-rc.2", &highlights)?;
    assert!(
        inherited.contains("Release candidate 2 of v0.2.0, published for testing."),
        "{inherited}"
    );
    assert!(
        inherited.contains("Version highlights.\n\n## Download"),
        "{inherited}"
    );
    fs::write(highlights.join("v0.2.0-rc.2.md"), "Candidate only.\n")?;
    let own = render("v0.2.0-rc.2", &highlights)?;
    assert!(own.contains("Candidate only."), "{own}");
    assert!(!own.contains("Version highlights."), "{own}");
    let other = render("v0.2.0-rc.3", &highlights)?;
    assert!(other.contains("Version highlights."), "{other}");
    assert!(!other.contains("Candidate only."), "{other}");
    fs::remove_file(highlights.join("v0.2.0.md"))?;
    fs::remove_file(highlights.join("v0.2.0-rc.2.md"))?;
    let none = render("v0.2.0-rc.4", &highlights)?;
    assert!(none.contains("## Download"), "{none}");
    assert!(!none.contains("Version highlights."), "{none}");
    fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn a_stable_release_has_no_candidate_notice_and_still_needs_its_own_highlights() -> Result<()> {
    let (root, highlights) = staged(&all())?;
    let render = |tag: &str| {
        release_notes::render(
            &root.join("assets"),
            REPOSITORY,
            tag,
            &commit(),
            RUN,
            &highlights,
        )
    };
    assert!(render("v0.2.0").is_err());
    fs::write(highlights.join("v0.2.0.md"), "Stable highlights.\n")?;
    let body = render("v0.2.0")?;
    assert!(body.starts_with("Stable highlights."), "{body}");
    assert!(!body.contains("Release candidate"), "{body}");
    fs::remove_dir_all(root)?;
    Ok(())
}
