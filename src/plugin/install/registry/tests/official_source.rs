use super::*;
use serde_json::{json, Value};
use std::os::unix::fs::PermissionsExt;

fn effective(root: &Path) -> (Vec<Registration>, Option<Problem>) {
    match run(root, Request::EffectiveList, || Ok(()), || Ok(())).unwrap() {
        Outcome::EffectiveList {
            registrations,
            problem,
        } => (registrations, problem),
        _ => panic!("expected effective source overlay"),
    }
}

fn metadata(publisher: u64, revision: char) -> Inspection {
    inspection(
        official::REPOSITORY_ID,
        publisher,
        "among-plugins",
        revision,
    )
}

#[test]
fn fresh_omitted_and_corrupt_storage_cannot_revoke_or_persist_official_authority() {
    let root = OwnedRoot::new();
    let sources = root.0.join("sources");
    let (entries, problem) = effective(&sources);
    assert!(problem.is_none());
    assert_eq!(entries, vec![official_source()]);
    assert_eq!(entries[0].origin, official::origin());
    assert_eq!(
        entries[0].display_title(),
        "Official plugins for Among Terminal"
    );
    assert!(entries[0].available().is_ok());
    assert!(list(&sources).is_empty());
    assert!(!sources.exists());
    assert!(run(
        &sources,
        Request::Remove(entries[0].clone()),
        || Ok(()),
        || Ok(())
    )
    .is_err());
    assert!(!sources.exists(), "refused removal must not create storage");

    run(
        &sources,
        Request::Add(Box::new(inspection(12, 34, "custom", 'a'))),
        || Ok(()),
        || Ok(()),
    )
    .unwrap();
    let path = sources.join("sources.json");
    let before = std::fs::read(&path).unwrap();
    let (entries, problem) = effective(&sources);
    assert!(problem.is_none());
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[1..], list(&sources));
    assert_eq!(std::fs::read(&path).unwrap(), before);

    std::fs::remove_file(&path).unwrap();
    assert_eq!(effective(&sources).0, vec![official_source()]);
    assert!(
        !path.exists(),
        "manual omission is not silently repaired on disk"
    );
    for bytes in [
        b"broken user bytes".as_slice(),
        br#"{"version":1,"version":1,"registrations":[]}"#,
    ] {
        std::fs::write(&path, bytes).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        let (entries, problem) = effective(&sources);
        assert_eq!(entries, vec![official_source()]);
        assert!(problem.is_some());
        assert!(run(&sources, Request::List, || Ok(()), || Ok(())).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
    }
    std::fs::remove_file(&path).unwrap();
    let outside = root.0.join("outside");
    std::fs::write(&outside, b"retain outside bytes").unwrap();
    std::os::unix::fs::symlink(&outside, &path).unwrap();
    let (entries, problem) = effective(&sources);
    assert_eq!(entries, vec![official_source()]);
    assert!(problem.is_some());
    assert_eq!(std::fs::read(outside).unwrap(), b"retain outside bytes");
    assert!(std::fs::symlink_metadata(path)
        .unwrap()
        .file_type()
        .is_symlink());
}

#[test]
fn thirty_two_custom_sources_plus_cached_official_keep_every_custom_generation_and_byte() {
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
    let custom = list(&root.0);
    assert_eq!(custom.len(), 32);
    let (entries, problem) = effective(&root.0);
    assert!(problem.is_none());
    assert_eq!(entries.len(), 33);
    assert_eq!(entries[1..], custom);
    let expected = entries[0].clone();
    run(
        &root.0,
        Request::Refresh {
            expected,
            inspection: Box::new(metadata(official::PUBLISHER_ID, 'a')),
        },
        || Ok(()),
        || Ok(()),
    )
    .unwrap();
    assert_eq!(list(&root.0).len(), 33);
    let (entries, problem) = effective(&root.0);
    assert!(problem.is_none());
    assert_eq!(entries.len(), 33);
    assert_eq!(entries[1..], custom);
    assert_eq!(
        entries.iter().filter(|source| source.is_official()).count(),
        1
    );
    assert_eq!(entries[0].snapshot_count(), 1);
    let official = entries[0].clone();
    let before = std::fs::read(root.0.join("sources.json")).unwrap();
    assert_eq!(effective(&root.0).0[0], official);
    assert_eq!(std::fs::read(root.0.join("sources.json")).unwrap(), before);
    match run(&root.0, Request::Cached(official), || Ok(()), || Ok(())).unwrap() {
        Outcome::Cached {
            inspection,
            retrieved_at,
            ..
        } => {
            assert_eq!(inspection.repository_id, official::REPOSITORY_ID);
            assert_eq!(inspection.publisher_id, official::PUBLISHER_ID);
            assert!(retrieved_at > 0);
        }
        _ => panic!("expected retained official metadata"),
    }
    assert!(run(
        &root.0,
        Request::Add(Box::new(inspection(33, 34, "overflow", 'a'))),
        || Ok(()),
        || Ok(())
    )
    .is_err());
    assert_eq!(std::fs::read(root.0.join("sources.json")).unwrap(), before);
}

