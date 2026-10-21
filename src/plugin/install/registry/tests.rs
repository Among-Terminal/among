use super::super::package::tests::OwnedRoot;
use super::*;

fn inspection(id: u64, publisher: u64, name: &str, revision: char) -> Inspection {
    Inspection::from_bytes(
        Origin {
            repository_id: id,
            publisher_id: publisher,
            owner: "example".into(),
            repository: name.into(),
        },
        revision.to_string().repeat(40),
        super::super::metadata::tests::single()
            .to_string()
            .into_bytes(),
    )
    .unwrap()
}

fn list(root: &Path) -> Vec<Registration> {
    match run(root, Request::List, || Ok(()), || Ok(())).unwrap() {
        Outcome::List(entries) => entries,
        _ => panic!("expected list"),
    }
}

#[test]
fn committed_sources_reopen_offline_cache_and_removal_preserves_installations() {
    let root = OwnedRoot::new();
    let installed = root.0.join("managed");
    inventory::tests::fixture(&installed);
    let before = std::fs::read(installed.join("installed.json")).unwrap();
    let sources = root.0.join("sources");
    assert!(list(&sources).is_empty());
    assert!(
        !sources.exists(),
        "inspection/listing must not create storage"
    );
    run(
        &sources,
        Request::Add(Box::new(inspection(12, 34, "sample", 'a'))),
        || Ok(()),
        || Ok(()),
    )
    .unwrap();
    let registrations = list(&sources);
    assert_eq!(registrations.len(), 1);
    assert_eq!(registrations[0].snapshot_count(), 1);
    match run(
        &sources,
        Request::Cached(registrations[0].clone()),
        || Ok(()),
        || Ok(()),
    )
    .unwrap()
    {
        Outcome::Cached {
            inspection,
            stale,
            retrieved_at,
        } => {
            assert!(!stale);
            assert_eq!(inspection.repository_id, 12);
            assert!(retrieved_at > 0);
        }
        _ => panic!("expected actual cached metadata"),
    }
    run(
        &sources,
        Request::Remove(registrations[0].clone()),
        || Ok(()),
        || Ok(()),
    )
    .unwrap();
    assert!(list(&sources).is_empty());
    assert_eq!(
        std::fs::read(installed.join("installed.json")).unwrap(),
        before
    );
    assert!(inventory::read(&installed).unwrap()[0].inspected.is_ok());
    assert!(run(
        &sources,
        Request::Refresh {
            expected: registrations[0].clone(),
            inspection: Box::new(inspection(12, 34, "sample", 'b'))
        },
        || Ok(()),
        || Ok(())
    )
    .is_err());
}

#[test]
fn rename_preserves_identity_transfer_requires_explicit_approval_and_stale_refresh_is_refused() {
    let root = OwnedRoot::new();
    run(
        &root.0,
        Request::Add(Box::new(inspection(12, 34, "sample", 'a'))),
        || Ok(()),
        || Ok(()),
    )
    .unwrap();
    let original = list(&root.0).remove(0);
    run(
        &root.0,
        Request::Refresh {
            expected: original.clone(),
            inspection: Box::new(inspection(12, 34, "renamed", 'b')),
        },
        || Ok(()),
        || Ok(()),
    )
    .unwrap();
    let renamed = list(&root.0).remove(0);
    assert_eq!(original.token, renamed.token);
    assert_ne!(original.revision, renamed.revision);
    assert_eq!(renamed.origin.repository, "renamed");
    let before = std::fs::read(root.0.join("sources.json")).unwrap();
    assert!(run(
        &root.0,
        Request::Refresh {
            expected: renamed.clone(),
            inspection: Box::new(inspection(12, 99, "renamed", 'c'))
        },
        || Ok(()),
        || Ok(())
    )
    .is_err());
    assert_eq!(std::fs::read(root.0.join("sources.json")).unwrap(), before);
    run(
        &root.0,
        Request::ApprovePublisher {
            expected: renamed.clone(),
            inspection: Box::new(inspection(12, 99, "renamed", 'c')),
        },
        || Ok(()),
        || Ok(()),
    )
    .unwrap();
    let transferred = list(&root.0).remove(0);
    assert_ne!(renamed.token, transferred.token);
    assert_eq!(transferred.origin.publisher_id, 99);
    assert!(run(&root.0, Request::Remove(renamed), || Ok(()), || Ok(())).is_err());
    assert!(run(
        &root.0,
        Request::Refresh {
            expected: transferred,
            inspection: Box::new(inspection(55, 99, "renamed", 'd'))
        },
        || Ok(()),
        || Ok(())
    )
    .is_err());
    run(
        &root.0,
        Request::Add(Box::new(inspection(55, 99, "renamed", 'd'))),
        || Ok(()),
        || Ok(()),
    )
    .unwrap();
    assert_eq!(
        list(&root.0).len(),
        2,
        "delete/recreate cannot inherit the old numeric identity"
    );
}

