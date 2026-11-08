//! Byte checks, output preservation and workflow controls of the published
//! release-input preparer. Catalogue selection is covered next to its parser.

use among_tools::{plugin_install::release, storage, Result};
use std::collections::BTreeSet;
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
fn a_candidate_needs_its_complete_assets_and_never_overwrites_existing_output() -> Result<()> {
    let root = fixture()?;
    let assets = root.join("assets");
    fs::create_dir(&assets)?;
    assert!(release::prepare_candidate(&assets, &root.join("empty")).is_err());
    fs::write(assets.join("among-plugins.json"), b"{}")?;
    assert!(release::prepare_candidate(&assets, &root.join("incomplete")).is_err());
    fs::write(root.join("preserve"), b"existing evidence")?;
    assert!(release::prepare_candidate(&assets, &root).is_err());
    assert_eq!(fs::read(root.join("preserve"))?, b"existing evidence");
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
        "prepare-published-plugin-inputs",
        "check-published-prerelease-install",
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

#[test]
fn published_selection_and_lifecycle_keep_separate_required_native_jobs() -> Result<()> {
    let workflow: serde_yaml_ng::Value =
        serde_yaml_ng::from_str(include_str!("../../../.github/workflows/checks.yml"))?;
    let scripts = |job: &serde_yaml_ng::Value| -> Result<String> {
        Ok(job["steps"]
            .as_sequence()
            .ok_or("job steps absent")?
            .iter()
            .filter_map(|step| step["run"].as_str())
            .collect::<Vec<_>>()
            .join("\n"))
    };
    for name in ["native", "published-development"] {
        let job = &workflow["jobs"][name];
        assert_eq!(job["needs"].as_str(), Some("published-plugins"));
        assert!(job["if"].is_null());
        assert!(job["continue-on-error"].is_null());
        assert_eq!(
            job["strategy"]["matrix"]["runner"],
            serde_yaml_ng::to_value(["ubuntu-24.04", "ubuntu-24.04-arm"])?
        );
        let checkout = job["steps"]
            .as_sequence()
            .ok_or("steps absent")?
            .iter()
            .find(|step| {
                step["uses"]
                    .as_str()
                    .is_some_and(|name| name.starts_with("actions/checkout@"))
            })
            .ok_or("checkout absent")?;
        assert_eq!(
            checkout["with"]["ref"].as_str(),
            Some("${{ github.event.pull_request.head.sha || github.sha }}")
        );
    }
    let selection = scripts(&workflow["jobs"]["published-development"])?;
    let lifecycle = scripts(&workflow["jobs"]["native"])?;
    let input_download = workflow["jobs"]["published-development"]["steps"]
        .as_sequence()
        .ok_or("steps absent")?
        .iter()
        .find(|step| {
            step["uses"]
                .as_str()
                .is_some_and(|name| name.starts_with("actions/download-artifact@"))
        })
        .ok_or("published input download absent")?;
    assert_eq!(
        input_download["with"]["name"],
        workflow["jobs"]["published-plugins"]["with"]["artifact"]
    );
    assert!(input_download["with"]["run-id"].is_null());
    assert!(input_download["with"]["repository"].is_null());
    for command in [
        "prepare-published-plugin-inputs",
        "export-plugin-catalogue",
        "check-published-development-install",
        "cargo +1.98.1 test --bin among --locked --no-run --message-format=json",
    ] {
        assert!(
            selection.contains(command),
            "required selection control missing: {command}"
        );
    }
    assert!(!selection.contains("check-published-prerelease-install"));
    assert!(!lifecycle.contains("check-published-development-install"));
    assert!(lifecycle.contains("check-published-prerelease-install"));
    assert!(lifecycle.contains("check-synthetic-plugin-replacement"));
    Ok(())
}

#[test]
fn required_consumers_share_one_published_pin_and_do_not_build_substitute_plugins() -> Result<()> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    for (file, consumer) in [
        ("checks.yml", "native"),
        ("deb-release.yml", "build"),
        ("rpm-release.yml", "build"),
        ("appimage-release.yml", "build"),
        ("snap-build.yml", "snap"),
        ("managed-transport.yml", "native-interop"),
    ] {
        let bytes = fs::read_to_string(root.join(".github/workflows").join(file))?;
        let workflow: serde_yaml_ng::Value = serde_yaml_ng::from_str(&bytes)?;
        assert_eq!(
            workflow["jobs"]["published-plugins"]["uses"].as_str(),
            Some("./.github/workflows/published-plugin-inputs.yml"),
            "{file}"
        );
        assert!(
            workflow["jobs"]["published-plugins"]["if"].is_null(),
            "{file}"
        );
        assert!(
            workflow["jobs"]["published-plugins"]["continue-on-error"].is_null(),
            "{file}"
        );
        assert_eq!(
            workflow["jobs"][consumer]["needs"].as_str(),
            Some("published-plugins"),
            "{file}"
        );
        assert!(bytes.contains("prepare-published-plugin-inputs .github/plugin-acceptance.json") || bytes.contains("prepare-published-plugin-inputs \\\n            .github/plugin-acceptance.json"), "{file}");
        assert!(
            !bytes.contains("434653882"),
            "release ID must live in one pin: {file}"
        );
        assert!(
            !bytes.contains("8835249fbe2edb2a55a10080a4e0479e60de79c77901b4c7ca3c72844738d90a"),
            "catalogue digest must live in one pin: {file}"
        );
        assert!(
            !bytes.contains("uses: ./.github/workflows/plugin-candidate.yml"),
            "{file}"
        );
    }
    let producer = fs::read_to_string(root.join(".github/workflows/published-plugin-inputs.yml"))?;
    assert!(producer.contains("fetch-published-plugin-assets"));
    assert!(producer.contains(".github/plugin-acceptance.json"));
    assert!(!producer.contains("continue-on-error"));
    let checks = fs::read_to_string(root.join(".github/workflows/checks.yml"))?;
    for control in [
        "AMONG_OFFICIAL_PLUGINS=$packages",
        "check-synthetic-plugin-replacement",
        "export-plugin-catalogue",
        "check-published-development-install",
        "check-published-prerelease-install",
        "check-published-preferences-from-cargo",
    ] {
        assert!(
            checks.contains(control),
            "required control absent: {control}"
        );
    }
    Ok(())
}

