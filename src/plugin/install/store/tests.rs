use super::*;
use crate::plugin::install::package::tests::{extract_bytes, fixture, OwnedRoot};
use std::os::unix::fs::PermissionsExt;

fn release_origin(repository_id: u64) -> inventory::Provenance {
    inventory::Provenance::Release(inventory::ReleaseOrigin {
        repository_id,
        owner: "example".into(),
        repository: "sample".into(),
        release_id: 7,
        asset_id: 11,
        source: None,
    })
}

fn set_repository(receipt: &mut Receipt, repository_id: u64) {
    inventory::tests::release_of(receipt).repository_id = repository_id;
}

#[test]
fn retained_settings_deletion_requires_absence_and_exclusive_operation_authority() {
    let root = super::super::package::tests::OwnedRoot::new();
    let identity = crate::plugin::manifest::ManagedIdentity::new(12, "sample".into()).unwrap();
    let called = std::cell::Cell::new(false);
    let ticket = super::super::operations::Ticket::acquire(&root.0, identity.clone()).unwrap();
    assert!(while_uninstalled(&root.0, identity.clone(), || called.set(true)).is_err());
    drop(ticket);
    let directory = open_root(&root.0).unwrap();
    let lock = file(&directory, "install.lock", false).unwrap();
    assert_eq!(
        unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) },
        0
    );
    assert!(while_uninstalled(&root.0, identity.clone(), || called.set(true)).is_err());
    drop(lock);
    let (transaction, mut receipt) = prepared(&root.0);
    set_repository(&mut receipt, 12);
    let commit = transaction.commit(receipt, || Ok(()), || Ok(())).unwrap();
    drop(transaction);
    assert!(while_uninstalled(&root.0, identity.clone(), || called.set(true)).is_err());
    assert!(!called.get());
    mutate(
        &root.0,
        &commit.receipt,
        crate::plugin::install::mutation::Action::Uninstall,
        None,
        &|| Ok(()),
        || Ok(()),
    )
    .unwrap();
    while_uninstalled(&root.0, identity, || {
        called.set(true);
        assert!(super::super::operations::Ticket::acquire(
            &root.0,
            commit.receipt.identity().unwrap()
        )
        .is_err());
        let lock = file(&directory, "install.lock", false).unwrap();
        assert_ne!(
            unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) },
            0,
            "the absence check and settings edit are one exclusion interval"
        );
    })
    .unwrap();
    assert!(called.get());
}
fn prepared(root: &Path) -> (Transaction, Receipt) {
    let (metadata, bytes) = fixture();
    let mut transaction = Transaction::new(root, 8192, || Ok(())).unwrap();
    extract_bytes(&metadata, &bytes, &mut transaction, || Ok(())).unwrap();
    let receipt = Receipt {
        origin: release_origin(42),
        plugin_id: "sample".into(),
        revision: metadata["revision"].as_str().unwrap().into(),
        manifest_sha256: metadata["manifest_sha256"].as_str().unwrap().into(),
        package_sha256: metadata["artifacts"][0]["sha256"].as_str().unwrap().into(),
        generation: token(),
        installation: Some(token()),
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
        Transaction::new(&root.0, 1, || Err(Problem(
            "cancelled waiting for held store lock"
        )))
        .is_err(),
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
fn explicit_updates_retain_previous_and_cleanup_waits_for_real_inventory_resource_leases() {
    let root = OwnedRoot::new();
    let (transaction, first) = prepared(&root.0);
    transaction
        .commit(first.clone(), || Ok(()), || Ok(()))
        .unwrap();
    drop(transaction);
    let held = inventory::read(&root.0).unwrap().remove(0);
    let first_path = first.package_path(&root.0);
    let original = std::fs::read(first_path.join("bin/plugin")).unwrap();
    let (transaction, mut second) = prepared(&root.0);
    second.installation = first.installation.clone();
    transaction
        .commit_update(second.clone(), &first, || Ok(()), || Ok(()))
        .unwrap();
    drop(transaction);
    assert_eq!(inventory::read(&root.0).unwrap()[0].receipt, second);
    assert_eq!(
        document(&data::owned_directory(&root.0).unwrap())
            .unwrap()
            .previous,
        vec![first.clone()]
    );
    assert_eq!(
        std::fs::read(first_path.join("bin/plugin")).unwrap(),
        original
    );
    let (transaction, mut third) = prepared(&root.0);
    third.installation = first.installation.clone();
    transaction
        .commit_update(third.clone(), &second, || Ok(()), || Ok(()))
        .unwrap();
    drop(transaction);
    let transaction = Transaction::new(&root.0, 0, || Ok(())).unwrap();
    drop(transaction);
    assert!(
        first_path.exists(),
        "open inventory/resource lease outlives retention eviction"
    );
    let (directory, executable) = held.executable_files().unwrap();
    assert!(executable.metadata().unwrap().is_file());
    drop(directory);
    drop(executable);
    drop(held);
    let transaction = Transaction::new(&root.0, 0, || Ok(())).unwrap();
    drop(transaction);
    assert!(
        !first_path.exists(),
        "only unreferenced sealed generations can be reclaimed"
    );
    assert!(second.package_path(&root.0).exists());
    assert!(third.package_path(&root.0).exists());
    let (transaction, mut stale) = prepared(&root.0);
    stale.installation = first.installation.clone();
    assert!(transaction
        .commit_update(stale, &first, || Ok(()), || Ok(()))
        .is_err());
    assert_eq!(inventory::read(&root.0).unwrap()[0].receipt, third);
}

#[test]
fn root_and_generation_replacement_at_commit_refuse_hidden_or_mixed_selection() {
    for replace_root in [true, false] {
        let outer = OwnedRoot::new();
        let root = outer.0.join("managed");
        let (transaction, receipt) = prepared(&root);
        let outcome = transaction.commit(
            receipt.clone(),
            || Ok(()),
            || {
                if replace_root {
                    std::fs::rename(&root, outer.0.join("retired")).unwrap();
                    std::fs::create_dir(&root).unwrap();
                    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700))
                        .unwrap();
                } else {
                    let published = receipt.package_path(&root);
                    std::fs::rename(&published, published.with_extension("retired")).unwrap();
                    std::fs::create_dir(&published).unwrap();
                    std::fs::set_permissions(&published, std::fs::Permissions::from_mode(0o700))
                        .unwrap();
                }
                Ok(())
            },
        );
        assert!(outcome.is_err());
        assert!(!root.join("installed.json").exists());
    }
}

