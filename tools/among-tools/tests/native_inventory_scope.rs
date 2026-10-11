use among_tools::{native_inventory as inventory, storage, Result};
use std::fs;
use std::path::Path;

fn workspace() -> Result<std::path::PathBuf> {
    storage::private_directory(&std::env::temp_dir(), "among-inventory-scope")
}

/// Build the two markers that identify a prime as this project's own payload.
fn own_payload(prime: &Path) -> Result<()> {
    fs::create_dir_all(prime.join("bin"))?;
    fs::create_dir_all(prime.join("usr/share/doc/among"))?;
    fs::write(prime.join("bin/among"), b"synthetic executable")?;
    fs::write(
        prime.join("usr/share/doc/among/common-licenses-provider.txt"),
        b"base-files 13ubuntu10\n",
    )?;
    Ok(())
}

/// A prime assembled from unrelated packages must be refused. This is the exact
/// mistake that produced a `GFDL-3` licensing failure for a Snap that never
/// contained `gzip`: a hand-placed deb audited against a matching fake prime.
#[test]
fn a_prime_without_this_projects_own_payload_is_refused() -> Result<()> {
    let root = workspace()?;
    let prime = root.join("prime");
    let debs = root.join("debs");
    fs::create_dir_all(prime.join("usr/bin"))?;
    fs::create_dir(&debs)?;
    fs::write(prime.join("usr/bin/gzip"), b"synthetic")?;
    // An empty output directory is not the point; the refusal must come first.
    fs::create_dir(root.join("output"))?;
    assert!(
        inventory::inventory(&prime, &debs, &root.join("output"), &root).is_err(),
        "a gzip-only prime was accepted as this project's artifact"
    );
    fs::remove_dir_all(root)?;
    Ok(())
}

/// Each marker is required on its own, so a partially populated prime cannot
/// pass by carrying only the executable.
#[test]
fn each_missing_own_payload_marker_is_refused_independently() -> Result<()> {
    for missing in [
        "bin/among",
        "usr/share/doc/among/common-licenses-provider.txt",
    ] {
        let root = workspace()?;
        let prime = root.join("prime");
        let debs = root.join("debs");
        fs::create_dir(&debs)?;
        fs::create_dir(root.join("output"))?;
        own_payload(&prime)?;
        fs::remove_file(prime.join(missing))?;
        assert!(
            inventory::inventory(&prime, &debs, &root.join("output"), &root).is_err(),
            "prime missing {missing} was accepted"
        );
        fs::remove_dir_all(root)?;
    }
    Ok(())
}

/// An empty marker file is not evidence of anything; presence must mean bytes.
#[test]
fn an_empty_own_payload_marker_is_refused() -> Result<()> {
    let root = workspace()?;
    let prime = root.join("prime");
    let debs = root.join("debs");
    fs::create_dir(&debs)?;
    fs::create_dir(root.join("output"))?;
    own_payload(&prime)?;
    fs::write(
        prime.join("usr/share/doc/among/common-licenses-provider.txt"),
        b"",
    )?;
    assert!(inventory::inventory(&prime, &debs, &root.join("output"), &root).is_err());
    fs::remove_dir_all(root)?;
    Ok(())
}

/// The markers are necessary, not sufficient: the exact stage inputs are still
/// inventoried normally once the prime is recognisably ours.
#[test]
fn a_recognised_prime_still_requires_real_stage_inputs() -> Result<()> {
    let root = workspace()?;
    let prime = root.join("prime");
    let debs = root.join("debs");
    fs::create_dir(&debs)?;
    own_payload(&prime)?;
    // Own payload present but no Debian inputs at all: still a refusal, because
    // the audit must be built from the actual staged artifacts.
    let refusal = inventory::inventory(&prime, &debs, &root.join("output"), &root)
        .err()
        .ok_or("an empty input set was accepted")?
        .to_string();
    assert!(
        refusal.contains("no exact staged Debian artifacts"),
        "{refusal}"
    );
    fs::remove_dir_all(root)?;
    Ok(())
}
