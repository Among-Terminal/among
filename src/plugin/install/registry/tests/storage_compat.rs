use super::*;
use serde_json::Value;
use std::os::unix::fs::PermissionsExt;

fn effective(root: &Path) -> (Vec<Registration>, Option<Problem>) {
    match run(root, Request::EffectiveList, || Ok(()), || Ok(())).unwrap() {
        Outcome::EffectiveList {
            registrations,
            problem,
        } => (registrations, problem),
        _ => panic!("expected effective source list"),
    }
}

fn official_metadata(revision: char) -> Inspection {
    inspection(
        official::REPOSITORY_ID,
        official::PUBLISHER_ID,
        "among-plugins",
        revision,
    )
}

fn add_official(root: &Path) {
    run(
        root,
        Request::Add(Box::new(official_metadata('a'))),
        || Ok(()),
        || Ok(()),
    )
    .unwrap();
}

fn assert_cached(root: &Path, source: Registration) {
    assert!(matches!(
        run(root, Request::Cached(source), || Ok(()), || Ok(())).unwrap(),
        Outcome::Cached { .. }
    ));
}

#[test]
fn official_refresh_never_rewrites_full_legacy_registry_or_its_generations() {
    let root = OwnedRoot::new();
    for id in 1..=32 {
        run(
            &root.0,
            Request::Add(Box::new(inspection(id, 34, "custom", 'a'))),
            || Ok(()),
            || Ok(()),
        )
        .unwrap();
    }
    let original = std::fs::read(root.0.join("sources.json")).unwrap();
    let generations = list(&root.0);
    for revision in ['a', 'b', 'c', 'd', 'e'] {
        let expected = effective(&root.0).0.remove(0);
        run(
            &root.0,
            Request::Refresh {
                expected,
                inspection: Box::new(official_metadata(revision)),
            },
            || Ok(()),
            || Ok(()),
        )
        .unwrap();
        assert_eq!(
            std::fs::read(root.0.join("sources.json")).unwrap(),
            original
        );
        assert_eq!(list(&root.0), generations);
        let old_shape: Value = serde_json::from_slice(&original).unwrap();
        assert_eq!(old_shape["version"], 1);
        assert_eq!(old_shape["registrations"].as_array().unwrap().len(), 32);
        assert_cached(&root.0, generations[0].clone());
        let (entries, problem) = effective(&root.0);
        assert!(problem.is_none());
        assert_eq!(entries.len(), 33);
        assert_eq!(entries[1..], generations);
        assert_cached(&root.0, entries[0].clone());
        assert!(entries[0].snapshot_count() <= MAX_SNAPSHOTS);
    }
    let official_before = std::fs::read(root.0.join("official.json")).unwrap();
    run(
        &root.0,
        Request::Refresh {
            expected: generations[0].clone(),
            inspection: Box::new(inspection(1, 34, "custom", 'b')),
        },
        || Ok(()),
        || Ok(()),
    )
    .unwrap();
    assert_eq!(
        std::fs::read(root.0.join("official.json")).unwrap(),
        official_before
    );
    let current = list(&root.0).remove(0);
    assert_ne!(current.revision, generations[0].revision);
    run(&root.0, Request::Remove(current), || Ok(()), || Ok(())).unwrap();
    run(
        &root.0,
        Request::Add(Box::new(inspection(33, 34, "replacement", 'a'))),
        || Ok(()),
        || Ok(()),
    )
    .unwrap();
    assert_eq!(list(&root.0).len(), 32);
    assert_eq!(
        std::fs::read(root.0.join("official.json")).unwrap(),
        official_before
    );
}

