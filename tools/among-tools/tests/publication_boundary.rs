//! The publication boundary: only an explicitly checked, manually approved
//! promotion may reach a release.
//!
//! These tests are about what must NOT be able to happen. A schedule trigger,
//! a branch push, or an edge-channel instruction would each publish without a
//! person deciding to publish, and each would do it silently.

use among_tools::Result;
use serde_yaml_ng::Value;
use std::{fs, path::PathBuf};

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn workflow(name: &str) -> Result<Value> {
    let path = root().join(".github/workflows").join(name);
    let text = fs::read_to_string(path)?;
    Ok(serde_yaml_ng::from_str(&text)?)
}

fn top_level_keys(document: &Value) -> Vec<String> {
    document
        .as_mapping()
        .map(|mapping| {
            mapping
                .iter()
                .filter_map(|(key, _)| match key {
                    Value::String(name) => Some(name.clone()),
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Every event that could fire without a person deciding to publish.
const AUTOMATIC_EVENTS: [&str; 8] = [
    "schedule",
    "push",
    "pull_request",
    "pull_request_target",
    "workflow_dispatch",
    "workflow_run",
    "repository_dispatch",
    "release",
];

/// No active workflow may build or publish on a schedule or a branch push.
#[test]
fn no_workflow_publishes_on_a_schedule_or_a_branch() -> Result<()> {
    let directory = root().join(".github/workflows");
    let mut checked = 0;
    for entry in fs::read_dir(&directory)? {
        let path = entry?.path();
        if path.extension().is_none_or(|extension| extension != "yml") {
            continue;
        }
        let document: Value = serde_yaml_ng::from_str(&fs::read_to_string(&path)?)?;
        let keys = top_level_keys(&document);
        for event in AUTOMATIC_EVENTS {
            assert!(
                !keys.iter().any(|key| key == event),
                "{} declares a `{event}` trigger; publication must not happen without an explicit, checked decision",
                path.display()
            );
        }
        checked += 1;
    }
    assert!(checked > 0, "no workflows were inspected");
    Ok(())
}

/// The nightly channel must never be reachable from the stable path.
///
/// A nightly is built from a moving ref and rolls a mutable tag; promoting it
/// to users who expect stable releases would ship unreviewed bytes.
#[test]
fn no_stable_promotion_path_mentions_the_edge_channel() -> Result<()> {
    let document = workflow("checks.yml")?;
    let text = format!("{document:?}");
    assert!(
        !text.contains("latest/edge"),
        "the stable checks reference the edge channel"
    );
    let recording = fs::read_to_string(root().join(".github/workflows/deb-release.yml"))?;
    assert!(
        !recording.contains("latest/edge"),
        "the Debian release job references the edge channel"
    );
    Ok(())
}

/// Publication helpers that exist offline are not publication.
///
/// The repository keeps offline rehearsal tools that name channels and
/// destinations. They must stay unwired from CI, or "deferred" would be a claim
/// rather than a fact.
#[test]
fn offline_rehearsal_tools_are_not_invoked_by_any_workflow() -> Result<()> {
    let directory = root().join(".github/workflows");
    let mut invocations = Vec::new();
    for entry in fs::read_dir(&directory)? {
        let path = entry?.path();
        if path.extension().is_none_or(|extension| extension != "yml") {
            continue;
        }
        let text = fs::read_to_string(&path)?;
        for (index, line) in text.lines().enumerate() {
            // `release-promotion` publishes to GitHub and the Store; a
            // rehearsal driver of it must never return. The read-only
            // `check-promotion-admission` and the offline `cargo test` suites
            // do not name either command and stay allowed.
            if line.contains("release-promotion") || line.contains("rehearse-stable-promotion") {
                invocations.push(format!("{}:{}: {}", path.display(), index + 1, line.trim()));
            }
        }
    }
    assert!(
        invocations.is_empty(),
        "a workflow invokes publication or its rehearsal: {invocations:?}"
    );
    Ok(())
}
