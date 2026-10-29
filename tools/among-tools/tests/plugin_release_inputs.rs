//! Byte checks, output preservation and workflow controls of the published
//! release-input preparer. Catalogue selection is covered next to its parser.

use among_tools::{plugin_install::release, storage, Result};
use std::fs;
use std::os::unix::fs::symlink;
use std::path::PathBuf;

fn fixture() -> Result<PathBuf> {
    let root = among_tools::scratch::root()?;
    fs::create_dir_all(&root)?;
    storage::private_directory(&root, "release-input-unit")
}

#[test]
fn exact_byte_checks_reject_changed_size_hash_links_and_non_regular_inputs() -> Result<()> {
    let root = fixture()?;
    let path = root.join("archive");
    fs::write(&path, b"exact package bytes")?;
    let digest = storage::sha256(b"exact package bytes".as_slice())?;
    assert_eq!(
        release::checked_bytes(&path, 19, &digest)?,
        b"exact package bytes"
    );
    assert!(release::checked_bytes(&path, 18, &digest).is_err());
    assert!(release::checked_bytes(&path, 19, &"0".repeat(64)).is_err());
    symlink(&path, root.join("link"))?;
    assert!(release::checked_bytes(&root.join("link"), 19, &digest).is_err());
    assert!(release::checked_bytes(&root, 19, &digest).is_err());
    let fifo = root.join("fifo");
    nix::unistd::mkfifo(&fifo, nix::sys::stat::Mode::S_IRUSR)?;
    assert!(release::checked_bytes(&fifo, 19, &digest).is_err());
    fs::write(&path, b"other package bytes")?;
    assert!(release::checked_bytes(&path, 19, &digest).is_err());
    Ok(())
}

#[test]
fn preparation_refuses_occupied_output_without_network_or_overwriting_evidence() -> Result<()> {
    let root = fixture()?;
    fs::write(root.join("preserve"), b"existing evidence")?;
    assert!(release::prepare(&root).is_err());
    assert_eq!(fs::read(root.join("preserve"))?, b"existing evidence");
    assert!(!root.join("repository.json").exists());
    Ok(())
}

#[test]
fn both_native_workflow_jobs_use_release_binaries_and_keep_required_behaviour_controls(
) -> Result<()> {
    let bytes = include_str!("../../../.github/workflows/managed-transport.yml");
    let workflow: serde_yaml_ng::Value = serde_yaml_ng::from_str(bytes)?;
    let job = &workflow["jobs"]["native-interop"];
    // The companion protocol fixture is built from a source commit, not from
    // the published release; the workflow and the host test must name the
    // same full commit so the fixture hash proves which client was exercised.
    let client = job["env"]["CLIENT_REVISION"]
        .as_str()
        .ok_or("companion revision absent")?;
    assert!(client.len() == 40 && client.bytes().all(|b| b.is_ascii_hexdigit()));
    let host = include_str!("../../../src/app/plugin_tests/resources.rs");
    assert!(host.contains(&format!("const CLIENT: &str = \"{client}\";")));
    let matrix = job["strategy"]["matrix"]["include"]
        .as_sequence()
        .ok_or("matrix absent")?;
    assert_eq!(matrix.len(), 2);
    assert_eq!(matrix[0]["runner"].as_str(), Some("ubuntu-24.04"));
    assert_eq!(matrix[1]["runner"].as_str(), Some("ubuntu-24.04-arm"));
    for control in [
        "prepare-official-plugin-inputs",
        "check-plugin-install",
        "--example managed-status --example managed-pages",
        "real_companion_admits_only_after_review_and_publishes_correlated_status",
        "real_companion_identity_mismatch_never_becomes_admitted",
        "real_companion_closed_target_reply_releases_credit_without_publishing",
        "real_companion_retired_generation_reply_cannot_consume_current_credit",
        "utc tokyo unset empty too_long control",
        "real_resources_retired_reply_cannot_publish_into_reenabled_header",
        "real_sdk_closed_workspace_continuation_fails_without_publishing_empty_success",
        "real_sdk_retired_enumeration_cannot_attach_to_a_new_host_generation",
        "real_resource_meters_review_keyboard_input_and_disabled_restart",
        "test result: ok. 1 passed; 0 failed; 0 ignored;",
    ] {
        assert!(
            bytes.contains(control),
            "mandatory control missing: {control}"
        );
    }
    for obsolete in [
        "306839e144d7a4c98432c6c68dad016c412dd656",
        "AMONG_LEGACY_CLOCK",
        "--bin clock",
    ] {
        assert!(
            !bytes.contains(obsolete),
            "obsolete input remains: {obsolete}"
        );
    }
    Ok(())
}