#[test]
fn existing_official_registration_is_deduplicated_without_migration_or_cache_loss() {
    for transferred in [false, true] {
        let root = OwnedRoot::new();
        run(
            &root.0,
            Request::Add(Box::new(inspection(12, 34, "custom", 'a'))),
            || Ok(()),
            || Ok(()),
        )
        .unwrap();
        add_official(&root.0);
        let (mut legacy, _) = read(&root.0, &|| Ok(())).unwrap();
        let (mut cached, _) = read_storage(&root.0, Storage::Official, &|| Ok(())).unwrap();
        let mut retained = cached.registrations.pop().unwrap();
        if transferred {
            retained.origin.publisher_id = 99;
            retained.snapshots[0].origin.publisher_id = 99;
        }
        retained.origin.owner = "old-official-label".into();
        legacy.registrations.push(retained.clone());
        let original = serde_json::to_vec_pretty(&legacy).unwrap();
        std::fs::write(root.0.join("sources.json"), &original).unwrap();
        // model a real pre-built-in registration, not a new-format migration.
        std::fs::remove_file(root.0.join("official.json")).unwrap();
        let (entries, problem) = effective(&root.0);
        assert!(problem.is_none());
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0], retained);
        assert_eq!(entries[0].available().is_err(), transferred);
        assert_eq!(
            std::fs::read(root.0.join("sources.json")).unwrap(),
            original
        );
        run(
            &root.0,
            Request::Refresh {
                expected: entries[0].clone(),
                inspection: Box::new(official_metadata('b')),
            },
            || Ok(()),
            || Ok(()),
        )
        .unwrap();
        assert_eq!(
            std::fs::read(root.0.join("sources.json")).unwrap(),
            original
        );
        assert_eq!(list(&root.0)[1], retained);
        let refreshed = effective(&root.0).0.remove(0);
        assert_eq!(refreshed.display_origin(), official::origin());
        assert_eq!(refreshed.origin.publisher_id, official::PUBLISHER_ID);
        assert_ne!(refreshed.revision, retained.revision);
        assert_eq!(refreshed.token == retained.token, !transferred);
        assert_eq!(refreshed.snapshot_count(), if transferred { 1 } else { 2 });
        assert_cached(&root.0, refreshed);
        assert_eq!(effective(&root.0).0.len(), 2);
    }
}

#[test]
fn corrupt_linked_unsafe_or_foreign_official_metadata_does_not_break_custom_operations() {
    for case in [
        "corrupt",
        "duplicate",
        "foreign",
        "symlink",
        "hardlink",
        "permissions",
    ] {
        let root = OwnedRoot::new();
        run(
            &root.0,
            Request::Add(Box::new(inspection(12, 34, "custom", 'a'))),
            || Ok(()),
            || Ok(()),
        )
        .unwrap();
        add_official(&root.0);
        let path = root.0.join("official.json");
        let original = std::fs::read(&path).unwrap();
        let outside = root.0.join("outside");
        std::fs::write(&outside, b"outside is never source data").unwrap();
        match case {
            "corrupt" => std::fs::write(&path, b"broken official document").unwrap(),
            "duplicate" => {
                std::fs::write(&path, br#"{"version":1,"version":1,"registrations":[]}"#).unwrap()
            }
            "foreign" => {
                let mut document: Value = serde_json::from_slice(&original).unwrap();
                document["registrations"][0]["origin"]["repository_id"] = 99.into();
                std::fs::write(&path, document.to_string()).unwrap();
            }
            "symlink" => {
                std::fs::remove_file(&path).unwrap();
                std::os::unix::fs::symlink(&outside, &path).unwrap();
            }
            "hardlink" => {
                std::fs::remove_file(&path).unwrap();
                std::fs::hard_link(&outside, &path).unwrap();
            }
            "permissions" => {
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o666)).unwrap()
            }
            _ => unreachable!(),
        }
        let before = std::fs::read(&path).unwrap();
        let outside_before = std::fs::read(&outside).unwrap();
        let (entries, problem) = effective(&root.0);
        assert_eq!(problem, Some(OFFICIAL_STORAGE_PROBLEM), "{case}");
        assert_eq!(entries.len(), 2, "{case}");
        assert_eq!(entries[0], official_source(), "{case}");
        let expected = list(&root.0).remove(0);
        assert_cached(&root.0, expected.clone());
        let changed = run(
            &root.0,
            Request::Refresh {
                expected,
                inspection: Box::new(inspection(12, 34, "custom", 'b')),
            },
            || Ok(()),
            || Ok(()),
        )
        .unwrap();
        assert!(
            matches!(
                changed,
                Outcome::Changed {
                    problem: Some(OFFICIAL_STORAGE_PROBLEM),
                    ..
                }
            ),
            "{case}"
        );
        let current = list(&root.0).remove(0);
        assert_cached(&root.0, current.clone());
        run(&root.0, Request::Remove(current), || Ok(()), || Ok(())).unwrap();
        assert!(list(&root.0).is_empty(), "{case}");
        assert_eq!(std::fs::read(&path).unwrap(), before, "{case}");
        assert_eq!(std::fs::read(&outside).unwrap(), outside_before, "{case}");
        assert!(
            run(
                &root.0,
                Request::Refresh {
                    expected: official_source(),
                    inspection: Box::new(official_metadata('b'))
                },
                || Ok(()),
                || Ok(())
            )
            .is_err(),
            "{case}"
        );
        assert_eq!(std::fs::read(&path).unwrap(), before, "{case}");
    }
}

