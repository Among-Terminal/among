use super::*;
use serde_json::json;

fn repository(publisher: u64) -> Value {
    json!({"id":official::REPOSITORY_ID,"owner":{"login":"Among-Terminal","id":publisher},
        "name":"among-plugins","private":false,"default_branch":"main"})
}

fn selected_official(metadata: &Value) -> Selection {
    Inspection::from_bytes(
        official::origin(),
        "a".repeat(40),
        metadata.to_string().into_bytes(),
    )
    .unwrap()
    .select("sample", "1.0")
    .unwrap()
}

fn route(metadata: Value, archive: Vec<u8>, request: &str) -> Reply {
    let old = format!("/{}/", official::REPOSITORY_ID);
    let translated = request.replace(&old, "/12/");
    let mut reply = package_route(metadata, archive, &translated);
    for (_, value) in &mut reply.headers {
        *value = value.replace("/12/", &old);
    }
    reply
}

#[test]
fn actual_official_numeric_refresh_rejects_transfer_before_fetching_any_catalogue() {
    let _serial = JOB_TESTS.lock().unwrap();
    let transferred = Arc::new(AtomicBool::new(false));
    let server = Server::new({
        let transferred = transferred.clone();
        move |request| {
            let path = request.split_whitespace().nth(1).unwrap();
            if path == format!("/repositories/{}", official::REPOSITORY_ID) {
                Reply::json(repository(if transferred.load(Ordering::Acquire) {
                    99
                } else {
                    official::PUBLISHER_ID
                }))
            } else if path.ends_with("/commits/main") {
                Reply::json(json!({"sha":"a".repeat(40)}))
            } else if path.ends_with("/among-plugins.json") {
                Reply::json(super::super::super::metadata::tests::single())
            } else {
                panic!("unexpected official refresh path: {path}");
            }
        }
    });
    let root = super::super::super::package::tests::OwnedRoot::new();
    let initial = server.refresh(official::REPOSITORY_ID).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    let inspection = loop {
        if let Some(outcome) = initial.poll() {
            break outcome.unwrap();
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(2));
    };
    super::super::super::registry::run(
        &root.0,
        super::super::super::registry::Request::Add(Box::new(inspection)),
        || Ok(()),
        || Ok(()),
    )
    .unwrap();
    let before = std::fs::read(root.0.join("official.json")).unwrap();
    assert!(!root.0.join("sources.json").exists());
    assert_eq!(server.requests().len(), 3);
    transferred.store(true, Ordering::Release);
    let refresh = server.refresh(official::REPOSITORY_ID).unwrap();
    loop {
        if let Some(outcome) = refresh.poll() {
            assert!(outcome.unwrap_err().0.contains("pinned identity"));
            break;
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(2));
    }
    assert_eq!(
        server.requests().len(),
        4,
        "transferred publisher cannot fetch catalogue bytes"
    );
    assert_eq!(std::fs::read(root.0.join("official.json")).unwrap(), before);
    assert!(!root.0.join("sources.json").exists());
}

#[test]
fn actual_official_install_worker_refuses_live_origin_mismatch_before_staging() {
    let _serial = JOB_TESTS.lock().unwrap();
    let (metadata, _) = super::super::super::package::tests::fixture();
    let mut wrong_id = repository(official::PUBLISHER_ID);
    wrong_id["id"] = json!(12);
    let mut private = repository(official::PUBLISHER_ID);
    private["private"] = json!(true);
    for document in [repository(99), wrong_id, private] {
        let server = Server::new(move |_| Reply::json(document.clone()));
        let root = super::super::super::package::tests::OwnedRoot::new();
        let job = server
            .install(selected_official(&metadata), root.0.clone())
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(outcome) = job.poll() {
                assert!(outcome.err().unwrap().0.contains("installation refused"));
                break;
            }
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(server.requests().len(), 1);
        assert!(!root.0.join("installed.json").exists());
        assert!(std::fs::read_dir(&root.0).unwrap().next().is_none());
        let mut forged = selected_official(&metadata);
        forged.source.publisher_id = 99;
        assert!(server.install(forged, root.0.clone()).is_err());
        assert_eq!(server.requests().len(), 1);
    }
}

#[test]
fn actual_official_update_rechecks_publisher_before_commit_and_preserves_old_selection() {
    let _serial = JOB_TESTS.lock().unwrap();
    let (metadata, archive) = super::super::super::package::tests::fixture();
    let transfer_on_download = Arc::new(AtomicBool::new(false));
    let transferred = Arc::new(AtomicBool::new(false));
    let server = Server::new({
        let metadata = metadata.clone();
        let transferred = transferred.clone();
        let transfer_on_download = transfer_on_download.clone();
        move |request| {
            let path = request.split_whitespace().nth(1).unwrap();
            if path == format!("/repositories/{}", official::REPOSITORY_ID) {
                Reply::json(repository(if transferred.load(Ordering::Acquire) {
                    99
                } else {
                    official::PUBLISHER_ID
                }))
            } else {
                if path.starts_with("/github-production-release-asset/")
                    && transfer_on_download.load(Ordering::Acquire)
                {
                    transferred.store(true, Ordering::Release);
                }
                route(metadata.clone(), archive.clone(), request)
            }
        }
    });
    let root = super::super::super::package::tests::OwnedRoot::new();
    install_at(&server, selected_official(&metadata), &root.0).unwrap();
    let entry = crate::plugin::inventory::read(&root.0).unwrap().remove(0);
    assert_eq!(entry.receipt.repository_id, official::REPOSITORY_ID);
    assert_eq!(
        entry.receipt.source.as_ref().unwrap().publisher_id,
        official::PUBLISHER_ID
    );
    let before = std::fs::read(root.0.join("installed.json")).unwrap();
    let payload = std::fs::read(entry.receipt.package_path(&root.0).join("bin/plugin")).unwrap();
    std::fs::write(
        root.0.join("managed-settings-sentinel"),
        b"keep settings bytes",
    )
    .unwrap();
    transfer_on_download.store(true, Ordering::Release);
    let update = selected_official(&metadata)
        .for_update(&entry, None)
        .unwrap();
    let job = server.install(update, root.0.clone()).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(outcome) = job.poll() {
            assert!(outcome.err().unwrap().0.contains("installation refused"));
            break;
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(transferred.load(Ordering::Acquire));
    assert_eq!(
        std::fs::read(root.0.join("installed.json")).unwrap(),
        before
    );
    assert_eq!(
        std::fs::read(entry.receipt.package_path(&root.0).join("bin/plugin")).unwrap(),
        payload
    );
    assert_eq!(
        std::fs::read(root.0.join("managed-settings-sentinel")).unwrap(),
        b"keep settings bytes"
    );
    assert!(!entry
        .receipt
        .package_path(&root.0)
        .join("executed")
        .exists());
    assert_eq!(
        server
            .requests()
            .iter()
            .filter(|request| request.split_whitespace().nth(1)
                == Some(format!("/repositories/{}", official::REPOSITORY_ID).as_str()))
            .count(),
        4
    );
}