#[test]
fn official_removal_stale_refresh_and_generic_publisher_approval_never_change_storage() {
    let root = OwnedRoot::new();
    run(
        &root.0,
        Request::Add(Box::new(metadata(official::PUBLISHER_ID, 'a'))),
        || Ok(()),
        || Ok(()),
    )
    .unwrap();
    let old = effective(&root.0).0.remove(0);
    run(
        &root.0,
        Request::Refresh {
            expected: old.clone(),
            inspection: Box::new(metadata(official::PUBLISHER_ID, 'b')),
        },
        || Ok(()),
        || Ok(()),
    )
    .unwrap();
    let current = effective(&root.0).0.remove(0);
    assert_ne!(old.revision, current.revision);
    let before = std::fs::read(root.0.join("sources.json")).unwrap();
    let mut renamed = current.clone();
    renamed.origin.owner = "renamed".into();
    for request in [
        Request::Remove(old.clone()),
        Request::Remove(current.clone()),
        Request::Remove(renamed),
        Request::Refresh {
            expected: old,
            inspection: Box::new(metadata(official::PUBLISHER_ID, 'c')),
        },
        Request::Refresh {
            expected: current.clone(),
            inspection: Box::new(metadata(99, 'c')),
        },
        Request::ApprovePublisher {
            expected: current,
            inspection: Box::new(metadata(99, 'c')),
        },
    ] {
        assert!(run(&root.0, request, || Ok(()), || Ok(())).is_err());
        assert_eq!(std::fs::read(root.0.join("sources.json")).unwrap(), before);
    }
    let fresh = OwnedRoot::new();
    assert!(run(
        &fresh.0,
        Request::Refresh {
            expected: official_source(),
            inspection: Box::new(inspection(12, 34, "other", 'a')),
        },
        || Ok(()),
        || Ok(())
    )
    .is_err());
    assert!(!fresh.0.join("sources.json").exists());
}

#[test]
fn names_and_user_flags_cannot_grant_official_status_or_consume_a_custom_slot() {
    let root = OwnedRoot::new();
    let mut spoof = inspection(12, 34, "among-plugins", 'a');
    spoof.repository.owner = "Among-Terminal".into();
    run(&root.0, Request::Add(Box::new(spoof)), || Ok(()), || Ok(())).unwrap();
    let (entries, problem) = effective(&root.0);
    assert!(problem.is_none());
    assert_eq!(entries.len(), 2);
    assert!(entries[0].is_official());
    assert!(!entries[1].is_official());
    assert_ne!(entries[0].display_title(), entries[1].display_title());
    let path = root.0.join("sources.json");
    let mut document: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    document["registrations"][0]["official"] = json!(true);
    let bytes = document.to_string().into_bytes();
    std::fs::write(&path, &bytes).unwrap();
    let (entries, problem) = effective(&root.0);
    assert_eq!(entries, vec![official_source()]);
    assert!(problem.is_some());
    assert_eq!(std::fs::read(path).unwrap(), bytes);
}

