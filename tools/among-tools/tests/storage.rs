use among_tools::{promotion_destinations, release_policy, storage};
use serde_json::json;
use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::{symlink, PermissionsExt};
use std::path::{Path, PathBuf};

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let path = storage::private_directory(&std::env::temp_dir(), "among-storage")?;
        Ok(Self(path))
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn traversal_and_symlinks_are_refused() -> among_tools::Result<()> {
    let fixture = Fixture::new()?;
    for name in [
        "",
        "../outside",
        "/absolute",
        "nested/../../outside",
        "./dot",
        "nested/./dot",
        "nested//empty",
    ] {
        assert!(
            storage::safe_path(&fixture.0, Path::new(name)).is_err(),
            "{name}"
        );
    }
    symlink("/", fixture.0.join("link"))?;
    assert!(storage::safe_path(&fixture.0, Path::new("link/file")).is_err());
    assert!(storage::atomic_write(&fixture.0, Path::new("link"), b"changed", 0o600).is_err());
    assert!(fixture.0.join("link").is_symlink());
    Ok(())
}

#[test]
fn atomic_failure_preserves_bytes_and_removes_owned_temporary() -> among_tools::Result<()> {
    let fixture = Fixture::new()?;
    let name = Path::new("settings.json");
    storage::atomic_write(&fixture.0, name, b"original", 0o600)?;
    let failed =
        storage::atomic_write_before_commit(&fixture.0, name, b"substitute", 0o600, || {
            Err("injected write failure".into())
        });
    assert!(failed.is_err());
    assert_eq!(fs::read(fixture.0.join(name))?, b"original");
    assert_eq!(fs::read_dir(&fixture.0)?.count(), 1);
    assert_eq!(
        fs::metadata(fixture.0.join(name))?.permissions().mode() & 0o777,
        0o600
    );
    storage::atomic_write(&fixture.0, name, b"updated", 0o644)?;
    assert_eq!(fs::read(fixture.0.join(name))?, b"updated");
    Ok(())
}

/// The rolling marker counts only when the release is genuinely published.
///
/// A body still in the "rolling" state, or a draft, is what an interrupted run
/// leaves behind. Treating either as success would report a nightly as
/// delivered when it never reached the edge channel.
#[test]
fn a_rolling_or_draft_release_is_not_a_success_marker() -> among_tools::Result<()> {
    for (state, draft, expected) in [
        ("complete", false, true),
        ("complete", true, false),
        ("rolling", false, false),
        ("rolling", true, false),
        ("prepared", false, false),
    ] {
        let release = serde_json::json!({
            "draft": draft,
            "body": serde_json::to_string(&json!({"state":state,"record":{"tag":"nightly-x"}}))?
        });
        let marker = among_tools::promotion_destinations::success_marker(&release);
        assert_eq!(
            marker.is_some(),
            expected,
            "state={state} draft={draft} was treated as a success marker"
        );
    }
    // An unreadable body is not evidence of success either.
    let broken = json!({"draft": false, "body": "not json at all"});
    assert!(promotion_destinations::success_marker(&broken).is_none());
    Ok(())
}

/// A record whose `mode` is present but not exactly "nightly" is refused.
///
/// The caller dispatches on the same field. A non-string mode previously passed
/// nightly validation and was then routed down the stable path, which would
/// publish a nightly build to the stable channel.
#[test]
fn a_present_but_non_nightly_mode_is_refused() -> among_tools::Result<()> {
    let source = "a".repeat(40);
    let nightly_assets = json!({
        "schema": 2,
        "tag": format!("nightly-{source}"),
        "version": format!("n20261011-{}", &source[..12]),
        "source_sha": source,
        "architectures": {
            "amd64": {"snap":"a.snap","source":"a.tar","association":"a.json"},
            "arm64": {"snap":"b.snap","source":"b.tar","association":"b.json"},
        },
        "assets": {
            "a.snap":"0".repeat(64), "a.tar":"2".repeat(64), "a.json":"3".repeat(64),
            "b.snap":"1".repeat(64), "b.tar":"4".repeat(64), "b.json":"5".repeat(64),
        },
    });
    let mut record = nightly_assets.clone();
    record["mode"] = json!("nightly");
    assert!(
        release_policy::validate_record(&record).is_ok(),
        "a well-formed nightly record was refused"
    );
    for bad in [json!("edge"), json!(""), json!(5), json!(null), json!(true)] {
        let mut altered = nightly_assets.clone();
        altered["mode"] = bad.clone();
        assert!(
            release_policy::validate_record(&altered).is_err(),
            "mode {bad} was accepted"
        );
    }
    // Absent mode is the stable shape and must still be governed by the stable
    // tag rule.
    assert!(release_policy::validate_record(&nightly_assets).is_err());
    Ok(())
}

#[test]
fn the_default_lock_refuses_a_concurrent_writer_and_releases_on_drop() -> among_tools::Result<()> {
    let fixture = Fixture::new()?;
    let guard = storage::ExclusiveLock::acquire(&fixture.0, Path::new("lock"))?;

    // The default path must stay fail-fast: a second interactive or install
    // operation on the same state is a conflicting action, not a retry.
    let root = fixture.0.clone();
    let refused = std::thread::spawn(move || {
        storage::ExclusiveLock::acquire(&root, Path::new("lock")).is_err()
    });
    assert!(
        refused.join().map_err(|_| "lock test thread failed")?,
        "the default lock waited instead of refusing a concurrent writer"
    );
    drop(guard);
    let _next = storage::ExclusiveLock::acquire(&fixture.0, Path::new("lock"))?;
    Ok(())
}