#[test]
fn cache_retains_four_snapshots_and_freshness_never_claims_a_future_timestamp_is_fresh() {
    let root = OwnedRoot::new();
    run(
        &root.0,
        Request::Add(Box::new(inspection(12, 34, "sample", 'a'))),
        || Ok(()),
        || Ok(()),
    )
    .unwrap();
    for revision in ['b', 'c', 'd', 'e', 'f'] {
        let expected = list(&root.0).remove(0);
        run(
            &root.0,
            Request::Refresh {
                expected,
                inspection: Box::new(inspection(12, 34, "sample", revision)),
            },
            || Ok(()),
            || Ok(()),
        )
        .unwrap();
    }
    let entry = list(&root.0).remove(0);
    assert_eq!(entry.snapshot_count(), 4);
    assert_eq!(entry.snapshots[0].revision, "f".repeat(40));
    let time = entry.retrieved_at().unwrap();
    assert!(!entry.stale_at(time + STALE_SECONDS - 1));
    assert!(entry.stale_at(time + STALE_SECONDS));
    assert!(entry.stale_at(time - 1));
}

#[test]
fn source_generation_readd_cancellation_external_edits_and_unsafe_storage_do_not_resurrect() {
    let root = OwnedRoot::new();
    let cancelled = run(
        &root.0,
        Request::Add(Box::new(inspection(12, 34, "sample", 'a'))),
        || Ok(()),
        || Err(Problem("cancelled before source commit")),
    );
    assert!(cancelled.is_err());
    assert!(list(&root.0).is_empty());
    run(
        &root.0,
        Request::Add(Box::new(inspection(12, 34, "sample", 'a'))),
        || Ok(()),
        || Ok(()),
    )
    .unwrap();
    let old = list(&root.0).remove(0);
    run(&root.0, Request::Remove(old.clone()), || Ok(()), || Ok(())).unwrap();
    run(
        &root.0,
        Request::Add(Box::new(inspection(12, 34, "sample", 'a'))),
        || Ok(()),
        || Ok(()),
    )
    .unwrap();
    assert_ne!(old.token, list(&root.0)[0].token);
    assert!(run(
        &root.0,
        Request::Refresh {
            expected: old,
            inspection: Box::new(inspection(12, 34, "sample", 'b'))
        },
        || Ok(()),
        || Ok(())
    )
    .is_err());
    let entry = list(&root.0).remove(0);
    assert!(run(
        &root.0,
        Request::Remove(entry),
        || Ok(()),
        || {
            std::fs::write(root.0.join("sources.json"), b"external edit").unwrap();
            Ok(())
        }
    )
    .is_err());
    assert_eq!(
        std::fs::read(root.0.join("sources.json")).unwrap(),
        b"external edit"
    );
    assert!(run(
        &root.0,
        Request::Add(Box::new(inspection(12, 34, "sample", 'a'))),
        || Ok(()),
        || Ok(())
    )
    .is_err());
    let outside = root.0.join("outside");
    std::fs::write(&outside, b"unchanged").unwrap();
    std::fs::remove_file(root.0.join("sources.json")).unwrap();
    std::os::unix::fs::symlink(&outside, root.0.join("sources.json")).unwrap();
    assert!(run(
        &root.0,
        Request::Add(Box::new(inspection(12, 34, "sample", 'a'))),
        || Ok(()),
        || Ok(())
    )
    .is_err());
    assert_eq!(std::fs::read(outside).unwrap(), b"unchanged");
}

