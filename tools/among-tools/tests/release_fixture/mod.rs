//! Synthetic repositories for the release tooling tests. They never copy the
//! live version files, so they keep working after a release changes those.

#![allow(dead_code)]

use among_tools::{storage, Result};
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

pub fn git(root: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new("/usr/bin/git")
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
        .output()?;
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(String::from_utf8(output.stdout)?.trim().to_string())
}

pub fn write(root: &Path, relative: &str, text: &str) -> Result<()> {
    let path = root.join(relative);
    fs::create_dir_all(path.parent().ok_or("no parent")?)?;
    fs::write(path, text)?;
    Ok(())
}

/// Every version location at `version`.
pub fn write_version_files(root: &Path, version: &str) -> Result<()> {
    write(
        root,
        "Cargo.toml",
        &format!("[package]\nname = \"among\"\nversion = \"{version}\"\nedition = \"2021\"\n"),
    )?;
    write(
        root,
        "Cargo.lock",
        &format!("version = 4\n\n[[package]]\nname = \"among\"\nversion = \"{version}\"\n"),
    )?;
    write(
        root,
        "packaging/rpm/among.spec",
        &format!(
            "Name:           among\nVersion:        {version}\nRelease:        1%{{?dist}}\n\n%changelog\n\
* Mon Oct 19 2026 A <a@example.invalid> - 0.0.1-1\n- old\n"
        ),
    )?;
    write(
        root,
        "data/com.github.among-terminal.metainfo.xml",
        &format!(
            "<component>\n  <releases>\n    <release version=\"{version}\" date=\"2026-01-01\" type=\"stable\">\n\
      <description><p>old</p></description>\n    </release>\n  </releases>\n</component>\n"
        ),
    )?;
    write(
        root,
        "packaging/debian/changelog",
        "among-terminal (0.0.1-1) unstable; urgency=medium\n\n  * old\n\n -- A <a@example.invalid>  Mon, 19 Oct 2026 10:00:00 +0000\n",
    )
}

/// A repository on `main` with the version files at `version`, tagged
/// `v<tagged>` on its first commit.
pub fn repository(version: &str, tagged: &str) -> Result<PathBuf> {
    let root = storage::private_directory(&std::env::temp_dir(), "among-release-fixture")?;
    git(&root, &["init", "-q", "-b", "main"])?;
    write_version_files(&root, version)?;
    git(&root, &["add", "."])?;
    git(&root, &["commit", "-q", "-m", "chore: start"])?;
    git(&root, &["tag", &format!("v{tagged}")])?;
    Ok(root)
}

pub fn empty_commit(root: &Path, subject: &str) -> Result<String> {
    git(root, &["commit", "-q", "--allow-empty", "-m", subject])?;
    git(root, &["rev-parse", "HEAD"])
}

/// Commits everything that changed.
pub fn commit_all(root: &Path, subject: &str) -> Result<String> {
    git(root, &["add", "."])?;
    git(root, &["commit", "-q", "--allow-empty", "-m", subject])?;
    git(root, &["rev-parse", "HEAD"])
}