#[test]
fn a_publication_lock_waits_for_the_holder_and_then_proceeds() -> among_tools::Result<()> {
    let fixture = Fixture::new()?;
    let guard = storage::ExclusiveLock::acquire(&fixture.0, Path::new("lock"))?;

    // The barrier proves the waiter actually reached the lock attempt before
    // the holder is released. Without it, a thread that had not yet started
    // would let the timing assertion below pass without any contention.
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let root = fixture.0.clone();
    let rendezvous = barrier.clone();
    let (sender, receiver) = std::sync::mpsc::channel();
    let waiter = std::thread::spawn(move || {
        rendezvous.wait();
        let acquired = storage::ExclusiveLock::acquire_waiting(
            &root,
            Path::new("lock"),
            std::time::Duration::from_secs(30),
        )
        .is_ok();
        let _ = sender.send(acquired);
        acquired
    });

    barrier.wait();
    // While the lock is held the waiter must still be blocked.
    assert!(
        receiver
            .recv_timeout(std::time::Duration::from_millis(500))
            .is_err(),
        "a waiting publication entered while the lock was held"
    );
    drop(guard);
    assert!(
        waiter.join().map_err(|_| "lock test thread failed")?,
        "the waiting publication never acquired the lock after release"
    );
    Ok(())
}

#[test]
fn deterministic_archive_has_exact_inventory_and_rejects_duplicate_and_link(
) -> among_tools::Result<()> {
    let files = BTreeMap::from([
        ("z/file".into(), b"second".to_vec()),
        ("a/file".into(), b"first".to_vec()),
    ]);
    let first = storage::deterministic_tar(&files)?;
    assert_eq!(first, storage::deterministic_tar(&files)?);
    let inventory = storage::inspect_tar(&first)?;
    assert_eq!(inventory.len(), 2);
    assert_eq!(inventory["a/file"], storage::sha256(b"first".as_slice())?);
    for entry_type in [tar::EntryType::Regular, tar::EntryType::Symlink] {
        let mut builder = tar::Builder::new(Vec::new());
        for _ in 0..2 {
            let mut header = tar::Header::new_gnu();
            header.set_size(0);
            header.set_mode(0o644);
            header.set_entry_type(entry_type);
            header.set_cksum();
            builder.append_data(&mut header, "same", &[][..])?;
        }
        assert!(storage::inspect_tar(&builder.into_inner()?).is_err());
    }
    assert!(storage::inspect_tar(b"not an archive").is_err());
    Ok(())
}

#[test]
fn json_rejects_duplicate_keys_nested_duplicates_and_malformed_input() -> among_tools::Result<()> {
    for bytes in [
        b"{bad}".as_slice(),
        br#"{"schema":1,"schema":2}"#,
        br#"{"nested":{"a":1,"a":2}}"#,
        b"{} trailing",
    ] {
        assert!(storage::parse_json(bytes).is_err());
    }
    assert_eq!(
        storage::parse_json(br#"{"a":[true,null,1,"s"]}"#)?["a"][3],
        "s"
    );
    assert_eq!(
        storage::sha256(b"abc".as_slice())?,
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
    Ok(())
}
#[test]
fn parallel_private_roots_never_merge_fixture_files_or_touch_preexisting_state(
) -> among_tools::Result<()> {
    use std::collections::BTreeSet;
    let root =
        among_tools::storage::private_directory(&std::env::temp_dir(), "among-private-root-test")?;
    let original = root.join("unrelated");
    std::fs::write(&original, b"must survive")?;
    let mut threads = Vec::new();
    for index in 0..32 {
        let root = root.clone();
        threads.push(std::thread::spawn(
            move || -> std::io::Result<std::path::PathBuf> {
                let directory = among_tools::storage::private_directory(&root, "fixture")
                    .map_err(|error| std::io::Error::other(error.to_string()))?;
                std::fs::write(directory.join("marker"), index.to_string())?;
                Ok(directory)
            },
        ));
    }
    let directories = threads
        .into_iter()
        .map(|thread| {
            thread
                .join()
                .map_err(|_| "private-root worker panicked")
                .and_then(|result| result.map_err(|_| "private-root worker failed"))
        })
        .collect::<std::result::Result<Vec<_>, _>>()?;
    assert_eq!(directories.iter().collect::<BTreeSet<_>>().len(), 32);
    for (index, directory) in directories.into_iter().enumerate() {
        assert_eq!(
            std::fs::read_to_string(directory.join("marker"))?,
            index.to_string()
        );
        assert_eq!(
            std::fs::metadata(&directory)?.permissions().mode() & 0o777,
            0o700
        );
    }
    assert_eq!(std::fs::read(original)?, b"must survive");
    assert!(among_tools::storage::private_directory(&root, "../escape").is_err());
    std::fs::remove_dir_all(root)?;
    Ok(())
}
