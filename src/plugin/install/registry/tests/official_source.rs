use super::*;
use serde_json::json;
use std::os::unix::fs::PermissionsExt;

fn effective(root: &Path) -> (Vec<Registration>, Option<Problem>) {
    match run(root, Request::EffectiveList, || Ok(()), || Ok(())).unwrap() {
        Outcome::EffectiveList {
            registrations,
            problem,
        } => (registrations, problem),
        _ => panic!("expected effective source inventory"),
    }
}

fn official_inspection(revision: &str, quotes: usize) -> Inspection {
    large(official::origin(), revision, quotes)
}

fn large(origin: Origin, revision: &str, quotes: usize) -> Inspection {
    let mut value = crate::plugin::install::metadata::tests::single();
    if quotes > 0 {
        for index in 0..1900 {
            value["artifacts"][0]["members"].as_array_mut().unwrap().push(json!({"path":format!("data/{index}-{}","\"".repeat(quotes)),"size":1,"sha256":"c".repeat(64),"executable":false}));
        }
    }
    Inspection::from_bytes(origin, revision.into(), value.to_string().into_bytes()).unwrap()
}

#[test]
fn built_in_source_is_inert_permanent_numeric_and_additional_to_32_custom_origins() {
    let root = OwnedRoot::new();
    let sources = root.0.join("sources");
    let (initial, problem) = effective(&sources);
    assert!(problem.is_none());
    assert_eq!(initial.len(), 1);
    assert!(initial[0].is_official());
    assert_eq!(initial[0].display_title(), official::TITLE);
    assert_eq!(initial[0].display_origin(), official::origin());
    assert!(
        !sources.exists(),
        "merely displaying the built-in source must not write or fetch metadata"
    );
    assert!(run(
        &sources,
        Request::Remove(initial[0].clone()),
        || Ok(()),
        || panic!("official removal reached publication")
    )
    .is_err());
    for id in 100..132 {
        run(
            &sources,
            Request::Add(Box::new(inspection(id, 34, "custom", 'a'))),
            || Ok(()),
            || Ok(()),
        )
        .unwrap();
    }
    let (entries, problem) = effective(&sources);
    assert!(problem.is_none());
    assert_eq!(entries.len(), 33);
    assert_eq!(
        entries.iter().filter(|source| source.is_official()).count(),
        1
    );
    assert!(run(
        &sources,
        Request::Add(Box::new(inspection(132, 34, "excess", 'a'))),
        || Ok(()),
        || Ok(())
    )
    .is_err());
    let before = std::fs::read(sources.join("sources.json")).unwrap();
    run(
        &sources,
        Request::Refresh {
            expected: entries[0].clone(),
            inspection: Box::new(official_inspection(&"b".repeat(40), 0)),
        },
        || Ok(()),
        || Ok(()),
    )
    .unwrap();
    assert_eq!(std::fs::read(sources.join("sources.json")).unwrap(), before);
    let (reopened, problem) = effective(&sources);
    assert!(problem.is_none());
    assert_eq!(reopened.len(), 33);
    assert_eq!(reopened[0].snapshot_count(), 1);
    assert!(run(
        &sources,
        Request::Remove(initial[0].clone()),
        || Ok(()),
        || Ok(())
    )
    .is_err());
    std::fs::write(sources.join("official.json"), b"damaged official cache").unwrap();
    // A private mode makes the damaged content, not an inherited
    // group-writable mode, the reason the cache is refused.
    std::fs::set_permissions(
        sources.join("official.json"),
        std::fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    let (unavailable, problem) = effective(&sources);
    assert_eq!(problem, Some(OFFICIAL_STORAGE_PROBLEM));
    assert_eq!(unavailable.len(), 33);
    assert!(unavailable[0].is_official());
    assert_eq!(std::fs::read(sources.join("sources.json")).unwrap(), before);
}

#[test]
fn existing_official_duplicate_does_not_consume_a_custom_slot_or_gain_removal_authority() {
    let root = OwnedRoot::new();
    let mut duplicate = official_source();
    duplicate.token = store::token();
    duplicate.revision = store::token();
    let bytes = serde_json::to_vec(&Document {
        version: 1,
        registrations: vec![duplicate],
    })
    .unwrap();
    std::fs::write(root.0.join("sources.json"), bytes).unwrap();
    // The store refuses a group- or world-writable registry, so the mode is
    // set rather than inherited from the umask.
    std::fs::set_permissions(
        root.0.join("sources.json"),
        std::fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    for id in 100..132 {
        run(
            &root.0,
            Request::Add(Box::new(inspection(id, 34, "custom", 'a'))),
            || Ok(()),
            || Ok(()),
        )
        .unwrap();
    }
    let (entries, problem) = effective(&root.0);
    assert!(problem.is_none());
    assert_eq!(entries.len(), 33);
    assert_eq!(list(&root.0).len(), 33);
    assert!(run(
        &root.0,
        Request::Remove(entries[0].clone()),
        || Ok(()),
        || Ok(())
    )
    .is_err());
    assert_eq!(effective(&root.0).0.len(), 33);
}

#[test]
fn offline_removal_evicts_only_cache_to_fit_reachable_atomic_replacement_headroom() {
    let root = OwnedRoot::new();
    let settings = root.0.join("unrelated-settings");
    std::fs::write(&settings, b"preserve unrelated user settings").unwrap();
    for id in 100..132 {
        run(
            &root.0,
            Request::Add(Box::new(inspection(id, 34, "custom", 'a'))),
            || Ok(()),
            || Ok(()),
        )
        .unwrap();
    }
    let custom = Origin {
        repository_id: 100,
        publisher_id: 34,
        owner: "example".into(),
        repository: "custom".into(),
    };
    for index in 1..=3 {
        let expected = list(&root.0)
            .into_iter()
            .find(|entry| entry.origin.repository_id == 100)
            .unwrap();
        run(
            &root.0,
            Request::Refresh {
                expected,
                inspection: Box::new(large(custom.clone(), &format!("{index:040x}"), 140)),
            },
            || Ok(()),
            || Ok(()),
        )
        .unwrap();
    }
    for index in 1..=4 {
        let expected = effective(&root.0).0.remove(0);
        run(
            &root.0,
            Request::Refresh {
                expected,
                inspection: Box::new(official_inspection(&format!("{index:040x}"), 170)),
            },
            || Ok(()),
            || Ok(()),
        )
        .unwrap();
    }
    let expected = list(&root.0)
        .into_iter()
        .find(|entry| entry.origin.repository_id == 100)
        .unwrap();
    run(
        &root.0,
        Request::Refresh {
            expected,
            inspection: Box::new(large(custom, &format!("{:040x}", 4), 140)),
        },
        || Ok(()),
        || Ok(()),
    )
    .unwrap();
    // Every retained byte above was admitted by the production parser and write
    // path. This is not an impossible hand-written pair of maximum-size files.
    let previous = std::fs::read(root.0.join("sources.json")).unwrap();
    let official = std::fs::read(root.0.join("official.json")).unwrap();
    let before = list(&root.0);
    let expected = before
        .iter()
        .find(|entry| entry.origin.repository_id == 131)
        .unwrap()
        .clone();
    let mut proposed: Document = serde_json::from_slice(&previous).unwrap();
    proposed.registrations.retain(|entry| entry != &expected);
    let directory = data::owned_directory(&root.0).unwrap();
    let allowance = write_limit(&directory, Storage::User, Some(&previous)).unwrap();
    assert!(serde_json::to_vec(&proposed).unwrap().len()>allowance,"fixture must reach the actual removal headroom boundary: user={} official={} allowance={allowance}",previous.len(),official.len());
    run(
        &root.0,
        Request::Remove(expected),
        || Ok(()),
        || {
            let temporary = std::fs::read_dir(&root.0)
                .unwrap()
                .filter_map(|entry| entry.ok())
                .filter(|entry| entry.file_name().to_string_lossy().starts_with("sources-"))
                .map(|entry| entry.metadata().unwrap().len() as usize)
                .sum::<usize>();
            assert!(previous.len() + official.len() + temporary <= MAX_CACHE_BYTES);
            Ok(())
        },
    )
    .unwrap();
    let after = list(&root.0);
    assert_eq!(after.len(), 31);
    assert!(after.iter().all(|entry| before
        .iter()
        .any(|old| old.origin == entry.origin && old.token == entry.token)));
    assert!(
        after
            .iter()
            .map(Registration::snapshot_count)
            .sum::<usize>()
            < before
                .iter()
                .map(Registration::snapshot_count)
                .sum::<usize>()
                - 1,
        "removal must evict cache, not exceed the replacement budget"
    );
    assert_eq!(
        std::fs::read(root.0.join("official.json")).unwrap(),
        official
    );
    assert_eq!(
        std::fs::read(settings).unwrap(),
        b"preserve unrelated user settings"
    );
    assert_eq!(effective(&root.0).0.len(), 32);
}