#[test]
fn good_official_cache_is_independent_of_corrupt_or_unsafe_user_bytes() {
    for linked in [false, true] {
        let root = OwnedRoot::new();
        let user = root.0.join("sources.json");
        let outside = root.0.join("outside");
        std::fs::write(&outside, b"preserved unsafe user bytes").unwrap();
        if linked {
            std::os::unix::fs::symlink(&outside, &user).unwrap();
        } else {
            std::fs::write(&user, b"preserved corrupt user document").unwrap();
            std::fs::set_permissions(&user, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        let original = std::fs::read(&user).unwrap();
        add_official(&root.0);
        let (entries, problem) = effective(&root.0);
        assert!(problem.is_some());
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].snapshot_count(), 1);
        assert_cached(&root.0, entries[0].clone());
        assert_eq!(std::fs::read(&user).unwrap(), original);
        assert_eq!(
            std::fs::read(&outside).unwrap(),
            b"preserved unsafe user bytes"
        );
        assert_eq!(
            std::fs::symlink_metadata(&user)
                .unwrap()
                .file_type()
                .is_symlink(),
            linked
        );
    }
}

#[test]
fn source_edits_refuse_external_changes_to_the_other_document_before_commit() {
    let root = OwnedRoot::new();
    run(
        &root.0,
        Request::Add(Box::new(inspection(12, 34, "custom", 'a'))),
        || Ok(()),
        || Ok(()),
    )
    .unwrap();
    add_official(&root.0);
    let user = root.0.join("sources.json");
    let official = root.0.join("official.json");
    let user_before = std::fs::read(&user).unwrap();
    let official_before = std::fs::read(&official).unwrap();
    let expected = effective(&root.0).0.remove(0);
    assert!(run(
        &root.0,
        Request::Refresh {
            expected,
            inspection: Box::new(official_metadata('b'))
        },
        || Ok(()),
        || {
            std::fs::write(&user, b"external user edit").unwrap();
            Ok(())
        }
    )
    .is_err());
    assert_eq!(std::fs::read(&official).unwrap(), official_before);
    assert_eq!(std::fs::read(&user).unwrap(), b"external user edit");
    std::fs::write(&user, &user_before).unwrap();
    let expected = list(&root.0).remove(0);
    assert!(run(
        &root.0,
        Request::Refresh {
            expected,
            inspection: Box::new(inspection(12, 34, "custom", 'b'))
        },
        || Ok(()),
        || {
            std::fs::write(&official, b"external official edit").unwrap();
            Ok(())
        }
    )
    .is_err());
    assert_eq!(std::fs::read(&user).unwrap(), user_before);
    assert_eq!(std::fs::read(&official).unwrap(), b"external official edit");
}

