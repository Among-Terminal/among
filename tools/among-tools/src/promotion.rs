//! Publication state machine for stable releases.
//!
//! The endpoint adapters are separate types so the ordering, refusal and
//! retry behaviour can be exercised against local fakes with no credentials,
//! no network and no publication. The concrete GitHub and Snap Store adapters
//! live in `promotion_destinations`; this module never performs I/O itself.

use crate::{deb, release_policy, storage, Result};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::File,
    path::{Path, PathBuf},
    time::Duration,
};

/// How long a concurrent publication waits before refusing to proceed.
///
/// Bounded so a wedged holder cannot hang a release job, but long enough that
/// a retry queued behind a real publication still gets its turn.
const PUBLICATION_LOCK_WAIT: Duration = Duration::from_secs(300);

pub const GATES: [&str; 6] = [
    "packages",
    "focus",
    "metadata",
    "installed-runtime",
    "source-delivery",
    "privacy-license",
];

pub const ARCHITECTURES: [&str; 2] = ["amd64", "arm64"];

/// The three artifacts each architecture must contribute to a stable release.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Group {
    pub snap: String,
    pub source: String,
    pub association: String,
}

fn passed(value: &Value) -> bool {
    value.as_object().is_some_and(|checks| {
        checks.keys().map(String::as_str).collect::<BTreeSet<_>>() == GATES.into_iter().collect()
            && checks.values().all(|passed| passed == true)
    })
}

/// Map a promotion record to the artifacts each architecture publishes.
///
/// A schema-1 record is readable for historical offline fixtures but can never
/// authorize the first public two-architecture payload, so it is refused here
/// rather than silently widened.
pub fn artifact_groups(record: &Value) -> Result<BTreeMap<String, Group>> {
    let architectures = record["architectures"]
        .as_object()
        .ok_or("release requires checked amd64 and arm64 artifacts")?;
    if architectures
        .keys()
        .map(String::as_str)
        .collect::<BTreeSet<_>>()
        != ARCHITECTURES.into_iter().collect()
    {
        return Err("release requires checked amd64 and arm64 artifacts".into());
    }
    let expected_assets = record["assets"]
        .as_object()
        .ok_or("promotion record assets absent")?;
    let mut groups = BTreeMap::new();
    let mut names = Vec::new();
    for (architecture, group) in architectures {
        let snap = group["snap"]
            .as_str()
            .ok_or("architecture artifact names must be strings")?;
        let source = group["source"]
            .as_str()
            .ok_or("architecture artifact names must be strings")?;
        let association = group["association"]
            .as_str()
            .ok_or("architecture artifact names must be strings")?;
        if !snap.ends_with(".snap") || !source.ends_with(".tar") || !association.ends_with(".json")
        {
            return Err("invalid architecture artifact roles".into());
        }
        names.extend([snap, source, association]);
        groups.insert(
            architecture.clone(),
            Group {
                snap: snap.to_string(),
                source: source.to_string(),
                association: association.to_string(),
            },
        );
    }
    let unique = names.iter().collect::<BTreeSet<_>>();
    if unique.len() != names.len() {
        return Err("architecture artifacts must be distinct checked assets".into());
    }
    if names
        .iter()
        .any(|name| !expected_assets.contains_key(*name))
    {
        return Err("architecture artifacts must be distinct checked assets".into());
    }
    // A Snap asset that belongs to no architecture would be served without
    // corresponding sources, so it is an unexpected artifact.
    let snaps = expected_assets
        .keys()
        .filter(|name| name.ends_with(".snap"))
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    let grouped = groups
        .values()
        .map(|group| group.snap.as_str())
        .collect::<BTreeSet<_>>();
    if snaps != grouped {
        return Err("unexpected Snap artifact".into());
    }
    Ok(groups)
}

/// Whether `record` is a nightly publication.
///
/// `release_policy::validate_record` admits exactly two shapes: a nightly
/// declares `"mode":"nightly"`, and a stable release carries no `mode` at all.
/// Every caller decides stable-versus-nightly through this one predicate; a
/// comparison against an invented `"stable"` value is never true for a real
/// record and silently turns stable-only checks off.
pub fn is_nightly(record: &Value) -> bool {
    record["mode"] == "nightly"
}

