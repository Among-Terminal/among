//! The nightly half of the promotion state machine.
//!
//! A nightly is the mutable counterpart to a stable release: the same source
//! may be published again and again under one rolling tag. That makes the
//! success marker load-bearing. If it advances before both destinations
//! succeeded, a failed nightly is reported as complete and users keep an edge
//! channel pointing at bytes that were never reconciled.

use among_tools::{promotion, storage, Result};
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

/// A destination that records what it was asked to do and can fail part way.
struct Recorder {
    marker: Option<Value>,
    calls: Vec<String>,
    published: Option<Value>,
    /// Fail the Store step for this architecture, once.
    fail_architecture: Option<String>,
    rolled: Option<Value>,
}

impl Recorder {
    fn new() -> Self {
        Self {
            marker: None,
            calls: Vec::new(),
            published: None,
            fail_architecture: None,
            rolled: None,
        }
    }

    fn store_calls(&self) -> Vec<&String> {
        self.calls
            .iter()
            .filter(|call| call.starts_with("ensure:"))
            .collect()
    }
}

impl promotion::Destinations for Recorder {
    fn latest_stable(&mut self) -> Result<Option<Value>> {
        self.calls.push("latest_stable".into());
        // A nightly must never consult the stable record: the rolling channel
        // is independent of the last stable release.
        Ok(self.published.clone())
    }

    fn marker(&mut self) -> Result<Option<Value>> {
        self.calls.push("marker".into());
        Ok(self.marker.clone())
    }

    fn roll(&mut self, record: &Value, assets: &BTreeMap<String, PathBuf>) -> Result<()> {
        self.calls.push("roll".into());
        for path in assets.values() {
            if !path.is_file() {
                return Err("roll received a missing artifact".into());
            }
        }
        self.rolled = Some(record.clone());
        self.marker = Some(record.clone());
        Ok(())
    }

    fn record(&mut self, tag: &str) -> Result<Option<Value>> {
        self.calls.push(format!("record:{tag}"));
        Ok(None)
    }

    fn prepare(&mut self, record: &Value, _assets: &BTreeMap<String, PathBuf>) -> Result<()> {
        self.calls.push(format!("prepare:{}", record["tag"]));
        Ok(())
    }

    fn publish(&mut self, record: &Value) -> Result<()> {
        self.calls.push(format!("publish:{}", record["tag"]));
        self.published = Some(record.clone());
        Ok(())
    }

    fn ensure(
        &mut self,
        _path: &Path,
        version: &str,
        channel: &str,
        architecture: &str,
    ) -> Result<()> {
        self.calls
            .push(format!("ensure:{architecture}:{channel}:{version}"));
        if self.fail_architecture.as_deref() == Some(architecture) {
            return Err("simulated Store failure".into());
        }
        Ok(())
    }
}

/// A complete two-architecture nightly record with real artifact files.
fn nightly(source: &str) -> Result<(PathBuf, Value, Value, BTreeMap<String, PathBuf>)> {
    let root = storage::private_directory(&std::env::temp_dir(), "among-nightly")?;
    let mut groups = serde_json::Map::new();
    let mut recorded = serde_json::Map::new();
    let mut assets: BTreeMap<String, PathBuf> = BTreeMap::new();
    for architecture in promotion::ARCHITECTURES {
        let snap = root.join(format!("nightly_{architecture}.snap"));
        let bundle = root.join(format!("nightly-sources-{architecture}.tar"));
        let association = root.join(format!("nightly-association-{architecture}.json"));
        fs::write(&snap, format!("synthetic nightly snap {architecture}"))?;
        fs::write(&bundle, format!("synthetic nightly source {architecture}"))?;
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
        let name_of = |path: &Path| -> Result<String> {
            path.file_name()
                .and_then(|name| name.to_str())
                .map(str::to_string)
                .ok_or_else(|| "fixture filename is not UTF-8".into())
        };
        groups.insert(
            architecture.to_string(),
            json!({
                "snap": name_of(&snap)?,
                "source": name_of(&bundle)?,
                "association": name_of(&association)?,
            }),
        );
    }
    let record = json!({
        "schema": 2,
        "mode": "nightly",
        "tag": format!("nightly-{source}"),
        "version": format!("n20261011-{}", &source[..12]),
        "source_sha": source,
        "architectures": Value::Object(groups),
        "assets": Value::Object(recorded),
    });
    let mut checks = serde_json::Map::new();
    for gate in promotion::GATES {
        checks.insert(gate.to_string(), json!(true));
    }
    let gates = json!({
        "source_sha": record["source_sha"],
        "assets": record["assets"],
        "checks": Value::Object(checks.clone()),
        "architectures": {
            "amd64": Value::Object(checks.clone()),
            "arm64": Value::Object(checks),
        },
    });
    Ok((root, record, gates, assets))
}

fn promote(
    root: &Path,
    record: &Value,
    gates: &Value,
    assets: &BTreeMap<String, PathBuf>,
    recorder: &mut Recorder,
) -> Result<&'static str> {
    promotion::promote(
        record,
        gates,
        assets,
        recorder,
        true,
        root,
        "publication.lock",
    )
}

