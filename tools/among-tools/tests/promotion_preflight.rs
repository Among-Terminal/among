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
