use among_tools::{release_policy as policy, storage, Result};
use serde_json::{json, Value};
use std::fs;
use std::path::PathBuf;

struct Fixture(PathBuf);
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
impl Fixture {
    fn new() -> Result<Self> {
        let path = storage::private_directory(&std::env::temp_dir(), "among-policy")?;
        let f = Self(path);
        f.git(&["init", "-q", "-b", "main"])?;
        f.git(&["config", "user.name", "Synthetic Fixture"])?;
        f.git(&["config", "user.email", "fixture@example.invalid"])?;
        f.manifests("1.2.3")?;
        f.commit("synthetic source")?;
        f.git(&["-c", "tag.gpgSign=false", "tag", "v1.2.3"])?;
        fs::write(f.asset(), b"synthetic artifact, never uploaded")?;
        Ok(f)
    }
    fn manifests(&self, version: &str) -> Result<()> {
        for name in ["Cargo.toml", "examples/plugins/Cargo.toml"] {
            let path = self.0.join(name);
            fs::create_dir_all(path.parent().ok_or("fixture parent absent")?)?;
            fs::write(
                path,
                format!("[package]\nname=\"fixture\"\nversion=\"{version}\"\n"),
            )?;
        }
        Ok(())
    }
    fn git(&self, args: &[&str]) -> Result<String> {
        let output = std::process::Command::new("/usr/bin/git")
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .arg("-C")
            .arg(&self.0)
            .args(args)
            .output()?;
        if !output.status.success() {
            return Err(String::from_utf8_lossy(&output.stderr).to_string().into());
        }
        Ok(String::from_utf8(output.stdout)?.trim().into())
    }
    fn commit(&self, label: &str) -> Result<()> {
        self.git(&["add", "Cargo.toml", "examples/plugins/Cargo.toml"])?;
        self.git(&["-c", "commit.gpgSign=false", "commit", "-qm", label])?;
        Ok(())
    }
    fn asset(&self) -> PathBuf {
        self.0.join("among_1.2.3_amd64.snap")
    }
    fn stable(
        &self,
        tag: &str,
        assets: &[PathBuf],
        previous: Option<&Value>,
        groups: Option<&Value>,
    ) -> Result<Value> {
        policy::stable(&self.0, tag, "refs/heads/main", assets, previous, groups)
    }
}

#[test]
fn actual_git_tag_source_exact_retry_and_changed_artifact_are_observed() -> Result<()> {
    let f = Fixture::new()?;
    let record = f.stable("v1.2.3", &[f.asset()], None, None)?;
    assert_eq!(
        record["source_sha"],
        f.git(&["rev-parse", "v1.2.3^{commit}"])?
    );
    assert_eq!(
        record,
        f.stable("v1.2.3", &[f.asset()], Some(&record), None)?
    );
    let cli = std::process::Command::new(env!("CARGO_BIN_EXE_among-tools"))
        .env_clear()
        .arg("release-policy")
        .arg(&f.0)
        .args(["v1.2.3", "refs/heads/main"])
        .arg(f.asset())
        .output()?;
    assert!(
        cli.status.success(),
        "{}",
        String::from_utf8_lossy(&cli.stderr)
    );
    assert_eq!(storage::parse_json(&cli.stdout)?, record);
    fs::write(f.asset(), b"changed")?;
    assert!(f
        .stable("v1.2.3", &[f.asset()], Some(&record), None)
        .is_err());
    Ok(())
}

