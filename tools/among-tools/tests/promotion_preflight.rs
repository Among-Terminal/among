//! `preflight` refuses a publication that cannot possibly succeed.
//!
//! This is the gate that runs before any endpoint is contacted. A gap here is
//! worse than a gap in the state machine: the state machine can still refuse
//! later, but everything it does later has already published something.

use among_tools::{promotion_publication, storage, Result};
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

struct Fixture(PathBuf);

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn workspace() -> Result<Fixture> {
    Ok(Fixture(storage::private_directory(
        &std::env::temp_dir(),
        "among-preflight",
    )?))
}

/// A record, gates and asset map for `mode`, with the artifact files written.
fn scenario(
    root: &Path,
    mode: &str,
    source: &str,
) -> Result<(Value, Value, BTreeMap<String, PathBuf>)> {
    let mut groups = serde_json::Map::new();
    let mut recorded = serde_json::Map::new();
    let mut assets: BTreeMap<String, PathBuf> = BTreeMap::new();
    for architecture in ["amd64", "arm64"] {
        let snap = root.join(format!("among_{architecture}.snap"));
        let bundle = root.join(format!("sources-{architecture}.tar"));
        let association = root.join(format!("association-{architecture}.json"));
        // Not a real Snap: preflight must refuse on the earlier, cheaper
        // checks, so these fixtures never need to be installable.
        fs::write(&snap, format!("synthetic snap {architecture}"))?;
        fs::write(&bundle, format!("synthetic sources {architecture}"))?;
        fs::write(
            &association,
            format!("synthetic association {architecture}"),
        )?;
        for path in [&snap, &bundle, &association] {
            let name = path
                .file_name()
                .and_then(|name| name.to_str())
                .ok_or("fixture filename is not UTF-8")?
                .to_string();
            recorded.insert(name.clone(), json!(storage::sha256(fs::File::open(path)?)?));
            assets.insert(name, path.clone());
        }
        groups.insert(
            architecture.to_string(),
            json!({
                "snap": snap.file_name().and_then(|n| n.to_str()).unwrap_or_default(),
                "source": bundle.file_name().and_then(|n| n.to_str()).unwrap_or_default(),
                "association": association.file_name().and_then(|n| n.to_str()).unwrap_or_default(),
            }),
        );
    }
    let mut record = json!({
        "schema": 2,
        "tag": if mode == "nightly" { format!("nightly-{source}") } else { "v0.1.0".to_string() },
        "version": if mode == "nightly" { format!("n20261011-{}", &source[..12]) } else { "0.1.0".to_string() },
        "source_sha": source,
        "architectures": Value::Object(groups),
        "assets": Value::Object(recorded),
    });
    if mode == "nightly" {
        record["mode"] = json!("nightly");
    }
    let mut checks = serde_json::Map::new();
    for gate in among_tools::promotion::GATES {
        checks.insert(gate.to_string(), json!(true));
    }
    let gates = json!({
        "source_sha": record["source_sha"],
        "assets": record["assets"],
        "checks": Value::Object(checks.clone()),
        "architectures": {"amd64": Value::Object(checks.clone()), "arm64": Value::Object(checks)},
    });
    Ok((record, gates, assets))
}

#[test]
fn preflight_refuses_without_protected_approval_before_reading_anything() -> Result<()> {
    let fixture = workspace()?;
    let (record, gates, assets) = scenario(&fixture.0, "nightly", &"a".repeat(40))?;
    // No repository is consulted: an unapproved run must fail on its own cause
    // rather than on some later, more expensive check.
    let refusal = promotion_publication::preflight(
        &record,
        &gates,
        &assets,
        false,
        Path::new("/nonexistent-repository"),
    )
    .err()
    .ok_or("an unapproved publication was allowed past preflight")?
    .to_string();
    assert!(
        refusal.contains("approval"),
        "the refusal did not name its cause: {refusal}"
    );
    Ok(())
}

#[test]
fn a_record_whose_digests_do_not_match_the_supplied_assets_is_refused() -> Result<()> {
    let fixture = workspace()?;
    let (mut record, gates, assets) = scenario(&fixture.0, "nightly", &"b".repeat(40))?;
    // Change a digest after the gates were produced: the record no longer
    // describes the bytes that would be published.
    record["assets"]
        .as_object_mut()
        .ok_or("assets absent")?
        .values_mut()
        .for_each(|digest| *digest = json!("f".repeat(64)));

    let refusal = promotion_publication::preflight(&record, &gates, &assets, true, &fixture.0)
        .err()
        .ok_or("altered digests were accepted")?
        .to_string();
    assert!(!refusal.is_empty());
    Ok(())
}