#[test]
fn thirty_two_sources_and_total_cache_byte_budget_are_enforced_on_real_files() {
    let root = OwnedRoot::new();
    let catalogue = format!(
        "{}{}",
        " ".repeat(850_000),
        super::super::metadata::tests::single()
    );
    for id in 1..=32 {
        let value = inspection(id, 34, "sample", 'a');
        run(&root.0, Request::Add(Box::new(value)), || Ok(()), || Ok(())).unwrap();
    }
    let entries = list(&root.0);
    assert_eq!(entries.len(), 32);
    let before = std::fs::read(root.0.join("sources.json")).unwrap();
    assert!(run(
        &root.0,
        Request::Add(Box::new(inspection(33, 34, "sample", 'a'))),
        || Ok(()),
        || Ok(())
    )
    .is_err());
    assert_eq!(std::fs::read(root.0.join("sources.json")).unwrap(), before);
    let (mut document, _) = read(&root.0, &|| Ok(())).unwrap();
    for entry in document.registrations.iter_mut().take(9) {
        entry.snapshots[0].catalogue = catalogue.clone();
        entry.snapshots[0].sha256 = inventory::checksum(catalogue.as_bytes()).unwrap();
    }
    let bytes = serde_json::to_vec(&document).unwrap();
    assert!(bytes.len() < MAX_DOCUMENT_BYTES);
    std::fs::write(root.0.join("sources.json"), bytes).unwrap();
    let expected = list(&root.0).pop().unwrap();
    let mut value = inspection(32, 34, "sample", 'b');
    value.catalogue_bytes = catalogue.into_bytes();
    run(
        &root.0,
        Request::Refresh {
            expected,
            inspection: Box::new(value),
        },
        || Ok(()),
        || {
            let actual = std::fs::read_dir(&root.0)
                .unwrap()
                .map(|entry| entry.unwrap().metadata().unwrap().len())
                .sum::<u64>();
            assert!(
                actual <= MAX_CACHE_BYTES as u64,
                "committed cache plus actual flushed temporary must fit the total metadata ceiling"
            );
            Ok(())
        },
    )
    .unwrap();
    assert!(
        std::fs::metadata(root.0.join("sources.json"))
            .unwrap()
            .len()
            <= MAX_CACHE_BYTES as u64
    );
    let entries = list(&root.0);
    assert_eq!(entries.len(), 32);
    assert!(
        entries.iter().any(|entry| entry.snapshot_count() == 0),
        "cache eviction must preserve registrations, not exceed 16 MiB"
    );
}

#[test]
fn cached_bytes_are_revalidated_not_treated_as_installed_or_trusted() {
    let root = OwnedRoot::new();
    run(
        &root.0,
        Request::Add(Box::new(inspection(12, 34, "sample", 'a'))),
        || Ok(()),
        || Ok(()),
    )
    .unwrap();
    let path = root.0.join("sources.json");
    let original: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    for field in ["sha256", "catalogue", "revision"] {
        let mut changed = original.clone();
        changed["registrations"][0]["snapshots"][0][field] = "invalid".into();
        std::fs::write(&path, changed.to_string()).unwrap();
        assert!(
            run(&root.0, Request::List, || Ok(()), || Ok(())).is_err(),
            "{field}"
        );
    }
    std::fs::write(&path, original.to_string()).unwrap();
    let expected = list(&root.0).remove(0);
    let mut fake = inspection(12, 34, "sample", 'b');
    fake.catalogue.plugins[0].versions[0].manifest.name = "forged preview".into();
    assert!(run(
        &root.0,
        Request::Refresh {
            expected,
            inspection: Box::new(fake)
        },
        || Ok(()),
        || Ok(())
    )
    .is_err());
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&std::fs::read(&path).unwrap()).unwrap(),
        original
    );
}