#[test]
fn separate_metadata_replacement_counts_both_live_documents_and_actual_temporary() {
    let root = OwnedRoot::new();
    for id in 1..=32 {
        run(
            &root.0,
            Request::Add(Box::new(inspection(id, 34, "custom", 'a'))),
            || Ok(()),
            || Ok(()),
        )
        .unwrap();
    }
    let (mut document, _) = read(&root.0, &|| Ok(())).unwrap();
    let catalogue = format!(
        "{}{}",
        " ".repeat(850_000),
        super::super::super::metadata::tests::single()
    );
    for entry in document.registrations.iter_mut().take(9) {
        entry.snapshots[0].catalogue = catalogue.clone();
        entry.snapshots[0].sha256 = inventory::checksum(catalogue.as_bytes()).unwrap();
    }
    std::fs::write(
        root.0.join("sources.json"),
        serde_json::to_vec(&document).unwrap(),
    )
    .unwrap();
    for (index, revision) in ['a', 'b', 'c', 'd'].into_iter().enumerate() {
        let mut metadata = official_metadata(revision);
        metadata.catalogue_bytes = format!(
            "{}{}",
            " ".repeat(900_000),
            super::super::super::metadata::tests::single()
        )
        .into_bytes();
        metadata.retrieved_at = index as u64 + 1;
        let expected = effective(&root.0).0.remove(0);
        run(
            &root.0,
            Request::Refresh {
                expected,
                inspection: Box::new(metadata),
            },
            || Ok(()),
            || Ok(()),
        )
        .unwrap();
    }
    let official_before = std::fs::read(root.0.join("official.json")).unwrap();
    let mut metadata = inspection(32, 34, "custom", 'b');
    metadata.catalogue_bytes = catalogue.into_bytes();
    let expected = list(&root.0).pop().unwrap();
    run(&root.0, Request::Refresh { expected, inspection: Box::new(metadata) }, || Ok(()), || {
        let actual = std::fs::read_dir(&root.0).unwrap().map(|entry| entry.unwrap().metadata().unwrap().len()).sum::<u64>();
        assert!(actual <= MAX_CACHE_BYTES as u64, "both committed files plus flushed staging must fit the unchanged16MiB ceiling: {actual}");
        Ok(())
    }).unwrap();
    assert_eq!(list(&root.0).len(), 32);
    assert_eq!(
        std::fs::read(root.0.join("official.json")).unwrap(),
        official_before
    );
    assert!(list(&root.0)
        .iter()
        .any(|source| source.snapshot_count() == 0));
}

#[test]
fn cancellation_after_official_temporary_is_flushed_preserves_both_documents() {
    use std::cell::Cell;
    let root = OwnedRoot::new();
    run(
        &root.0,
        Request::Add(Box::new(inspection(12, 34, "custom", 'a'))),
        || Ok(()),
        || Ok(()),
    )
    .unwrap();
    add_official(&root.0);
    let user_before = std::fs::read(root.0.join("sources.json")).unwrap();
    let official_before = std::fs::read(root.0.join("official.json")).unwrap();
    let expected = effective(&root.0).0.remove(0);
    let saw_temporary = Cell::new(false);
    let result = run(
        &root.0,
        Request::Refresh {
            expected,
            inspection: Box::new(official_metadata('b')),
        },
        || {
            let temporary = std::fs::read_dir(&root.0).unwrap().any(|entry| {
                entry
                    .unwrap()
                    .file_name()
                    .to_str()
                    .unwrap()
                    .starts_with("official-")
            });
            if temporary {
                saw_temporary.set(true);
                Err(Problem("source operation cancelled"))
            } else {
                Ok(())
            }
        },
        || panic!("cancellation must precede commitment"),
    );
    assert!(matches!(result, Err(Problem("source operation cancelled"))));
    assert!(
        saw_temporary.get(),
        "control must observe actual staged bytes"
    );
    assert_eq!(
        std::fs::read(root.0.join("sources.json")).unwrap(),
        user_before
    );
    assert_eq!(
        std::fs::read(root.0.join("official.json")).unwrap(),
        official_before
    );
    assert!(!std::fs::read_dir(&root.0).unwrap().any(|entry| entry
        .unwrap()
        .file_name()
        .to_str()
        .unwrap()
        .starts_with("official-")));
}