/// Read the packaged identity from the Snap itself rather than trusting a
/// filename or the record. A devel candidate can never satisfy a stable
/// promotion.
pub fn check_snap_identity(path: &Path, record: &Value, architecture: &str) -> Result<()> {
    let metadata = crate::snap_runtime::snap_metadata(path)?;
    if metadata["name"] != "among"
        || metadata["version"] != record["version"]
        || metadata["confinement"] != "classic"
        || metadata["architectures"] != json!([architecture])
    {
        return Err("packaged identity/version/confinement/architecture mismatch".into());
    }
    if !is_nightly(record) && metadata["grade"] != "stable" {
        return Err("a devel candidate cannot be promoted to stable".into());
    }
    Ok(())
}

/// The remote destinations a promotion talks to.
///
/// Implementations reconcile immutable remote identity before writing anything.
/// A trait exists so ordering and refusal can be observed without credentials.
pub trait Destinations {
    /// The previously published stable record, if the destination has one.
    fn latest_stable(&mut self) -> Result<Option<Value>>;
    /// The record of the last fully successful nightly, if any.
    ///
    /// This is the rolling marker: it advances only after every destination
    /// succeeded, so its presence is the only evidence that a nightly reached
    /// users. Nothing may treat it as advisory.
    fn marker(&mut self) -> Result<Option<Value>>;
    /// Advance the rolling nightly release to this record and re-upload the
    /// checked artifacts. Called only after both destinations have succeeded.
    fn roll(&mut self, record: &Value, assets: &BTreeMap<String, PathBuf>) -> Result<()>;
    /// The checked record already attached to this tag, if any.
    fn record(&mut self, tag: &str) -> Result<Option<Value>>;
    /// Create the draft release and attach every checked artifact.
    fn prepare(&mut self, record: &Value, assets: &BTreeMap<String, PathBuf>) -> Result<()>;
    /// Publish the prepared release and confirm the tag still identifies the
    /// checked source.
    fn publish(&mut self, record: &Value) -> Result<()>;
    /// Ensure the Store serves these exact bytes for one architecture.
    fn ensure(
        &mut self,
        path: &Path,
        version: &str,
        channel: &str,
        architecture: &str,
    ) -> Result<()>;
}

/// Validate everything that must hold before any destination is contacted.
pub fn validate(
    record: &Value,
    gates: &Value,
    assets: &BTreeMap<String, PathBuf>,
    approved: bool,
) -> Result<BTreeMap<String, Group>> {
    // Refuse before reading an artifact directory when approval is absent.
    if !approved {
        return Err("protected publication approval is required".into());
    }
    release_policy::validate_record(record)?;
    if gates["source_sha"] != record["source_sha"] || gates["assets"] != record["assets"] {
        return Err("gates are not bound to these exact source/artifact digests".into());
    }
    if !passed(&gates["checks"]) {
        return Err("every required release gate must pass".into());
    }
    let groups = artifact_groups(record)?;
    let receipts = gates["architectures"]
        .as_object()
        .ok_or("architecture gate receipts absent")?;
    if receipts.keys().map(String::as_str).collect::<BTreeSet<_>>()
        != ARCHITECTURES.into_iter().collect()
        || receipts.values().any(|checks| !passed(checks))
    {
        return Err("every architecture must pass every release gate".into());
    }
    let expected = record["assets"].as_object().ok_or("assets absent")?;
    if assets.keys().collect::<BTreeSet<_>>() != expected.keys().collect::<BTreeSet<_>>() {
        return Err("exact artifact set is required".into());
    }
    for (name, path) in assets {
        storage::relative_path(Path::new(name))?;
        if Path::new(name).components().count() != 1
            || !path.symlink_metadata()?.file_type().is_file()
        {
            return Err("unsafe promotion artifact".into());
        }
        if storage::sha256(File::open(path)?)? != expected[name] {
            return Err("artifact changed after checks".into());
        }
    }
    Ok(groups)
}

/// Verify what each packaged Snap actually claims, for every architecture.
///
/// This is separate from `validate` because reading a Snap requires the real
/// artifact, while ordering and refusal must be observable against local fakes.
/// The command entry point always runs both, in that order, before any
/// destination is contacted.
pub fn check_packaged_identities(
    record: &Value,
    assets: &BTreeMap<String, PathBuf>,
    groups: &BTreeMap<String, Group>,
) -> Result<()> {
    for (architecture, group) in groups {
        check_snap_identity(&assets[&group.snap], record, architecture)?;
    }
    Ok(())
}