#[test]
fn actual_source_worker_cancellation_before_commit_and_too_late_after_commit_are_distinct() {
    let _serial = super::super::source::tests::JOB_TESTS.lock().unwrap();
    let root = OwnedRoot::new();
    let mut value = inspection(12, 34, "sample", 'a');
    value.catalogue_bytes = format!(
        "{}{}",
        " ".repeat(900_000),
        super::super::metadata::tests::single()
    )
    .into_bytes();
    let job = Job::start(root.0.clone(), Request::Add(Box::new(value))).unwrap();
    assert!(job.cancel());
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(outcome) = job.poll() {
            assert!(matches!(
                outcome,
                Err(Problem("source operation cancelled"))
            ));
            break;
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(list(&root.0).is_empty());
    let job = Job::start(
        root.0.clone(),
        Request::Add(Box::new(inspection(12, 34, "sample", 'b'))),
    )
    .unwrap();
    loop {
        if let Some(outcome) = job.poll() {
            assert!(matches!(
                outcome,
                Ok(Outcome::Changed {
                    durability: Durability::Durable,
                    ..
                })
            ));
            break;
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(!job.cancel());
    assert_eq!(list(&root.0).len(), 1);
}

#[test]
fn abandoned_owned_temporaries_are_cleaned_but_links_and_unrelated_files_are_not() {
    use std::os::unix::fs::PermissionsExt;
    let root = OwnedRoot::new();
    let abandoned = root.0.join(format!("sources-{}", "a".repeat(64)));
    std::fs::write(&abandoned, b"incomplete own source document").unwrap();
    std::fs::set_permissions(&abandoned, std::fs::Permissions::from_mode(0o600)).unwrap();
    std::fs::write(root.0.join("unrelated"), b"preserve").unwrap();
    run(
        &root.0,
        Request::Add(Box::new(inspection(12, 34, "sample", 'a'))),
        || Ok(()),
        || Ok(()),
    )
    .unwrap();
    assert!(!abandoned.exists());
    assert_eq!(
        std::fs::read(root.0.join("unrelated")).unwrap(),
        b"preserve"
    );
    let linked = root.0.join(format!("sources-{}", "b".repeat(64)));
    std::os::unix::fs::symlink(root.0.join("unrelated"), &linked).unwrap();
    let before = std::fs::read(root.0.join("sources.json")).unwrap();
    assert!(run(
        &root.0,
        Request::Remove(list(&root.0).remove(0)),
        || Ok(()),
        || Ok(())
    )
    .is_err());
    assert_eq!(std::fs::read(root.0.join("sources.json")).unwrap(), before);
    assert_eq!(
        std::fs::read(root.0.join("unrelated")).unwrap(),
        b"preserve"
    );
    assert!(std::fs::symlink_metadata(linked)
        .unwrap()
        .file_type()
        .is_symlink());
}

#[test]
fn source_directory_replacement_and_uncertain_final_confirmation_preserve_truthful_authority() {
    use std::os::unix::fs::PermissionsExt;
    let root = OwnedRoot::new();
    let sources = root.0.join("sources");
    run(
        &sources,
        Request::Add(Box::new(inspection(12, 34, "sample", 'a'))),
        || Ok(()),
        || Ok(()),
    )
    .unwrap();
    let expected = list(&sources).remove(0);
    let previous = std::fs::read(sources.join("sources.json")).unwrap();
    let retired = root.0.join("retired");
    assert!(run(
        &sources,
        Request::Remove(expected),
        || Ok(()),
        || {
            std::fs::rename(&sources, &retired).unwrap();
            std::fs::create_dir(&sources).unwrap();
            std::fs::set_permissions(&sources, std::fs::Permissions::from_mode(0o700)).unwrap();
            std::fs::write(sources.join("sources.json"), &previous).unwrap();
            std::fs::set_permissions(
                sources.join("sources.json"),
                std::fs::Permissions::from_mode(0o600),
            )
            .unwrap();
            Ok(())
        }
    )
    .is_err());
    assert_eq!(
        std::fs::read(sources.join("sources.json")).unwrap(),
        previous
    );
    assert_eq!(
        std::fs::read(retired.join("sources.json")).unwrap(),
        previous
    );
    let directory = store::open_root(&sources).unwrap();
    let bytes = serde_json::to_vec(&Document {
        version: 1,
        registrations: Vec::new(),
    })
    .unwrap();
    let durability = save_with_confirmation(
        &sources,
        &directory,
        Some(previous),
        &bytes,
        &|| Ok(()),
        || Ok(()),
        |_| {
            Err(std::io::Error::other(
                "controlled final source directory confirmation failure",
            ))
        },
    )
    .unwrap();
    assert_eq!(durability, Durability::Uncertain);
    assert!(
        list(&sources).is_empty(),
        "actual authoritative rename precedes uncertain reporting"
    );
    assert_eq!(std::fs::read(sources.join("sources.json")).unwrap(), bytes);
}

#[test]
fn an_owned_child_inheriting_a_lock_cannot_make_a_sequential_source_edit_fail() {
    use std::os::unix::process::CommandExt;
    let root = OwnedRoot::new();
    let directory = store::open_root(&root.0).unwrap();
    let lock = store::file(&directory, "sources.lock", false).unwrap();
    assert_eq!(
        unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) },
        0
    );
    let fd = lock.as_raw_fd();
    let mut command = std::process::Command::new("/usr/bin/sleep");
    command.arg("1");
    unsafe {
        command.pre_exec(move || {
            if libc::fcntl(fd, libc::F_SETFD, 0) < 0 {
                Err(std::io::Error::last_os_error())
            } else {
                Ok(())
            }
        });
    }
    let mut child = command.spawn().unwrap();
    drop(lock);
    let result = run(
        &root.0,
        Request::Add(Box::new(inspection(12, 34, "sample", 'a'))),
        || Ok(()),
        || Ok(()),
    );
    assert!(child.wait().unwrap().success());
    assert!(result.is_ok());
    assert_eq!(list(&root.0).len(), 1);
    let lock = store::file(&directory, "sources.lock", false).unwrap();
    lock_registry(&lock, &|| Ok(())).unwrap();
    let checks = std::cell::Cell::new(0);
    assert!(run(
        &root.0,
        Request::Remove(list(&root.0).remove(0)),
        || {
            checks.set(checks.get() + 1);
            if checks.get() > 3 {
                Err(Problem("cancelled during bounded source lock wait"))
            } else {
                Ok(())
            }
        },
        || Ok(())
    )
    .is_err());
    assert_eq!(list(&root.0).len(), 1);
}
