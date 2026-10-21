use super::*;
use crate::plugin::install::package::tests::{extract_bytes, fixture, OwnedRoot};
use std::os::unix::fs::PermissionsExt;

fn prepared(root: &Path) -> (Transaction, Receipt) {
    let (metadata, bytes) = fixture();
    let mut transaction = Transaction::new(root, 8192, || Ok(())).unwrap();
    extract_bytes(&metadata, &bytes, &mut transaction, || Ok(())).unwrap();
    let receipt = Receipt {
        repository_id: 42,
        owner: "example".into(),
        repository: "sample".into(),
        plugin_id: "sample".into(),
        revision: metadata["revision"].as_str().unwrap().into(),
        manifest_sha256: metadata["manifest_sha256"].as_str().unwrap().into(),
        package_sha256: metadata["artifacts"][0]["sha256"].as_str().unwrap().into(),
        release_id: 7,
        asset_id: 11,
        generation: token(),
        installation: Some(token()),
        source: None,
    };
    (transaction, receipt)
}

#[test]
fn real_store_commit_survives_reopen_and_rejects_overwrite_or_retained_consent() {
    let root = OwnedRoot::new();
    let legacy = root.0.join("legacy");
    std::fs::write(&legacy, b"unrelated executable").unwrap();
    let (transaction, receipt) = prepared(&root.0);
    assert!(
        Transaction::new(&root.0, 1, || Ok(())).is_err(),
        "advisory store operation must serialize cooperating writers"
    );
    assert!(inventory::read(&root.0).unwrap().is_empty());
    let outcome = transaction
        .commit(receipt.clone(), || Ok(()), || Ok(()))
        .unwrap();
    assert_eq!(outcome.receipt, receipt);
    assert_eq!(outcome.durability, Durability::Durable);
    drop(transaction);
    let entries = inventory::read(&root.0).unwrap();
    assert_eq!(entries.len(), 1);
    assert!(entries[0].inspected.is_ok());
    assert_eq!(entries[0].receipt, receipt);
    let mut host = crate::plugin::host::PluginHost::new();
    host.register_managed(entries[0].clone(), Some(&serde_json::json!({"enabled":true,"installation":"0".repeat(64),"settings":{"version":1,"values":{"label":"retained"}}})));
    assert!(!host.desired_enabled("github:42/sample"));
    assert_eq!(host.supervised_count(), 0);
    assert_eq!(
        host.managed_details("github:42/sample").unwrap().1.unwrap()["values"]["label"],
        "retained"
    );
    let before = std::fs::read(root.0.join("installed.json")).unwrap();
    let (transaction, replacement) = prepared(&root.0);
    assert!(transaction
        .commit(replacement, || Ok(()), || Ok(()))
        .is_err());
    assert_eq!(
        before,
        std::fs::read(root.0.join("installed.json")).unwrap()
    );
    assert_eq!(std::fs::read(legacy).unwrap(), b"unrelated executable");
}

#[test]
fn cancellation_and_external_selection_edits_do_not_publish_a_selected_install() {
    let root = OwnedRoot::new();
    let (transaction, receipt) = prepared(&root.0);
    assert_eq!(
        transaction
            .commit(
                receipt.clone(),
                || Ok(()),
                || Err(Problem("cancelled at commit boundary"))
            )
            .unwrap_err()
            .0,
        "cancelled at commit boundary"
    );
    assert!(inventory::read(&root.0).unwrap().is_empty());
    assert!(
        receipt.package_path(&root.0).exists(),
        "an inert orphan is not an installed selection"
    );
    drop(transaction);
    let (transaction, receipt) = prepared(&root.0);
    let old = inventory::tests::fixture(&root.0);
    let before = std::fs::read(root.0.join("installed.json")).unwrap();
    // edit after the transaction's read/flush, exactly at its authority boundary.
    let external = Receipt {
        repository_id: 43,
        ..old
    };
    let replacement = serde_json::to_vec(&inventory::Selection {
        version: 1,
        installations: vec![external],
    })
    .unwrap();
    assert_eq!(
        transaction
            .commit(
                receipt,
                || Ok(()),
                || {
                    std::fs::write(root.0.join("installed.json"), &replacement).unwrap();
                    Ok(())
                }
            )
            .unwrap_err()
            .0,
        "installed selection changed externally; refusing replacement"
    );
    assert_ne!(
        before,
        std::fs::read(root.0.join("installed.json")).unwrap()
    );
    assert_eq!(
        replacement,
        std::fs::read(root.0.join("installed.json")).unwrap()
    );
}