/// Drive one stable promotion.
///
/// Returns `published` after both destinations succeed and `unchanged` when the
/// release is already in that exact state. A failure after the GitHub release
/// is published leaves it in place; the Store is reconciled on retry rather than
/// rolled back, because corresponding sources must stay public once served.
pub fn promote(
    record: &Value,
    gates: &Value,
    assets: &BTreeMap<String, PathBuf>,
    destinations: &mut impl Destinations,
    approved: bool,
    lock_root: &Path,
    lock_name: &str,
) -> Result<&'static str> {
    let groups = validate(record, gates, assets, approved)?;
    // Serialize concurrent promotions so two runs cannot both decide to write.
    // A concurrent promotion is a retry of the same intent, so it waits for the
    // holder instead of failing; the interactive and installation locks stay
    // fail-fast because there a second holder is a conflicting user action.
    let _lock = storage::ExclusiveLock::acquire_waiting(
        lock_root,
        Path::new(lock_name),
        PUBLICATION_LOCK_WAIT,
    )?;

    let nightly = is_nightly(record);
    let tag = record["tag"]
        .as_str()
        .ok_or("promotion record has no tag")?;
    if nightly {
        // The rolling marker's state file is this record and nothing else. A
        // repeat of an already successful nightly must write nothing at all:
        // re-uploading would push the edge channel to an identical revision and
        // churn a release users are already downloading.
        if let Some(marker) = destinations.marker()? {
            if marker["source_sha"] == record["source_sha"] {
                if &marker != record {
                    return Err("successful nightly digest conflict".into());
                }
                return Ok("unchanged");
            }
        }
    } else {
        if let Some(previous) = destinations.latest_stable()? {
            let previous_tag = previous["tag"]
                .as_str()
                .ok_or("published stable release has no tag")?;
            // A published release newer than this candidate would be replaced by
            // an older tag, so a stable release may only move forward.
            if deb::stable_version(tag)? < deb::stable_version(previous_tag)? {
                return Err("stable version must increase".into());
            }
            if previous_tag == tag && &previous != record {
                return Err("immutable stable release conflict".into());
            }
        }
    }
    if let Some(existing) = destinations.record(tag)? {
        if &existing != record {
            return Err(
                "prepared artifact/source conflict: reuse the original checked artifact".into(),
            );
        }
    }
    destinations.prepare(record, assets)?;
    // Corresponding sources must already be public when a Store channel starts
    // serving the binary.
    destinations.publish(record)?;
    let version = record["version"]
        .as_str()
        .ok_or("promotion record has no version")?;
    let channel = if nightly {
        "latest/edge"
    } else {
        "latest/stable"
    };
    for (architecture, group) in &groups {
        destinations.ensure(&assets[&group.snap], version, channel, architecture)?;
    }
    if nightly {
        // Only after both destinations succeeded. A partial failure must leave
        // the marker unset so a retry re-reconciles rather than declaring a
        // nightly complete that never reached users.
        destinations.roll(record, assets)?;
    }
    Ok("published")
}

pub fn check(record: &Value, gates: &Value, directory: &Path, approved: bool) -> Result<Value> {
    if !approved {
        return Err("protected publication approval is required".into());
    }
    let mut assets = BTreeMap::new();
    for entry in std::fs::read_dir(directory)? {
        let entry = entry?;
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| "artifact filename is not UTF-8")?;
        let path = storage::safe_path(directory, Path::new(&name))?;
        if !path.symlink_metadata()?.file_type().is_file() {
            return Err("promotion artifact directory must contain only regular files".into());
        }
        assets.insert(name, path);
    }
    let groups = validate(record, gates, &assets, approved)?;
    check_packaged_identities(record, &assets, &groups)?;
    Ok(
        json!({"schema":1,"source_sha":record["source_sha"],"tag":record["tag"],"assets":record["assets"],"scope":"read-only promotion admission and packaged identity verification; no endpoint reconciliation or publication","published":false}),
    )
}
