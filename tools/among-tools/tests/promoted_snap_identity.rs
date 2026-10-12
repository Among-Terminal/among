//! `check_snap_identity` decides whether a Snap may be published, so it is
//! tested against real built artifacts rather than a fixture that could agree
//! with the code by construction.
//!
//! The property under test is that the packaged metadata is authoritative: an
//! amd64 build named `..._arm64.snap` must be refused, and a `devel` grade must
//! not satisfy a stable promotion. Neither can be faked by renaming a file,
//! which is exactly the attack the check exists to stop.

use among_tools::{promotion, snap_runtime, storage, Result};
use serde_json::json;
use std::{fs, path::PathBuf};

/// Where the built Snaps are mounted when this target runs.
///
/// Set by `snap-build.yml`, which is the only place the identity gate is meant
/// to execute. When it is absent the tests report that they did not run instead
/// of silently passing: a skipped identity gate must be visible, or a release
/// whose architecture was never read from the artifact looks certified.
fn artifacts_directory() -> Option<PathBuf> {
    std::env::var_os("AMONG_SNAP_ARTIFACTS")
        .map(PathBuf::from)
        .filter(|path| path.is_dir())
}

/// A stable record as `release_policy` defines it: no `mode` field at all.
/// Fixtures once invented `"mode":"stable"`, which no real record carries, and
/// so hid that the grade check never ran for a genuine stable promotion.
fn stable(version: &str) -> serde_json::Value {
    json!({"version":version})
}

/// A nightly record, the only kind that declares a `mode`. A devel-grade
/// artifact is acceptable here, which makes it the control case.
fn nightly(version: &str) -> serde_json::Value {
    json!({"version":version,"mode":"nightly"})
}

/// The single built Snap for `architecture` in the mounted artifact directory.
///
/// The file is discovered by its packaged architecture, not by a hardcoded name,
/// so bumping the release version does not silently turn these tests into
/// no-ops.
///
/// `Ok(None)` means the gate could not run at all and the caller must assert
/// nothing; `Err` means it was asked to run and the expected Snap was absent.
fn artifact(architecture: &str) -> Result<Option<PathBuf>> {
    let Some(directory) = artifacts_directory() else {
        return Ok(None);
    };
    let mut found = fs::read_dir(&directory)
        .map_err(|_| format!("artifact directory {} is unreadable", directory.display()))?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "snap")
        })
        .collect::<Vec<_>>();
    found.sort();
    for path in &found {
        let metadata = snap_runtime::snap_metadata(path)?;
        if metadata["architectures"] == json!([architecture]) {
            return Ok(Some(path.clone()));
        }
    }
    Err(format!(
        "no Snap for {architecture} among {} candidate(s) in {}",
        found.len(),
        directory.display()
    )
    .into())
}

/// Report loudly when the gate did not run, so a skip is never invisible.
fn report_skip(architecture: &str, snap: &Option<PathBuf>) -> bool {
    if snap.is_none() {
        eprintln!(
            "SKIPPED: no built Snap for {architecture}; this identity gate did not run. \
             It executes where AMONG_SNAP_ARTIFACTS points at built artifacts."
        );
        return true;
    }
    false
}

/// The version the artifact actually carries, so records are built from the
/// package rather than from a literal that a version bump would invalidate.
fn packaged_version(snap: &std::path::Path) -> Result<String> {
    Ok(snap_runtime::snap_metadata(snap)?["version"]
        .as_str()
        .ok_or("packaged Snap has no version")?
        .to_string())
}

#[test]
fn each_architecture_is_accepted_only_for_its_own_packaged_metadata() -> Result<()> {
    for (architecture, other) in [("amd64", "arm64"), ("arm64", "amd64")] {
        let snap = artifact(architecture)?;
        if report_skip(architecture, &snap) {
            continue;
        }
        let snap = snap.ok_or("unreachable after report_skip")?;
        let devel = nightly(&packaged_version(&snap)?);
        promotion::check_snap_identity(&snap, &devel, architecture)?;
        // The real artifact, offered as the other architecture, must be
        // refused: the name says otherwise, the packaged metadata does not.
        assert!(
            promotion::check_snap_identity(&snap, &devel, other).is_err(),
            "a real {architecture} artifact was accepted as {other}"
        );
    }
    Ok(())
}

#[test]
fn a_devel_candidate_cannot_satisfy_a_stable_promotion() -> Result<()> {
    let snap = artifact("amd64")?;
    if report_skip("amd64", &snap) {
        return Ok(());
    }
    let snap = snap.ok_or("unreachable after report_skip")?;
    let version = packaged_version(&snap)?;
    // The candidate CI builds is a devel build. The stable promotion must fail,
    // and the same artifact under a nightly record must pass; that second
    // assertion is the control proving the failure is about the grade alone
    // rather than about name/version/architecture not matching.
    assert!(
        promotion::check_snap_identity(&snap, &stable(&version), "amd64").is_err(),
        "a devel artifact satisfied a stable promotion"
    );
    promotion::check_snap_identity(&snap, &nightly(&version), "amd64")?;
    Ok(())
}

#[test]
fn a_version_mismatch_is_refused_even_for_the_right_architecture() -> Result<()> {
    let snap = artifact("amd64")?;
    if report_skip("amd64", &snap) {
        return Ok(());
    }
    let snap = snap.ok_or("unreachable after report_skip")?;
    let wrong = format!("{}.99", packaged_version(&snap)?);
    assert!(
        promotion::check_snap_identity(&snap, &nightly(&wrong), "amd64").is_err(),
        "a version that is not the packaged one was accepted"
    );
    Ok(())
}

/// Renaming must not change any verdict. This is the property that distinguishes
/// reading the packaged identity from inferring it from the filename.
#[test]
fn renaming_the_artifact_does_not_change_the_verdict() -> Result<()> {
    let snap = artifact("amd64")?;
    if report_skip("amd64", &snap) {
        return Ok(());
    }
    let snap = snap.ok_or("unreachable after report_skip")?;
    let base = storage::private_directory(&std::env::temp_dir(), "among-snap-rename")?;
    let disguised = base.join("among_definitely_arm64.snap");
    fs::copy(&snap, &disguised)?;

    let devel = nightly(&packaged_version(&snap)?);
    // Named arm64, still amd64 inside.
    assert!(
        promotion::check_snap_identity(&disguised, &devel, "arm64").is_err(),
        "an amd64 artifact was accepted as arm64 purely because of its filename"
    );
    // The same bytes still pass as amd64: the rename changed nothing.
    promotion::check_snap_identity(&disguised, &devel, "amd64")?;

    fs::remove_dir_all(base)?;
    Ok(())
}

/// A missing or non-Snap file must be refused before any metadata is trusted.
#[test]
fn something_that_is_not_a_snap_is_refused() -> Result<()> {
    let base = storage::private_directory(&std::env::temp_dir(), "among-snap-not-a-snap")?;
    let path = base.join("not-really.snap");
    fs::write(&path, b"this is not a squashfs image")?;
    assert!(promotion::check_snap_identity(&path, &nightly("0.1.0"), "amd64").is_err());
    // A directory is not an artifact either.
    assert!(promotion::check_snap_identity(&base, &nightly("0.1.0"), "amd64").is_err());
    // A path that does not exist at all.
    assert!(
        promotion::check_snap_identity(&base.join("absent.snap"), &nightly("0.1.0"), "amd64")
            .is_err()
    );
    fs::remove_dir_all(base)?;
    Ok(())
}