#[test]
fn malformed_older_untrusted_and_mismatched_tags_are_refused() -> Result<()> {
    let f = Fixture::new()?;
    for tag in [
        "1.2.3",
        "v01.2.3",
        "v1.2",
        "v1.2.3-rc1",
        "v1.2.3+build",
        "nightly",
        "v-1.2.3",
    ] {
        assert!(f.stable(tag, &[f.asset()], None, None).is_err(), "{tag}");
    }
    let mut newer = f.stable("v1.2.3", &[f.asset()], None, None)?;
    newer["tag"] = "v1.2.4".into();
    newer["version"] = "1.2.4".into();
    assert!(f
        .stable("v1.2.3", &[f.asset()], Some(&newer), None)
        .is_err());
    f.git(&["checkout", "-qb", "untrusted"])?;
    f.manifests("1.2.4")?;
    f.commit("untrusted source")?;
    f.git(&["-c", "tag.gpgSign=false", "tag", "v1.2.4"])?;
    assert!(f.stable("v1.2.4", &[f.asset()], None, None).is_err());
    f.git(&["checkout", "-q", "main"])?;
    f.git(&["-c", "tag.gpgSign=false", "tag", "v9.9.9"])?;
    assert!(f.stable("v9.9.9", &[f.asset()], None, None).is_err());
    fs::write(
        f.0.join("examples/plugins/Cargo.toml"),
        "[package]\nversion=\"1.2.5\"\n",
    )?;
    f.commit("mismatched plugin")?;
    f.git(&["-c", "tag.gpgSign=false", "tag", "v1.2.5"])?;
    assert!(f.stable("v1.2.5", &[f.asset()], None, None).is_err());
    Ok(())
}

#[test]
fn duplicate_missing_symlink_and_malformed_previous_provenance_fail() -> Result<()> {
    let f = Fixture::new()?;
    assert!(f
        .stable("v1.2.3", &[f.asset(), f.asset()], None, None)
        .is_err());
    assert!(f
        .stable("v1.2.3", &[f.0.join("missing")], None, None)
        .is_err());
    let link = f.0.join("linked.snap");
    std::os::unix::fs::symlink(f.asset(), &link)?;
    assert!(f.stable("v1.2.3", &[link], None, None).is_err());
    let record = f.stable("v1.2.3", &[f.asset()], None, None)?;
    for (key, value) in [
        ("source_sha", json!("short")),
        ("architecture", json!("arm64")),
        ("schema", json!(2)),
        ("assets", json!({})),
        ("unknown", json!(true)),
    ] {
        let mut changed = record.clone();
        changed[key] = value;
        assert!(
            f.stable("v1.2.3", &[f.asset()], Some(&changed), None)
                .is_err(),
            "{key}"
        );
    }
    Ok(())
}

#[test]
fn two_architectures_require_exact_distinct_checked_role_artifacts() -> Result<()> {
    let f = Fixture::new()?;
    let mut groups = json!({});
    let mut assets = Vec::new();
    for arch in ["amd64", "arm64"] {
        groups[arch] = json!({"snap":format!("among-{arch}.snap"),"source":format!("source-{arch}.tar"),"association":format!("association-{arch}.json")});
        for name in groups[arch]
            .as_object()
            .ok_or("fixture roles absent")?
            .values()
        {
            let name = name.as_str().ok_or("fixture name absent")?;
            let path = f.0.join(name);
            fs::write(&path, format!("synthetic {name}"))?;
            assets.push(path);
        }
    }
    let record = f.stable("v1.2.3", &assets, None, Some(&groups))?;
    assert_eq!(record["schema"], 2);
    assert!(record.get("architecture").is_none());
    assert_eq!(
        record,
        f.stable("v1.2.3", &assets, Some(&record), Some(&groups))?
    );
    for changed in [
        json!({"amd64":groups["amd64"]}),
        json!({"amd64":groups["amd64"],"arm64":groups["amd64"]}),
        json!({"amd64":groups["amd64"],"arm64":{"snap":"missing.snap","source":"source-arm64.tar","association":"association-arm64.json"}}),
    ] {
        assert!(f.stable("v1.2.3", &assets, None, Some(&changed)).is_err());
    }
    let extra = f.0.join("extra.snap");
    fs::write(&extra, b"extra")?;
    let mut extras = assets.clone();
    extras.push(extra);
    assert!(f.stable("v1.2.3", &extras, None, Some(&groups)).is_err());
    fs::write(
        assets.last().ok_or("fixture assets absent")?,
        b"changed arm64",
    )?;
    assert!(f
        .stable("v1.2.3", &assets, Some(&record), Some(&groups))
        .is_err());
    assert!(policy::validate_record(&json!({"schema":1,"tag":"v1.2.3","version":"1.2.3","source_sha":"a".repeat(40),"architecture":"amd64","assets":{"../escape":"b".repeat(64)}})).is_err());
    Ok(())
}