#[test]
fn a_gate_that_did_not_pass_refuses_the_publication() -> Result<()> {
    let fixture = workspace()?;
    let (record, mut gates, assets) = scenario(&fixture.0, "nightly", &"c".repeat(40))?;
    gates["checks"]["metadata"] = json!(false);
    assert!(
        promotion_publication::preflight(&record, &gates, &assets, true, &fixture.0).is_err(),
        "a failed gate did not refuse the publication"
    );
    // One architecture passing is not enough: both must.
    let (_, gates, assets) = scenario(&fixture.0, "nightly", &"c".repeat(40))?;
    let mut gates = gates;
    gates["architectures"]["arm64"]["metadata"] = json!(false);
    assert!(
        promotion_publication::preflight(&record, &gates, &assets, true, &fixture.0).is_err(),
        "a failed per-architecture gate did not refuse the publication"
    );
    Ok(())
}

/// A nightly must be reachable from the protected main line. Without a real
/// repository the ancestry query cannot succeed, which is exactly the refusal
/// this test observes: an unverifiable source is not a publishable source.
#[test]
fn a_nightly_whose_source_cannot_be_verified_on_main_is_refused() -> Result<()> {
    let fixture = workspace()?;
    let source = "d".repeat(40);
    let (record, gates, assets) = scenario(&fixture.0, "nightly", &source)?;
    // An empty directory is not a repository at all.
    assert!(
        promotion_publication::preflight(&record, &gates, &assets, true, &fixture.0).is_err(),
        "an unverifiable nightly source was allowed past preflight"
    );
    Ok(())
}

/// Run git in `repo` with no user or system configuration, so the fixture
/// behaves the same on a developer machine and on a clean runner.
fn git(repo: &Path, args: &[&str]) -> Result<String> {
    let output = std::process::Command::new("/usr/bin/git")
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).to_string().into());
    }
    Ok(String::from_utf8(output.stdout)?.trim().into())
}

/// A repository whose protected main carries `v0.1.0` with both manifests at
/// 0.1.0: the exact state a genuine stable release is derived from.
fn released_repository(root: &Path) -> Result<String> {
    git(root, &["init", "-q", "-b", "main"])?;
    git(root, &["config", "user.name", "Synthetic Fixture"])?;
    git(root, &["config", "user.email", "fixture@example.invalid"])?;
    for name in ["Cargo.toml", "examples/plugins/Cargo.toml"] {
        let path = root.join(name);
        fs::create_dir_all(path.parent().ok_or("fixture parent absent")?)?;
        fs::write(path, "[package]\nname=\"fixture\"\nversion=\"0.1.0\"\n")?;
    }
    git(root, &["add", "Cargo.toml", "examples/plugins/Cargo.toml"])?;
    git(
        root,
        &["-c", "commit.gpgSign=false", "commit", "-qm", "release"],
    )?;
    git(root, &["-c", "tag.gpgSign=false", "tag", "v0.1.0"])?;
    git(root, &["update-ref", "refs/remotes/origin/main", "HEAD"])?;
    git(root, &["rev-parse", "HEAD"])
}

/// A stable record is the record `release_policy::stable` derives, which has
/// no `mode` field. Preflight once compared it with a copy into which it had
/// inserted `"mode":"stable"`, so no stable release could ever pass. The
/// synthetic Snap stops preflight at the packaged-identity check; reaching that
/// check is what proves the provenance comparison accepted a genuine record.
#[test]
fn a_genuine_stable_record_passes_the_provenance_comparison() -> Result<()> {
    let fixture = workspace()?;
    let repository = fixture.0.join("repository");
    fs::create_dir_all(&repository)?;
    let source = released_repository(&repository)?;
    let artifacts = fixture.0.join("artifacts");
    fs::create_dir_all(&artifacts)?;
    let (record, gates, assets) = scenario(&artifacts, "stable", &source)?;
    assert!(
        record.get("mode").is_none(),
        "a stable record carries no mode"
    );
    let refusal = promotion_publication::preflight(&record, &gates, &assets, true, &repository)
        .err()
        .ok_or("a synthetic Snap was accepted as a packaged artifact")?
        .to_string();
    assert!(
        !refusal.contains("provenance"),
        "a genuine stable record was refused by the provenance comparison: {refusal}"
    );
    Ok(())
}

/// The other side of the same comparison: a record that claims a source other
/// than the commit the protected tag names is not the released state.
#[test]
fn a_stable_record_claiming_another_source_is_refused_by_provenance() -> Result<()> {
    let fixture = workspace()?;
    let repository = fixture.0.join("repository");
    fs::create_dir_all(&repository)?;
    released_repository(&repository)?;
    let artifacts = fixture.0.join("artifacts");
    fs::create_dir_all(&artifacts)?;
    let (record, gates, assets) = scenario(&artifacts, "stable", &"e".repeat(40))?;
    let refusal = promotion_publication::preflight(&record, &gates, &assets, true, &repository)
        .err()
        .ok_or("a record with a foreign source passed preflight")?
        .to_string();
    assert!(
        refusal.contains("stable provenance differs"),
        "the refusal did not name the provenance mismatch: {refusal}"
    );
    Ok(())
}
