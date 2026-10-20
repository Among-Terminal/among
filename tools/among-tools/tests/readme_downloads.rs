//! release assets stay complete, but the README offers only the ten primary
//! packages. Count links as well as names: a set hides duplicate presentations.

use among_tools::{release_assets, Result};
use std::{collections::BTreeSet, fs, path::Path};

const LATEST: &str = "https://github.com/Among-Terminal/among/releases/latest/download/";
const HEADER: &str = "| x86-64 (`amd64`) | ARM 64-bit (`arm64`) |";
const ROWS: [(&str, &str, &str, &str); 5] = [
    (
        "Ubuntu 24.04 DEB",
        "ubuntu24.04",
        "deb",
        "Ubuntu_24.04-DEB_{arch}-C7461A?style=for-the-badge&logo=ubuntu&logoColor=white",
    ),
    (
        "Debian 13 DEB",
        "debian13",
        "deb",
        "Debian_13-DEB_{arch}-A81D33?style=for-the-badge&logo=debian&logoColor=white",
    ),
    (
        "Fedora 44 RPM",
        "fedora44",
        "rpm",
        "Fedora_44-RPM_{arch}-294172?style=for-the-badge&logo=fedora&logoColor=white",
    ),
    (
        "Fedora 43 RPM",
        "fedora43",
        "rpm",
        "Fedora_43-RPM_{arch}-294172?style=for-the-badge&logo=fedora&logoColor=white",
    ),
    (
        "Linux AppImage",
        "",
        "AppImage",
        "Linux-AppImage_{arch}-333333?style=for-the-badge&logo=linux&logoColor=white",
    ),
];

fn linked(text: &str) -> Vec<String> {
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

// archives and source materials remain discoverable through the guide/release.
fn expected() -> BTreeSet<String> {
    release_assets::attached_names()
        .into_iter()
        .filter(|name| {
            name.ends_with(".deb")
                || (name.ends_with(".rpm") && !name.ends_with(".src.rpm"))
                || name.ends_with(".AppImage")
        })
        .chain(["SHA256SUMS".to_string()])
        .collect()
}

fn button(row: usize, arch: &str) -> String {
    let (label, target, extension, badge) = ROWS[row];
    let target = if target.is_empty() {
        String::new()
    } else {
        format!("{target}-")
    };
    let badge = badge.replace("{arch}", arch);
    format!(
        "[![Download {label} for {arch}](https://img.shields.io/badge/{badge})]({LATEST}among-terminal-{target}{arch}.{extension})"
    )
}

fn validate_links(readme: &str) -> std::result::Result<(), String> {
    let mut links = linked(readme);
    links.sort();
    let expected = expected().into_iter().collect::<Vec<_>>();
    if links != expected || links.len() != 11 {
        return Err(format!("unexpected or duplicate download links: {links:?}"));
    }
    Ok(())
}

fn validate_grid(readme: &str) -> std::result::Result<(), String> {
    validate_links(readme)?;
    let installation = readme
        .split_once("## Installation\n")
        .ok_or("missing Installation section")?
        .1
        .split("\n## ")
        .next()
        .ok_or("missing installation contents")?;
    let table = installation
        .lines()
        .filter(|line| line.starts_with('|'))
        .collect::<Vec<_>>();
    if table.len() != 7 || table[0] != HEADER || table[1] != "|---|---|" {
        return Err("expected exactly one five-row, two-column download grid".into());
    }
    for (row, line) in table[2..].iter().enumerate() {
        let expected_row = format!("| {} | {} |", button(row, "amd64"), button(row, "arm64"));
        if **line != expected_row {
            return Err(format!("wrong labels, badge, target or order in row {row}"));
        }
    }
    if installation.matches("](https://img.shields.io/").count() != 10
        || !installation.contains("AppImages require glibc 2.39 or newer.")
        || !installation.contains(&format!("[SHA256SUMS]({LATEST}SHA256SUMS)"))
    {
        return Err("expected ten badges, a plain checksum link and the glibc note".into());
    }
    Ok(())
}

#[test]
fn readme_links_exactly_ten_primary_packages_and_one_plain_checksum() -> Result<()> {
    let readme = fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../README.md"))?;
    assert_eq!(validate_grid(&readme), Ok(()));
    Ok(())
}

#[test]
fn readme_status_badges_describe_this_repository_and_its_main_ci() -> Result<()> {
    let readme = fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../README.md"))?;
    for badge in [
        "[![CI](https://img.shields.io/github/actions/workflow/status/Among-Terminal/among/checks.yml?branch=main&label=CI)](https://github.com/Among-Terminal/among/actions/workflows/checks.yml?query=branch%3Amain)",
        "[![Latest release](https://img.shields.io/github/v/release/Among-Terminal/among)](https://github.com/Among-Terminal/among/releases/latest)",
        "[![Downloads](https://img.shields.io/github/downloads/Among-Terminal/among/total?label=downloads)](https://github.com/Among-Terminal/among/releases)",
        "[![License: Apache-2.0](https://img.shields.io/badge/license-Apache--2.0-blue)](LICENSE)",
    ] {
        assert!(readme.contains(badge), "missing or misdirected badge: {badge}");
    }
    Ok(())
}

#[test]
fn missing_renamed_and_duplicate_downloads_are_rejected() -> Result<()> {
    let readme = fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../README.md"))?;
    for changed in [
        readme.replace(&button(0, "amd64"), ""),
        readme.replace("among-terminal-amd64.AppImage", "among-amd64.AppImage"),
        format!("{readme}\n{}", button(0, "amd64")),
        format!("{readme}\n[duplicate]({LATEST}among-terminal-debian13-arm64.deb)"),
    ] {
        assert!(validate_links(&changed).is_err());
    }
    Ok(())
}

#[test]
fn wrong_architectures_versions_order_and_extra_badges_are_rejected() -> Result<()> {
    let readme = fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../README.md"))?;
    let first_row = format!("| {} | {} |", button(0, "amd64"), button(0, "arm64"));
    let reversed_row = format!("| {} | {} |", button(0, "arm64"), button(0, "amd64"));
    for changed in [
        readme.replace(
            "Download Ubuntu 24.04 DEB for arm64",
            "Download Ubuntu 24.04 DEB for amd64",
        ),
        readme.replace("Ubuntu_24.04-DEB", "Ubuntu-DEB"),
        readme.replace(&first_row, &reversed_row),
        readme.replace("|---|---|", "|---|---|---|"),
        readme.replace("AppImages require glibc 2.39 or newer.", ""),
        readme.replace(
            "[SHA256SUMS]",
            "[![SHA256SUMS](https://img.shields.io/badge/checksums-blue)]",
        ),
        readme.replace(HEADER, &format!("{HEADER}\n{first_row}")),
    ] {
        assert!(validate_grid(&changed).is_err());
    }
    for name in [
        "among-terminal-debian13-amd64.tar.gz",
        "among-terminal-fedora44.src.rpm",
        "among-terminal-amd64-appimage-sources.tar",
        "SHA256SUMS",
    ] {
        let changed = format!(
            "{readme}\n[![Extra](https://img.shields.io/badge/extra-blue)]({LATEST}{name})"
        );
        assert!(validate_grid(&changed).is_err(), "accepted extra {name}");
    }
    Ok(())
}
