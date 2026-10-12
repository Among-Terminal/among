use among_tools::{release_policy, source_snapshot, stable_build, stable_source, storage, Result};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

struct Fixture {
    base: PathBuf,
    record: Value,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.base);
    }
}
impl Fixture {
    fn new() -> Result<Self> {
        let base = storage::private_directory(&std::env::temp_dir(), "among-stable-source")?;
        let mut f = Self {
            base,
            record: json!({}),
        };
        fs::create_dir(f.repo())?;
        for name in source_snapshot::FILES {
            let path = f.repo().join(name);
            fs::create_dir_all(path.parent().ok_or("fixture parent absent")?)?;
            fs::write(path, b"fixture\n")?;
        }
        for name in ["Cargo.toml", "examples/plugins/Cargo.toml"] {
            fs::write(f.repo().join(name), "[package]\nversion=\"0.1.0\"\n")?;
        }
        fs::write(
            f.repo().join("LICENSE"),
            b"\xffbinary license integrity fixture\n\n",
        )?;
        fs::create_dir(f.repo().join("snap"))?;
        fs::write(
            f.repo().join("snap/snapcraft.yaml"),
            "# keep comment\nname: among\nbase: core24\nconfinement: classic\ngrade: devel\n",
        )?;
        fs::write(f.repo().join(".secret"), b"synthetic private marker")?;
        f.git(&["init", "-q", "-b", "main"])?;
        f.git(&["config", "user.name", "Synthetic Fixture"])?;
        f.git(&["config", "user.email", "fixture@example.invalid"])?;
        f.git(&["add", "."])?;
        f.git(&[
            "-c",
            "commit.gpgSign=false",
            "commit",
            "-qm",
            "synthetic source",
        ])?;
        f.git(&["-c", "tag.gpgSign=false", "tag", "v0.1.0"])?;
        f.record =
            json!({"tag":"v0.1.0","version":"0.1.0","source_sha":f.git(&["rev-parse","HEAD"])?});
        stable_build::prepare(&f.repo(), &f.tree(), "v0.1.0")?;
        fs::create_dir_all(f.base.join("vendor/crate"))?;
        fs::write(
            f.base.join("vendor/crate/.cargo-checksum.json"),
            b"{\"files\":{},\"package\":null}",
        )?;
        f.snapshot()?;
        Ok(f)
    }
    fn repo(&self) -> PathBuf {
        self.base.join("repo")
    }
    fn tree(&self) -> PathBuf {
        self.base.join("build")
    }
    fn bundle(&self) -> PathBuf {
        self.base.join("bundle.tar")
    }
    fn git(&self, args: &[&str]) -> Result<String> {
        let output = std::process::Command::new("/usr/bin/git")
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .arg("-C")
            .arg(self.repo())
            .args(args)
            .output()?;
        if !output.status.success() {
            return Err(String::from_utf8_lossy(&output.stderr).to_string().into());
        }
        Ok(String::from_utf8(output.stdout)?.trim().into())
    }
    fn snapshot(&self) -> Result<()> {
        let archive = self.base.join("original-source.tar");
        if archive.exists() {
            fs::remove_file(&archive)?;
        }
        source_snapshot::snapshot(&self.tree(), &self.base.join("vendor"), &archive)?;
        let files = BTreeMap::from([("original-source.tar".into(), fs::read(archive)?)]);
        fs::write(self.bundle(), storage::deterministic_tar(&files)?)?;
        Ok(())
    }
    fn check(&self) -> Result<Value> {
        stable_source::check(&self.repo(), &self.record, &self.bundle())
    }
}

#[test]
fn actual_cli_binds_streamed_original_archive_and_binary_git_blobs_to_tag() -> Result<()> {
    let f = Fixture::new()?;
    let result = f.check()?;
    assert_eq!(result["source_sha"], f.record["source_sha"]);
    assert_eq!(result["original_inputs"], source_snapshot::FILES.len() + 1);
    assert_eq!(
        release_policy::git_bytes(&f.repo(), &["show", "v0.1.0:LICENSE"])?,
        b"\xffbinary license integrity fixture\n\n"
    );
    let record = f.base.join("record.json");
    fs::write(&record, serde_json::to_vec(&f.record)?)?;
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_among-tools"))
        .env_clear()
        .arg("check-stable-source")
        .arg(f.repo())
        .arg(&record)
        .arg(f.bundle())
        .output()?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(storage::parse_json(&output.stdout)?, result);
    Ok(())
}

#[test]
fn unrelated_commit_and_self_consistent_untagged_sources_are_rejected() -> Result<()> {
    let f = Fixture::new()?;
    let mut changed = f.record.clone();
    changed["source_sha"] = "a".repeat(40).into();
    assert!(stable_source::check(&f.repo(), &changed, &f.bundle()).is_err());
    fs::write(f.tree().join("LICENSE"), b"different source")?;
    let path = f.tree().join("snap/stable-build-input.json");
    let mut overlay = storage::parse_json(&fs::read(&path)?)?;
    overlay["original_inputs"]["LICENSE"] = storage::sha256(b"different source".as_slice())?.into();
    fs::write(path, serde_json::to_vec(&overlay)?)?;
    f.snapshot()?;
    let refusal = f
        .check()
        .err()
        .ok_or("untagged source accepted")?
        .to_string();
    assert!(
        refusal.contains("tagged commit"),
        "refused for another reason: {refusal}"
    );
    Ok(())
}

#[test]
fn extra_regular_source_members_and_symlink_bundles_are_rejected() -> Result<()> {
    let f = Fixture::new()?;
    fs::create_dir(f.tree().join("src"))?;
    fs::write(f.tree().join("src/extra.rs"), b"not tagged")?;
    f.snapshot()?;
    assert!(f
        .check()
        .err()
        .ok_or("extra source accepted")?
        .to_string()
        .contains("unexpected or missing"));
    let link = f.base.join("linked.tar");
    std::os::unix::fs::symlink(f.bundle(), &link)?;
    assert!(stable_source::check(&f.repo(), &f.record, &link).is_err());
    Ok(())
}
