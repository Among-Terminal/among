//! Every place that spells the application version must be read, must agree
//! with the others, and must fail the check when it does not. Each test starts
//! from a copy of the real files and breaks exactly one thing.

use among_tools::{storage, version_locations, Result};
use std::{
    fs,
    path::{Path, PathBuf},
};

const FILES: [&str; 7] = [
    "Cargo.toml",
    "Cargo.lock",
    "examples/plugins/Cargo.toml",
    "examples/plugins/Cargo.lock",
    "packaging/rpm/among.spec",
    "data/com.github.among-terminal.metainfo.xml",
    "packaging/debian/changelog",
];

fn repository() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// A private copy of the files the check reads.
fn copy() -> Result<PathBuf> {
    let root = storage::private_directory(&std::env::temp_dir(), "among-version-locations")?;
    for file in FILES {
        let target = root.join(file);
        fs::create_dir_all(target.parent().ok_or("no parent")?)?;
        fs::copy(repository().join(file), target)?;
    }
    Ok(root)
}

fn edit(root: &Path, file: &str, from: &str, to: &str) -> Result<()> {
    let path = root.join(file);
    let text = fs::read_to_string(&path)?;
    assert!(text.contains(from), "{file} does not contain {from:?}");
    fs::write(path, text.replacen(from, to, 1))?;
    Ok(())
}

fn current() -> Result<String> {
    version_locations::check(&repository())
}

#[test]
fn the_real_repository_agrees_on_one_version() -> Result<()> {
    let version = current()?;
    let found = version_locations::read_all(&repository())?;
    assert_eq!(found.len(), version_locations::LOCATIONS.len());
    assert!(found.iter().all(|(_, other)| *other == version));
    Ok(())
}

#[test]
fn a_location_with_another_version_fails_the_check() -> Result<()> {
    let version = current()?;
    let other = "9.9.9";
    let mutations: [(&str, String, String); 6] = [
        (
            "Cargo.toml",
            format!("version = \"{version}\""),
            format!("version = \"{other}\""),
        ),
        (
            "Cargo.lock",
            format!("name = \"among\"\nversion = \"{version}\""),
            format!("name = \"among\"\nversion = \"{other}\""),
        ),
        (
            "examples/plugins/Cargo.toml",
            format!("version = \"{version}\""),
            format!("version = \"{other}\""),
        ),
        (
            "examples/plugins/Cargo.lock",
            format!("name = \"among-example-plugins\"\nversion = \"{version}\""),
            format!("name = \"among-example-plugins\"\nversion = \"{other}\""),
        ),
        (
            "packaging/rpm/among.spec",
            format!("Version:        {version}"),
            format!("Version:        {other}"),
        ),
        (
            "data/com.github.among-terminal.metainfo.xml",
            format!("<release version=\"{version}\""),
            format!("<release version=\"{other}\""),
        ),
    ];
    for (file, from, to) in mutations {
        let root = copy()?;
        edit(&root, file, &from, &to)?;
        let error = version_locations::check(&root)
            .err()
            .ok_or_else(|| format!("changing {file} did not fail the check"))?;
        assert!(
            error.to_string().contains(file),
            "the error for {file} does not name it: {error}"
        );
    }
    Ok(())
}

#[test]
fn a_missing_location_fails_the_check() -> Result<()> {
    for location in &version_locations::LOCATIONS {
        let root = copy()?;
        fs::remove_file(root.join(location.path))?;
        let error = version_locations::check(&root)
            .err()
            .ok_or_else(|| format!("removing {} did not fail the check", location.path))?;
        assert!(error.to_string().contains(location.path), "{error}");
    }
    Ok(())
}

#[test]
fn a_changelog_ahead_of_the_application_fails_the_check() -> Result<()> {
    let root = copy()?;
    let changelog = root.join("packaging/debian/changelog");
    let text = fs::read_to_string(&changelog)?;
    fs::write(
        &changelog,
        format!("among-terminal (9.0.0-1) unstable; urgency=medium\n\n{text}"),
    )?;
    assert!(version_locations::check(&root).is_err());

    let root = copy()?;
    edit(
        &root,
        "packaging/rpm/among.spec",
        "%changelog\n",
        "%changelog\n* Mon Nov 02 2026 Someone <a@example.invalid> - 9.0.0-1\n- ahead\n\n",
    )?;
    assert!(version_locations::check(&root).is_err());
    Ok(())
}

#[test]
fn rewriting_changes_every_location_and_nothing_else() -> Result<()> {
    let root = copy()?;
    let old = current()?;
    let rewritten = version_locations::rewrite_all(&root, "9.9.9")?;
    for (path, text) in &rewritten {
        let original = fs::read_to_string(root.join(path))?;
        if *path == version_locations::METAINFO {
            assert_eq!(
                text, &original,
                "the metainfo entry is added by the release plan"
            );
            continue;
        }
        assert_ne!(text, &original, "{path} was not rewritten");
        // Only the version differs: undoing it gives the original bytes back.
        assert_eq!(text.replacen("9.9.9", &old, 1), original, "{path}");
        assert_eq!(text.matches("9.9.9").count(), 1, "{path}");
    }
    for (path, text) in rewritten {
        fs::write(root.join(path), text)?;
    }
    edit(
        &root,
        "data/com.github.among-terminal.metainfo.xml",
        &format!("<release version=\"{old}\""),
        "<release version=\"9.9.9\"",
    )?;
    assert_eq!(version_locations::check(&root)?, "9.9.9");
    Ok(())
}