#[test]
fn startup_worker_cleans_abandoned_stage_and_selection_temps_but_not_unsealed_or_unrelated_files() {
    let _serial = crate::plugin::install::source::tests::JOB_TESTS
        .lock()
        .unwrap();
    let root = OwnedRoot::new();
    let (transaction, receipt) = prepared(&root.0);
    assert!(transaction
        .commit(
            receipt.clone(),
            || Ok(()),
            || Err(Problem("controlled interruption before selection"))
        )
        .is_err());
    drop(transaction);
    let rootfd = data::owned_directory(&root.0).unwrap();
    let abandoned = directory(&rootfd, "staging").unwrap();
    let stage = token();
    self::directory(&abandoned, &stage).unwrap();
    file(&rootfd, &format!("selection-{}", token()), true)
        .unwrap()
        .write_all(b"incomplete")
        .unwrap();
    let unsealed = directory(&rootfd, "generations").unwrap();
    let namespace = self::directory(&unsealed, "999").unwrap();
    let plugins = self::directory(&namespace, "other").unwrap();
    let unrelated = self::directory(&plugins, &"d".repeat(64)).unwrap();
    file(&unrelated, "external", true)
        .unwrap()
        .write_all(b"preserve")
        .unwrap();
    let job = RecoveryJob::start(root.0.clone()).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(outcome) = job.poll() {
            outcome.unwrap();
            break;
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(!receipt.package_path(&root.0).exists());
    assert!(!root.0.join("staging").join(stage).exists());
    assert_eq!(
        std::fs::read(
            root.0
                .join("generations/999/other")
                .join("d".repeat(64))
                .join("external")
        )
        .unwrap(),
        b"preserve"
    );
    assert!(inventory::read(&root.0).unwrap().is_empty());
}

#[test]
fn explicit_recovery_removal_uncertainty_and_reinstall_authority_follow_complete_records() {
    use crate::plugin::install::mutation::Action;
    let root = OwnedRoot::new();
    let (transaction, first) = prepared(&root.0);
    transaction
        .commit(first.clone(), || Ok(()), || Ok(()))
        .unwrap();
    drop(transaction);
    let (transaction, mut second) = prepared(&root.0);
    second.installation = first.installation.clone();
    transaction
        .commit_update(second.clone(), &first, || Ok(()), || Ok(()))
        .unwrap();
    drop(transaction);
    let restore = mutate(
        &root.0,
        &second,
        Action::RestorePrevious,
        None,
        &|| Ok(()),
        || Ok(()),
    )
    .unwrap();
    assert_eq!(restore.receipt, first);
    assert_eq!(inventory::read(&root.0).unwrap()[0].receipt, first);
    assert_eq!(
        inventory::read(&root.0).unwrap()[0].previous,
        Some(second.clone())
    );
    let before = std::fs::read(root.0.join("installed.json")).unwrap();
    assert!(mutate(
        &root.0,
        &second,
        Action::RestorePrevious,
        None,
        &|| Ok(()),
        || Ok(())
    )
    .is_err());
    assert_eq!(
        std::fs::read(root.0.join("installed.json")).unwrap(),
        before
    );
    let outcome = mutate_with_confirmation(
        &root.0,
        &first,
        Action::Uninstall,
        None,
        &|| Ok(()),
        || Ok(()),
        |_| {
            Err(std::io::Error::other(
                "controlled removal directory confirmation failure",
            ))
        },
    )
    .unwrap();
    assert_eq!(outcome.durability, Durability::Uncertain);
    assert!(inventory::read(&root.0).unwrap().is_empty());
    assert!(document(&data::owned_directory(&root.0).unwrap())
        .unwrap()
        .previous
        .is_empty());
    assert!(document(&data::owned_directory(&root.0).unwrap())
        .unwrap()
        .revision
        .is_some());
    assert!(
        first.package_path(&root.0).exists(),
        "committed absence is independent from later cleanup"
    );
    let (transaction, fresh) = prepared(&root.0);
    transaction
        .commit(fresh.clone(), || Ok(()), || Ok(()))
        .unwrap();
    drop(transaction);
    assert_ne!(fresh.installation, first.installation);
    assert!(
        mutate(&root.0, &first, Action::Uninstall, None, &|| Ok(()), || Ok(
            ()
        ))
        .is_err()
    );
    assert_eq!(inventory::read(&root.0).unwrap()[0].receipt, fresh);
}

#[test]
fn removal_needs_actual_stop_ack_and_failed_commit_preserves_complete_selection() {
    use crate::plugin::install::mutation::{Action, Job};
    let _serial = crate::plugin::install::source::tests::JOB_TESTS
        .lock()
        .unwrap();
    let root = OwnedRoot::new();
    let (transaction, receipt) = prepared(&root.0);
    transaction
        .commit(receipt.clone(), || Ok(()), || Ok(()))
        .unwrap();
    drop(transaction);
    let before = std::fs::read(root.0.join("installed.json")).unwrap();
    let job = Job::start(root.0.clone(), receipt.clone(), Action::Uninstall, None).unwrap();
    let mut replacement = crate::plugin::host::PluginHost::new();
    let local = serde_json::json!({"enabled":true,"installation":receipt.installation,"review":receipt.review(Vec::new())});
    replacement.register_managed(inventory::read(&root.0).unwrap().remove(0), Some(&local));
    assert!(!replacement.desired_enabled("github:42/sample"));
    replacement.enable("github:42/sample");
    assert_eq!(
        replacement.supervised_count(),
        0,
        "window replacement cannot restart pending removal even when disable persistence failed"
    );
    assert!(
        Job::start(root.0.clone(), receipt.clone(), Action::Uninstall, None).is_err(),
        "duplicate uninstall cannot invalidate its own earlier stop authority"
    );
    let deadline = Instant::now() + Duration::from_secs(7);
    loop {
        if let Some(outcome) = job.poll() {
            assert!(matches!(outcome,Err(Problem("supervised stop was not confirmed within five seconds; package remains selected"))));
            break;
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(2));
    }
    assert_eq!(
        std::fs::read(root.0.join("installed.json")).unwrap(),
        before
    );
    assert!(mutate(
        &root.0,
        &receipt,
        Action::Uninstall,
        None,
        &|| Ok(()),
        || Err(Problem("controlled failure before removal commit"))
    )
    .is_err());
    assert_eq!(
        std::fs::read(root.0.join("installed.json")).unwrap(),
        before
    );
    assert!(inventory::read(&root.0).unwrap()[0].inspected.is_ok());
    replacement.reconcile_managed(inventory::read(&root.0).unwrap().remove(0), Some(&local));
    assert!(!replacement.desired_enabled("github:42/sample"));
    replacement.enable("github:42/sample");
    assert_eq!(
        replacement.supervised_count(),
        1,
        "failed removal releases its guard, not the user's disabled state"
    );
    replacement.shutdown();
    while !replacement.is_stopped() {
        replacement.poll();
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(2));
    }
}