#[test]
fn a_nightly_publishes_to_the_edge_channel_and_rolls_the_marker() -> Result<()> {
    let source = "b".repeat(40);
    let (root, record, gates, assets) = nightly(&source)?;
    let mut recorder = Recorder::new();

    assert_eq!(
        promote(&root, &record, &gates, &assets, &mut recorder)?,
        "published"
    );
    // The edge channel, never stable: a nightly must not move the stable track.
    let channels = recorder.store_calls();
    assert_eq!(channels.len(), 2, "both architectures must reach the Store");
    for call in channels {
        assert!(
            call.contains(":latest/edge:"),
            "a nightly reached {call:?} instead of latest/edge"
        );
        assert!(!call.contains("latest/stable"));
    }
    // The marker may only exist after the Store steps succeeded.
    assert!(recorder.calls.iter().any(|call| call == "roll"));
    let roll_at = recorder
        .calls
        .iter()
        .position(|call| call == "roll")
        .ok_or("roll did not happen")?;
    let last_ensure = recorder
        .calls
        .iter()
        .rposition(|call| call.starts_with("ensure:"))
        .ok_or("no Store step happened")?;
    assert!(
        roll_at > last_ensure,
        "the marker advanced before the Store"
    );
    assert_eq!(recorder.marker.as_ref(), Some(&record));
    fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn a_repeated_nightly_is_unchanged_and_writes_nothing() -> Result<()> {
    let source = "c".repeat(40);
    let (root, record, gates, assets) = nightly(&source)?;
    let mut recorder = Recorder::new();
    assert_eq!(
        promote(&root, &record, &gates, &assets, &mut recorder)?,
        "published"
    );

    let before = recorder.calls.len();
    assert_eq!(
        promote(&root, &record, &gates, &assets, &mut recorder)?,
        "unchanged",
        "a repeated nightly must report unchanged"
    );
    let after = &recorder.calls[before..];
    // Exactly one marker read, then nothing: no prepare, publish, ensure or roll.
    assert_eq!(
        after,
        ["marker"],
        "a repeated nightly performed {:?} beyond reading the marker",
        after
    );
    fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn a_nightly_that_fails_part_way_does_not_advance_the_marker() -> Result<()> {
    let source = "d".repeat(40);
    let (root, record, gates, assets) = nightly(&source)?;
    let mut recorder = Recorder::new();
    recorder.fail_architecture = Some("arm64".into());

    assert!(
        promote(&root, &record, &gates, &assets, &mut recorder).is_err(),
        "a failing Store step was reported as a published nightly"
    );
    // The GitHub release exists, but the nightly never reached users, so the
    // marker must stay unset or a later run would call it complete.
    assert!(
        recorder.published.is_some(),
        "sources should already be public"
    );
    assert!(
        recorder.marker.is_none(),
        "the marker advanced despite a failed Store step"
    );
    assert!(recorder.rolled.is_none());

    // The retry, once the Store is healthy, completes and only then rolls.
    recorder.fail_architecture = None;
    assert_eq!(
        promote(&root, &record, &gates, &assets, &mut recorder)?,
        "published"
    );
    assert_eq!(recorder.marker.as_ref(), Some(&record));
    fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn a_marker_for_the_same_source_with_different_content_is_a_conflict() -> Result<()> {
    let source = "e".repeat(40);
    let (root, record, gates, assets) = nightly(&source)?;
    let mut recorder = Recorder::new();
    assert_eq!(
        promote(&root, &record, &gates, &assets, &mut recorder)?,
        "published"
    );

    // The same source already published, but under a different record. A nightly
    // version is `<date>-<12 hex of the source>`, so the date may legitimately
    // differ between runs while the source stays identical. Re-pointing the
    // marker at a different record for the same source would make the marker
    // disagree with what users were actually served.
    let mut altered = record.clone();
    altered["version"] = json!(format!("n20261231-{}", &source[..12]));
    recorder.calls.clear();
    let refusal = promote(&root, &altered, &gates, &assets, &mut recorder)
        .err()
        .ok_or("a conflicting nightly record was accepted")?
        .to_string();
    assert!(refusal.contains("nightly digest conflict"), "{refusal}");
    assert!(
        !recorder
            .calls
            .iter()
            .any(|call| call.starts_with("ensure:")),
        "a conflicting record reached the Store before being refused"
    );
    fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn a_nightly_never_consults_the_published_stable_record() -> Result<()> {
    let source = "f".repeat(40);
    let (root, record, gates, assets) = nightly(&source)?;
    let mut recorder = Recorder::new();
    // A stable release exists. It must neither block the nightly nor be
    // compared against it.
    recorder.published =
        Some(json!({"tag":"v0.9.0","version":"0.9.0","source_sha":"a".repeat(40)}));
    assert_eq!(
        promote(&root, &record, &gates, &assets, &mut recorder)?,
        "published"
    );
    assert!(
        !recorder.calls.iter().any(|call| call == "latest_stable"),
        "the nightly path consulted the stable release"
    );
    fs::remove_dir_all(root)?;
    Ok(())
}