#[test]
fn transferred_cached_official_is_unavailable_without_blocking_custom_sources_or_adopting_the_pin()
{
    let root = OwnedRoot::new();
    run(
        &root.0,
        Request::Add(Box::new(metadata(official::PUBLISHER_ID, 'a'))),
        || Ok(()),
        || Ok(()),
    )
    .unwrap();
    run(
        &root.0,
        Request::Add(Box::new(inspection(12, 34, "custom", 'a'))),
        || Ok(()),
        || Ok(()),
    )
    .unwrap();
    let path = root.0.join("sources.json");
    let mut document: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    document["registrations"][0]["origin"]["publisher_id"] = json!(99);
    document["registrations"][0]["snapshots"][0]["origin"]["publisher_id"] = json!(99);
    std::fs::write(&path, document.to_string()).unwrap();
    let before = std::fs::read(&path).unwrap();
    let (entries, problem) = effective(&root.0);
    assert!(problem.is_none());
    assert_eq!(entries.len(), 2);
    assert!(entries[0].available().is_err());
    assert_eq!(entries[0].display_origin(), official::origin());
    assert_eq!(entries[0].origin.publisher_id, 99);
    assert!(run(
        &root.0,
        Request::Cached(entries[0].clone()),
        || Ok(()),
        || Ok(())
    )
    .is_err());
    assert!(metadata(99, 'a').select("sample", "1.0").is_err());
    assert_eq!(std::fs::read(&path).unwrap(), before);
    run(
        &root.0,
        Request::Remove(entries[1].clone()),
        || Ok(()),
        || Ok(()),
    )
    .unwrap();
    assert_eq!(effective(&root.0).0, vec![entries[0].clone()]);
    run(
        &root.0,
        Request::Refresh {
            expected: entries[0].clone(),
            inspection: Box::new(metadata(official::PUBLISHER_ID, 'b')),
        },
        || Ok(()),
        || Ok(()),
    )
    .unwrap();
    let restored = effective(&root.0).0.remove(0);
    assert!(restored.available().is_ok());
    assert_ne!(restored.token, entries[0].token);
    assert_eq!(restored.snapshot_count(), 1);
    assert_eq!(restored.origin.publisher_id, official::PUBLISHER_ID);
}

#[test]
fn actual_effective_list_worker_returns_official_and_reports_corrupt_user_storage() {
    let _serial = super::super::super::source::tests::JOB_TESTS
        .lock()
        .unwrap();
    let root = OwnedRoot::new();
    let path = root.0.join("sources.json");
    std::fs::write(&path, b"broken user document").unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    let job = Job::start(root.0.clone(), Request::EffectiveList).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(outcome) = job.poll() {
            match outcome.unwrap() {
                Outcome::EffectiveList {
                    registrations,
                    problem,
                } => {
                    assert_eq!(registrations, vec![official_source()]);
                    assert!(problem.is_some());
                }
                _ => panic!("expected actual effective-list worker outcome"),
            }
            break;
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(2));
    }
    assert_eq!(std::fs::read(path).unwrap(), b"broken user document");
}

#[test]
fn a_fresh_process_reconstructs_official_authority_after_manual_omission() {
    let root = OwnedRoot::new();
    let sources = root.0.join("sources");
    let first = effective(&sources).0;
    assert_eq!(first, vec![official_source()]);
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "plugin::install::registry::tests::official_source::effective_sources_fresh_process_worker", "--ignored", "--nocapture"])
        .env("AMONG_OFFICIAL_SOURCES_WORKER_ROOT", &sources)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{} {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("PASS official_source_fresh_process"));
    assert!(!sources.exists());
}

#[test]
#[ignore = "called only by the owning fresh-process source authority regression"]
fn effective_sources_fresh_process_worker() {
    let root = PathBuf::from(std::env::var_os("AMONG_OFFICIAL_SOURCES_WORKER_ROOT").unwrap());
    assert!(root.is_absolute() && root.starts_with(std::env::temp_dir()));
    assert!(!root.exists());
    let (entries, problem) = effective(&root);
    assert_eq!(entries, vec![official_source()]);
    assert!(problem.is_none());
    assert!(run(
        &root,
        Request::Remove(entries[0].clone()),
        || Ok(()),
        || Ok(())
    )
    .is_err());
    assert!(!root.exists());
    println!("PASS official_source_fresh_process pid={} application_owned_without_user_storage=true removal_refused=true", std::process::id());
}
