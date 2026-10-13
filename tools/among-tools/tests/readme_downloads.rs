//! The README's download links name files of the latest stable release
//! directly. A link to a name the release does not attach is a 404 for every
//! reader, and a file without a link is a format nobody finds, so the links
//! must stay exactly the attached installable files plus `SHA256SUMS`.

use among_tools::{release_assets, Result};
use std::{collections::BTreeSet, fs, path::Path};

const LATEST: &str = "https://github.com/Among-Terminal/among/releases/latest/download/";

fn linked(text: &str) -> BTreeSet<String> {
    text.split(LATEST)
        .skip(1)
        .map(|rest| {
            rest.split(|character: char| character == ')' || character.is_whitespace())
                .next()
                .unwrap_or_default()
                .to_string()
        })
        .collect()
}

/// Source packages and library sources are reached from the release itself;
/// the README offers what a user installs.
fn expected() -> BTreeSet<String> {
    release_assets::attached_names()
        .into_iter()
        .filter(|name| !name.ends_with(".src.rpm") && !name.ends_with("-appimage-sources.tar"))
        .chain(["SHA256SUMS".to_string()])
        .collect()
}

#[test]
fn readme_links_exactly_the_installable_files_of_the_latest_release() -> Result<()> {
    let readme = fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../README.md"))?;
    assert_eq!(linked(&readme), expected());
    Ok(())
}

#[test]
fn a_removed_or_renamed_link_is_noticed() {
    let complete = expected()
        .iter()
        .map(|name| format!("[{name}]({LATEST}{name})\n"))
        .collect::<String>();
    assert_eq!(linked(&complete), expected());
    let without_one = complete.replace(&format!("({LATEST}among-terminal-fedora43-arm64.rpm)"), "");
    assert_ne!(linked(&without_one), expected());
    let renamed = complete.replace("among-terminal-amd64.AppImage", "among-amd64.AppImage");
    assert_ne!(linked(&renamed), expected());
}
