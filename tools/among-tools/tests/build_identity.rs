//! `scripts/build-identity.sh` derives the About-page build variables from the
//! checked-out commit. These tests run the real script against a real
//! repository with a fixed commit time.

use among_tools::{storage, Result};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// 2026-11-05 23:30:00 UTC; in a UTC+3 zone the local day would already be the
/// sixth, which the script must not report.
const COMMIT_TIME: &str = "2026-11-05T23:30:00+00:00";

struct Repository(PathBuf);

impl Repository {
    fn new() -> Result<Self> {
        let base = storage::private_directory(&std::env::temp_dir(), "among-build-identity")?;
        fs::create_dir(base.join("scripts"))?;
        fs::copy(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scripts/build-identity.sh"),
            base.join("scripts/build-identity.sh"),
        )?;
        let repository = Self(base);
        repository.git(&["init", "--quiet"])?;
        repository.git(&["add", "scripts/build-identity.sh"])?;
        repository.git(&[
            "-c",
            "user.name=test",
            "-c",
            "user.email=test@example.invalid",
            "commit",
            "--quiet",
            "-m",
            "fixture",
        ])?;
        Ok(repository)
    }

    fn git(&self, args: &[&str]) -> Result<String> {
        let output = Command::new("git")
            .args(args)
            .current_dir(&self.0)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_DATE", COMMIT_TIME)
            .env("GIT_COMMITTER_DATE", COMMIT_TIME)
            .output()?;
        if !output.status.success() {
            return Err(String::from_utf8_lossy(&output.stderr).into_owned().into());
        }
        Ok(String::from_utf8(output.stdout)?.trim().to_string())
    }

    fn identity(&self, ref_type: &str, ref_name: &str) -> std::io::Result<std::process::Output> {
        Command::new("sh")
            .arg(self.0.join("scripts/build-identity.sh"))
            .args([ref_type, ref_name])
            .env("TZ", "Europe/Moscow")
            .output()
    }
}

impl Drop for Repository {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn a_tag_build_reports_the_tag_the_short_commit_and_the_commit_utc_date() -> Result<()> {
    let repository = Repository::new()?;
    let commit = repository.git(&["rev-parse", "HEAD"])?;
    let output = repository.identity("tag", "v0.2.0-rc.2")?;
    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        String::from_utf8(output.stdout)?,
        format!(
            "AMONG_BUILD_VERSION=0.2.0-rc.2\nAMONG_BUILD_COMMIT={}\nAMONG_BUILD_DATE=2026-11-05\n",
            &commit[..7]
        )
    );
    Ok(())
}

#[test]
fn a_build_that_is_not_a_tag_leaves_the_version_to_the_crate() -> Result<()> {
    let repository = Repository::new()?;
    let output = repository.identity("branch", "feat/anything")?;
    assert!(output.status.success(), "{output:?}");
    let text = String::from_utf8(output.stdout)?;
    assert!(!text.contains("AMONG_BUILD_VERSION"), "{text}");
    assert!(text.contains("AMONG_BUILD_COMMIT="), "{text}");
    assert!(text.contains("AMONG_BUILD_DATE=2026-11-05"), "{text}");
    Ok(())
}

#[test]
fn a_tag_that_is_not_a_release_tag_is_refused() -> Result<()> {
    let repository = Repository::new()?;
    for name in ["0.2.0", "v0.2", "latest", "v0.2.0; rm -rf /", "v1.2.3\nx"] {
        let output = repository.identity("tag", name)?;
        assert!(!output.status.success(), "{name:?} was accepted");
        assert!(output.stdout.is_empty(), "{name:?} printed values");
    }
    Ok(())
}

#[test]
fn a_missing_repository_is_an_error_not_unknown_values() -> Result<()> {
    let repository = Repository::new()?;
    fs::remove_dir_all(repository.0.join(".git"))?;
    let output = repository.identity("tag", "v0.2.0")?;
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    Ok(())
}