#[test]
fn failed_observer_uploads_only_unique_curl_diagnostics() -> Result<()> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let producer = fs::read_to_string(root.join(".github/workflows/published-plugin-inputs.yml"))?;
    let workflow: serde_yaml_ng::Value = serde_yaml_ng::from_str(&producer)?;
    assert_eq!(workflow["permissions"]["contents"].as_str(), Some("read"));
    let steps = workflow["jobs"]["observe"]["steps"]
        .as_sequence()
        .ok_or("steps absent")?;
    let uploads = steps
        .iter()
        .filter(|step| {
            step["uses"]
                .as_str()
                .is_some_and(|uses| uses.starts_with("actions/upload-artifact@"))
        })
        .collect::<Vec<_>>();
    let [successful, diagnostics] = uploads.as_slice() else {
        return Err("expected two upload steps".into());
    };
    let successful = *successful;
    let diagnostics = *diagnostics;
    assert!(successful["if"].is_null());
    assert_eq!(
        successful["with"]["name"].as_str(),
        Some("${{ inputs.artifact }}")
    );
    assert_eq!(
        successful["with"]["path"].as_str(),
        Some("${{ runner.temp }}/published-plugin-assets/")
    );
    assert_eq!(
        successful["with"]["if-no-files-found"].as_str(),
        Some("error")
    );
    assert_eq!(successful["with"]["retention-days"].as_u64(), Some(14));
    assert_eq!(diagnostics["if"].as_str(), Some("failure()"));
    assert_eq!(
        steps
            .iter()
            .filter(|step| step["if"].as_str() == Some("failure()"))
            .count(),
        1
    );
    assert!(diagnostics["continue-on-error"].is_null());
    assert_eq!(diagnostics["uses"], successful["uses"]);
    assert_eq!(
        diagnostics["with"]["name"].as_str(),
        Some("${{ inputs.artifact }}-curl-diagnostics")
    );
    assert_eq!(
        diagnostics["with"]["path"].as_str(),
        Some("${{ runner.temp }}/published-plugin-assets/*.curl.log")
    );
    assert_eq!(
        diagnostics["with"]["if-no-files-found"].as_str(),
        Some("ignore")
    );
    assert_eq!(diagnostics["with"]["retention-days"].as_u64(), Some(5));
    assert!(!producer.contains("continue-on-error"));
    let fetch = steps
        .iter()
        .position(|step| {
            step["run"]
                .as_str()
                .is_some_and(|run| run.contains("fetch-published-plugin-assets"))
        })
        .ok_or("fetch step absent")?;
    let diagnostic = steps
        .iter()
        .position(|step| step["name"].as_str() == Some("Upload curl diagnostics on failure"))
        .ok_or("diagnostic step absent")?;
    assert!(fetch < diagnostic);
    let mut artifacts = BTreeSet::new();
    for file in [
        "checks.yml",
        "deb-release.yml",
        "rpm-release.yml",
        "appimage-release.yml",
        "snap-build.yml",
        "managed-transport.yml",
    ] {
        let bytes = fs::read_to_string(root.join(".github/workflows").join(file))?;
        let caller: serde_yaml_ng::Value = serde_yaml_ng::from_str(&bytes)?;
        let artifact = caller["jobs"]["published-plugins"]["with"]["artifact"]
            .as_str()
            .ok_or("caller artifact absent")?;
        assert!(
            artifacts.insert(artifact.to_owned()),
            "duplicate artifact name: {artifact}"
        );
    }
    Ok(())
}