#[test]
fn real_process_exit_at_publication_boundaries_recovers_complete_old_or_new_without_resurrection() {
    const PHASE: &str = "AMONG_INSTALLER_CRASH_PHASE";
    const ROOT: &str = "AMONG_INSTALLER_CRASH_ROOT";
    if let Ok(phase) = std::env::var(PHASE) {
        let root = PathBuf::from(std::env::var_os(ROOT).unwrap());
        if phase == "recover" {
            let transaction = Transaction::new(&root, 0, || Ok(())).unwrap();
            drop(transaction);
            return;
        }
        if phase.starts_with("remove-") || phase.starts_with("restore-") {
            let current = selection(&open_existing_root(&root).unwrap())
                .unwrap()
                .remove(0);
            let action = if phase.starts_with("remove-") {
                crate::plugin::install::mutation::Action::Uninstall
            } else {
                crate::plugin::install::mutation::Action::RestorePrevious
            };
            mutate_with_confirmation(
                &root,
                &current,
                action,
                None,
                &|| Ok(()),
                || {
                    if phase.ends_with("prepared") {
                        unsafe { libc::_exit(0) }
                    }
                    Ok(())
                },
                |directory| {
                    if phase.ends_with("renamed") {
                        unsafe { libc::_exit(0) }
                    }
                    directory.sync_all()
                },
            )
            .unwrap();
            unsafe { libc::_exit(0) }
        }
        let (metadata, bytes) = fixture();
        let mut transaction = Transaction::new(&root, 8192, || Ok(())).unwrap();
        if phase == "stage" {
            unsafe { libc::_exit(0) }
        }
        extract_bytes(&metadata, &bytes, &mut transaction, || Ok(())).unwrap();
        if phase == "verified" {
            unsafe { libc::_exit(0) }
        }
        let mut receipt = Receipt {
            origin: release_origin(42),
            plugin_id: "sample".into(),
            revision: metadata["revision"].as_str().unwrap().into(),
            manifest_sha256: metadata["manifest_sha256"].as_str().unwrap().into(),
            package_sha256: metadata["artifacts"][0]["sha256"].as_str().unwrap().into(),
            generation: token(),
            installation: Some(token()),
        };
        let expected = if phase.starts_with("update-") {
            Some(
                selection(&open_existing_root(&root).unwrap())
                    .unwrap()
                    .remove(0),
            )
        } else {
            None
        };
        if let Some(expected) = &expected {
            receipt.installation = expected.installation.clone();
        }
        transaction
            .commit_selection(
                receipt,
                expected
                    .as_ref()
                    .map(|expected| (expected, Consent::Inherit)),
                || Ok(()),
                || {
                    if phase.ends_with("published") {
                        unsafe { libc::_exit(0) }
                    }
                    Ok(())
                },
                |directory| {
                    if phase.ends_with("renamed") {
                        unsafe { libc::_exit(0) }
                    }
                    directory.sync_all()
                },
            )
            .unwrap();
        unsafe { libc::_exit(0) }
    }
    for phase in ["stage", "verified", "published", "renamed", "durable"] {
        let root = OwnedRoot::new();
        let original = inventory::tests::fixture(&root.0);
        let run = |phase: &str| {
            std::process::Command::new(std::env::current_exe().unwrap()).args(["--exact","plugin::install::store::tests::real_process_exit_at_publication_boundaries_recovers_complete_old_or_new_without_resurrection","--nocapture","--test-threads=1"]).env(PHASE,phase).env(ROOT,&root.0).output().unwrap()
        };
        let crashed = run(phase);
        assert!(
            crashed.status.success(),
            "{phase}: {}",
            String::from_utf8_lossy(&crashed.stderr)
        );
        let recovered = run("recover");
        assert!(
            recovered.status.success(),
            "{phase}: {}",
            String::from_utf8_lossy(&recovered.stderr)
        );
        let entries = inventory::read(&root.0).unwrap();
        assert_eq!(
            entries.len(),
            if matches!(phase, "renamed" | "durable") {
                2
            } else {
                1
            }
        );
        assert!(entries.iter().all(|entry| entry.inspected.is_ok()));
        assert!(entries.iter().any(|entry| entry.receipt == original));
        assert_eq!(
            std::fs::read_dir(root.0.join("staging")).unwrap().count(),
            0
        );
        println!("PASS actual _exit at {phase}, fresh-process recovery retained only complete committed selections");
    }
    for phase in [
        "update-published",
        "update-renamed",
        "update-durable",
        "remove-prepared",
        "remove-renamed",
        "remove-durable",
        "restore-prepared",
        "restore-renamed",
        "restore-durable",
    ] {
        let root = OwnedRoot::new();
        let (transaction, first) = prepared(&root.0);
        transaction
            .commit(first.clone(), || Ok(()), || Ok(()))
            .unwrap();
        drop(transaction);
        let (transaction, mut second) = prepared(&root.0);
        second.installation = first.installation.clone();
        transaction
            .commit_update(second.clone(), &first, || Ok(()), || Ok(()))
            .unwrap();
        drop(transaction);
        let run = |phase: &str| {
            std::process::Command::new(std::env::current_exe().unwrap()).args(["--exact","plugin::install::store::tests::real_process_exit_at_publication_boundaries_recovers_complete_old_or_new_without_resurrection","--nocapture","--test-threads=1"]).env(PHASE,phase).env(ROOT,&root.0).output().unwrap()
        };
        for step in [phase, "recover"] {
            let output = run(step);
            assert!(
                output.status.success(),
                "{phase}/{step}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        let entries = inventory::read(&root.0).unwrap();
        let before = phase.ends_with("prepared") || phase.ends_with("published");
        if phase.starts_with("remove-") && !before {
            assert!(entries.is_empty());
            assert!(!first.package_path(&root.0).exists());
            assert!(!second.package_path(&root.0).exists());
        } else {
            assert_eq!(entries.len(), 1);
            assert!(entries[0].inspected.is_ok());
            if before {
                assert_eq!(entries[0].receipt, second);
            } else if phase.starts_with("restore-") {
                assert_eq!(entries[0].receipt, first);
            } else {
                assert_ne!(entries[0].receipt.generation, second.generation);
                assert_eq!(entries[0].previous, Some(second.clone()));
            }
        }
        println!("PASS actual _exit at {phase}, fresh-process recovery follows complete update/removal/recovery authority without resurrection");
    }
}

#[test]
#[ignore = "requires an explicitly owned small tmpfs mount in AMONG_INSTALLER_FULL_DISK_ROOT"]
fn real_enospc_before_selection_keeps_previous_installation_and_never_reports_success() {
    use std::io::Write;
    let root = PathBuf::from(
        std::env::var_os("AMONG_INSTALLER_FULL_DISK_ROOT").expect("owned tmpfs fixture root"),
    );
    std::fs::create_dir_all(&root).unwrap();
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
    let old = inventory::tests::fixture(&root);
    let before = std::fs::read(root.join("installed.json")).unwrap();
    let (transaction, receipt) = prepared(&root);
    let filler = root.join("owned-enospc-fixture");
    let mut full = File::create(&filler).unwrap();
    let bytes = vec![0u8; 65536];
    loop {
        if let Err(error) = full.write_all(&bytes) {
            assert_eq!(error.raw_os_error(), Some(libc::ENOSPC));
            break;
        }
    }
    let outcome = transaction.commit(receipt, || Ok(()), || Ok(()));
    assert!(outcome.is_err());
    assert_eq!(std::fs::read(root.join("installed.json")).unwrap(), before);
    assert_eq!(inventory::read(&root).unwrap()[0].receipt, old);
    assert!(inventory::read(&root).unwrap()[0].inspected.is_ok());
    std::fs::remove_file(filler).unwrap();
    drop(transaction);
    println!("PASS real ENOSPC on owned tmpfs after staging and before selection: prior record and usable generation retained, no false success");
}

#[test]
fn detached_supervision_retains_real_process_lease_until_the_child_has_been_reaped() {
    use crate::plugin::host::PluginHost;
    use crate::plugin::install::source::tests::{install_at, package_route, Server};
    use serde_json::json;
    let root = OwnedRoot::new();
    let (metadata, archive) =
        crate::plugin::install::test_managed_package("1.0", "managed-ignore-term", |_| {});
    let server =
        Server::new(move |request| package_route(metadata.clone(), archive.clone(), request));
    let first = install_at(
        &server,
        server
            .inspect(true)
            .unwrap()
            .select("sample", "1.0")
            .unwrap(),
        &root.0,
    )
    .unwrap()
    .receipt;
    let mut host = PluginHost::new();
    host.register_managed(inventory::read(&root.0).unwrap().remove(0),Some(&json!({"enabled":true,"installation":first.installation,"review":first.review(vec!["status".into(),"settings".into()])})));
    let deadline = Instant::now() + Duration::from_secs(5);
    while host.running_generation("github:12/sample").is_none() {
        host.poll();
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(1));
    }
    let pid = host.supervised_pids()[0];
    let (transaction, mut second) = prepared(&root.0);
    set_repository(&mut second, 12);
    second.installation = first.installation.clone();
    transaction
        .commit_update(second.clone(), &first, || Ok(()), || Ok(()))
        .unwrap();
    drop(transaction);
    let (transaction, mut third) = prepared(&root.0);
    set_repository(&mut third, 12);
    third.installation = first.installation.clone();
    transaction
        .commit_update(third, &second, || Ok(()), || Ok(()))
        .unwrap();
    drop(transaction);
    drop(host);
    assert_eq!(
        unsafe { libc::kill(pid as i32, 0) },
        0,
        "actual TERM-ignoring old child remains supervised during stop grace"
    );
    let directory = open_existing_root(&root.0).unwrap();
    let package = data::contained_directory(&directory, &first.generation_directory()).unwrap();
    let lease = data::contained_file(&package, ".among-lease").unwrap();
    assert_ne!(
        unsafe { libc::flock(lease.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) },
        0,
        "the dropped host cannot be the supervisor's last lease owner"
    );
    assert_eq!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::EWOULDBLOCK)
    );
    drop(lease);
    let transaction = Transaction::new(&root.0, 0, || Ok(())).unwrap();
    drop(transaction);
    if unsafe { libc::kill(pid as i32, 0) } == 0 {
        assert!(
            first.package_path(&root.0).exists(),
            "cleanup cannot delete beneath the actual live old child"
        );
    }
    while unsafe { libc::kill(pid as i32, 0) } == 0 {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(2));
    }
    while first.package_path(&root.0).exists() {
        assert!(
            Instant::now() < deadline,
            "cleanup must eventually observe release of the supervisor's final lease"
        );
        let transaction = Transaction::new(&root.0, 0, || Ok(())).unwrap();
        drop(transaction);
        std::thread::sleep(Duration::from_millis(2));
    }
    println!("PASS real TERM-ignoring child retained lease after host drop; cleanup waited for bounded supervisor kill/reap");
}