#[test]
fn uncertain_official_directory_confirmation_does_not_hide_the_actual_new_generation() {
    let root = OwnedRoot::new();
    run(
        &root.0,
        Request::Add(Box::new(inspection(12, 34, "custom", 'a'))),
        || Ok(()),
        || Ok(()),
    )
    .unwrap();
    add_official(&root.0);
    let user_before = std::fs::read(root.0.join("sources.json")).unwrap();
    let (mut document, previous) = read_storage(&root.0, Storage::Official, &|| Ok(())).unwrap();
    let old = document.registrations[0].clone();
    document.registrations[0].revision = "b".repeat(64);
    let current = document.registrations[0].clone();
    let bytes = serde_json::to_vec(&document).unwrap();
    let directory = store::open_root(&root.0).unwrap();
    let durability = save_storage_with_confirmation(
        &root.0,
        &directory,
        (Storage::Official, previous),
        &bytes,
        &|| Ok(()),
        || Ok(()),
        |_| {
            Err(std::io::Error::other(
                "controlled final confirmation failure",
            ))
        },
    )
    .unwrap();
    assert_eq!(durability, Durability::Uncertain);
    assert_eq!(std::fs::read(root.0.join("official.json")).unwrap(), bytes);
    assert_eq!(
        std::fs::read(root.0.join("sources.json")).unwrap(),
        user_before
    );
    assert_eq!(effective(&root.0).0[0], current);
    assert_cached(&root.0, current);
    assert!(run(&root.0, Request::Cached(old), || Ok(()), || Ok(())).is_err());
}

#[test]
fn official_refresh_refuses_an_exhausted_shared_budget_without_evicting_custom_cache() {
    let root = OwnedRoot::new();
    for id in 1..=32 {
        run(
            &root.0,
            Request::Add(Box::new(inspection(id, 34, "custom", 'a'))),
            || Ok(()),
            || Ok(()),
        )
        .unwrap();
    }
    add_official(&root.0);
    let (mut user, _) = read(&root.0, &|| Ok(())).unwrap();
    let user_catalogue = format!(
        "{}{}",
        " ".repeat(900_000),
        super::super::super::metadata::tests::single()
    );
    for entry in user.registrations.iter_mut().take(9) {
        entry.snapshots[0].catalogue = user_catalogue.clone();
        entry.snapshots[0].sha256 = inventory::checksum(user_catalogue.as_bytes()).unwrap();
    }
    let user_bytes = serde_json::to_vec(&user).unwrap();
    assert!(user_bytes.len() < MAX_DOCUMENT_BYTES);
    std::fs::write(root.0.join("sources.json"), &user_bytes).unwrap();
    let (mut official, _) = read_storage(&root.0, Storage::Official, &|| Ok(())).unwrap();
    let catalogue = format!(
        "{}{}",
        "\n".repeat(1_000_000),
        super::super::super::metadata::tests::single()
    );
    let template = official.registrations[0].snapshots[0].clone();
    official.registrations[0].snapshots = ['a', 'b', 'c', 'd']
        .into_iter()
        .enumerate()
        .map(|(index, revision)| Snapshot {
            revision: revision.to_string().repeat(40),
            retrieved_at: index as u64 + 1,
            catalogue: catalogue.clone(),
            sha256: inventory::checksum(catalogue.as_bytes()).unwrap(),
            ..template.clone()
        })
        .collect();
    let official_bytes = serde_json::to_vec(&official).unwrap();
    assert!(official_bytes.len() < MAX_DOCUMENT_BYTES);
    assert!(user_bytes.len() + official_bytes.len() < MAX_CACHE_BYTES);
    std::fs::write(root.0.join("official.json"), &official_bytes).unwrap();
    let mut update = official_metadata('e');
    update.catalogue_bytes = catalogue.into_bytes();
    update.retrieved_at = 5;
    let expected = effective(&root.0).0.remove(0);
    let result = run(
        &root.0,
        Request::Refresh {
            expected,
            inspection: Box::new(update),
        },
        || Ok(()),
        || panic!("unadmitted metadata must not begin commitment"),
    );
    assert!(result.is_err());
    assert_eq!(
        std::fs::read(root.0.join("sources.json")).unwrap(),
        user_bytes
    );
    assert_eq!(
        std::fs::read(root.0.join("official.json")).unwrap(),
        official_bytes
    );
    assert_eq!(list(&root.0).len(), 32);
    for registration in list(&root.0) {
        assert_cached(&root.0, registration);
    }
}
