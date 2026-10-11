//! The publication destination a real promotion talks to.
//!
//! `promotion::Destinations` is deliberately split so the state machine can be
//! exercised against fakes. A release needs one object, though: the same run
//! creates a GitHub release, publishes it, and then makes the Store serve the
//! same bytes. This type composes the two concrete adapters so a caller cannot
//! accidentally drive one without the other.
//!
//! The split is by responsibility, not by convenience. Release identity,
//! provenance and the rolling marker belong to GitHub; the channel, the
//! revision and the digest belong to the Store. Neither is a fallback for the
//! other, and this type refuses to invent behaviour when one is unavailable.

use crate::{
    promotion::{self, Destinations},
    promotion_destinations::{GitHubRelease, SnapStore},
    Result,
};
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

/// The protected branch every published commit must already be on.
const MAIN_REF: &str = "refs/remotes/origin/main";

/// GitHub for release identity and the rolling marker, Store for the channel.
pub struct Publication {
    github: GitHubRelease,
    store: SnapStore,
}

impl Publication {
    /// Build both destinations. Both credentials are required up front so a
    /// run cannot discover a missing credential halfway through, after the
    /// release exists but before the channel moves.
    pub fn new(repository: &str, workdir: &Path) -> Result<Self> {
        Ok(Self {
            github: GitHubRelease::new(repository, workdir)?,
            store: SnapStore::new(workdir)?,
        })
    }
}

impl Destinations for Publication {
    fn latest_stable(&mut self) -> Result<Option<Value>> {
        self.github.latest_stable()
    }

    fn marker(&mut self) -> Result<Option<Value>> {
        self.github.marker()
    }

    fn record(&mut self, tag: &str) -> Result<Option<Value>> {
        self.github.record(tag)
    }

    fn prepare(&mut self, record: &Value, assets: &BTreeMap<String, PathBuf>) -> Result<()> {
        self.github.prepare(record, assets)
    }

    fn publish(&mut self, record: &Value) -> Result<()> {
        self.github.publish(record)
    }

    fn roll(&mut self, record: &Value, assets: &BTreeMap<String, PathBuf>) -> Result<()> {
        self.github.roll(record, assets)
    }

    fn ensure(
        &mut self,
        path: &Path,
        version: &str,
        channel: &str,
        architecture: &str,
    ) -> Result<()> {
        self.store.ensure(path, version, channel, architecture)
    }
}

/// Everything a publication needs before any endpoint is contacted.
///
/// This runs the checks that do not require credentials, so a run that cannot
/// possibly succeed is refused before a release is created. Mirroring the
/// Python `main`, which validates provenance, source delivery, packaged
/// identity and the checkout identity before calling `promote`.
pub fn preflight(
    record: &Value,
    gates: &Value,
    assets: &BTreeMap<String, PathBuf>,
    approved: bool,
    source_root: &Path,
) -> Result<()> {
    promotion::validate(record, gates, assets, approved)?;
    let groups = promotion::artifact_groups(record)?;
    let nightly = record["mode"] == "nightly";
    let tag = record["tag"]
        .as_str()
        .ok_or("promotion record has no tag")?;
    let source = record["source_sha"]
        .as_str()
        .ok_or("promotion record has no source")?;
    if nightly {
        // A nightly may only be built from a commit already on the protected
        // main line; otherwise an unreviewed commit could reach the edge channel.
        crate::release_policy::git(
            source_root,
            &["merge-base", "--is-ancestor", "--", source, MAIN_REF],
        )?;
    } else {
        // For a stable release the whole provenance must be derived from the
        // protected checkout: the tag must resolve to a full commit, that
        // commit must be on main, and the manifests at that commit must carry
        // the released version. `stable` re-derives the record from those
        // facts, so a record that merely claims them is refused.
        let derived = crate::release_policy::stable(
            source_root,
            tag,
            MAIN_REF,
            &assets.values().cloned().collect::<Vec<_>>(),
            None,
            record.get("architectures"),
        )?;
        let mut claimed = record.clone();
        claimed
            .as_object_mut()
            .ok_or("promotion record must be an object")?
            .insert("mode".into(), json!("stable"));
        if derived != claimed {
            return Err("stable provenance differs from protected Git source".into());
        }
    }
    for (architecture, group) in &groups {
        let snap = assets
            .get(&group.snap)
            .ok_or("promotion record names a Snap that was not supplied")?;
        // The packaged identity must be read from the artifact. A filename or a
        // record field is not evidence of what will actually be installed.
        promotion::check_snap_identity(snap, record, architecture)?;
        // Source delivery binds the association and archive to the Snap for
        // BOTH modes: a nightly reaches users through the same channel, so an
        // unbound source bundle would ship just as visibly as an unbound stable
        // one.
        crate::source_delivery::check(
            assets.get(&group.association).ok_or("missing source association")?,
            assets.get(&group.source).ok_or("missing source archive")?,
            snap,
        )?;
        if nightly {
            continue;
        }
        // A stable release additionally must carry sources that correspond to
        // the repository state being released.
        crate::stable_source::check(
            source_root,
            record,
            assets.get(&group.source).ok_or("missing source archive")?,
        )?;
    }
    // The checkout must be the exact immutable source being published.
    let head = crate::release_policy::git(source_root, &["rev-parse", "HEAD"])?;
    if head.trim() != record["source_sha"].as_str().unwrap_or_default() {
        return Err("publication checkout is not the checked immutable source".into());
    }
    Ok(())
}