#[test]
#[ignore = "requires the unprivileged test user to observe a real filesystem permission refusal"]
fn actual_permission_refusal_after_generation_publication_does_not_replace_selection() {
    assert_ne!(
        unsafe { libc::geteuid() },
        0,
        "run owned permission checks as the unprivileged test user"
    );
    let root = OwnedRoot::new();
    let old = inventory::tests::fixture(&root.0);
    let before = std::fs::read(root.0.join("installed.json")).unwrap();
    let (transaction, receipt) = prepared(&root.0);
    std::fs::set_permissions(&root.0, std::fs::Permissions::from_mode(0o500)).unwrap();
    let outcome = transaction.commit(receipt, || Ok(()), || Ok(()));
    std::fs::set_permissions(&root.0, std::fs::Permissions::from_mode(0o700)).unwrap();
    assert!(outcome.is_err());
    assert_eq!(
        std::fs::read(root.0.join("installed.json")).unwrap(),
        before
    );
    assert_eq!(inventory::read(&root.0).unwrap()[0].receipt, old);
    assert!(inventory::read(&root.0).unwrap()[0].inspected.is_ok());
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
    let mut external = old;
    set_repository(&mut external, 43);
    let replacement = serde_json::to_vec(&inventory::Selection {
        version: 1,
        installations: vec![external],
        previous: Vec::new(),
        revision: None,
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