#[test]
fn final_directory_confirmation_failure_is_a_real_written_but_uncertain_selection() {
    let root = OwnedRoot::new();
    let (transaction, receipt) = prepared(&root.0);
    let outcome = transaction
        .commit_with_confirmation(
            receipt.clone(),
            || Ok(()),
            || Ok(()),
            |_| {
                Err(std::io::Error::other(
                    "controlled directory confirmation failure",
                ))
            },
        )
        .unwrap();
    assert_eq!(outcome.durability, Durability::Uncertain);
    drop(transaction);
    let entries = inventory::read(&root.0).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].receipt, receipt);
    assert!(
        entries[0].inspected.is_ok(),
        "actual generation and selection rename must precede uncertain reporting"
    );
}

#[test]
fn filesystem_obstructions_keep_previous_selection_and_files_intact() {
    let root = OwnedRoot::new();
    let (transaction, receipt) = prepared(&root.0);
    let old = inventory::tests::fixture(&root.0);
    let previous = std::fs::read(root.0.join("installed.json")).unwrap();
    let occupied = receipt.package_path(&root.0);
    std::fs::create_dir_all(&occupied).unwrap();
    std::fs::write(occupied.join("unrelated"), b"do not replace").unwrap();
    assert!(transaction.commit(receipt, || Ok(()), || Ok(())).is_err());
    assert_eq!(
        previous,
        std::fs::read(root.0.join("installed.json")).unwrap()
    );
    assert_eq!(
        std::fs::read(occupied.join("unrelated")).unwrap(),
        b"do not replace"
    );
    assert!(old.package_path(&root.0).exists());
}

#[test]
fn no_follow_store_paths_malformed_records_and_sparse_quota_overflow_are_refused() {
    for target in ["root", "generations", "staging", "install.lock"] {
        let root = OwnedRoot::new();
        let outside = root.0.join("outside");
        std::fs::create_dir(&outside).unwrap();
        let path = root.0.join("managed");
        if target == "root" {
            std::os::unix::fs::symlink(&outside, &path).unwrap();
        } else {
            std::fs::create_dir(&path).unwrap();
            std::os::unix::fs::symlink(&outside, path.join(target)).unwrap();
        }
        assert!(Transaction::new(&path, 1, || Ok(())).is_err(), "{target}");
        assert_eq!(std::fs::read_dir(&outside).unwrap().count(), 0);
    }
    let root = OwnedRoot::new();
    std::fs::set_permissions(&root.0, std::fs::Permissions::from_mode(0o777)).unwrap();
    assert!(Transaction::new(&root.0, 1, || Ok(())).is_err());
    std::fs::set_permissions(&root.0, std::fs::Permissions::from_mode(0o700)).unwrap();
    std::fs::create_dir(root.0.join("generations")).unwrap();
    std::fs::set_permissions(
        root.0.join("generations"),
        std::fs::Permissions::from_mode(0o700),
    )
    .unwrap();
    let sparse = root.0.join("generations/sparse");
    File::create(&sparse).unwrap().set_len(STORE_BYTES).unwrap();
    std::fs::set_permissions(&sparse, std::fs::Permissions::from_mode(0o600)).unwrap();
    assert_eq!(
        Transaction::new(&root.0, 1, || Ok(())).err().unwrap().0,
        "managed package or staging quota would be exceeded"
    );
    std::fs::remove_file(sparse).unwrap();
    let (transaction, receipt) = prepared(&root.0);
    std::fs::write(root.0.join("installed.json"), b"{malformed").unwrap();
    assert!(transaction.commit(receipt, || Ok(()), || Ok(())).is_err());
    assert_eq!(
        std::fs::read(root.0.join("installed.json")).unwrap(),
        b"{malformed"
    );
}
