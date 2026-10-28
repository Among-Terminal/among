use super::*;
use serde_json::json;

#[test]
fn actual_latest_asset_discovery_uses_api_ids_and_reopens_the_same_snapshot_offline() {
    use super::super::super::registry::{self, Outcome, Request};
    let root = super::super::super::package::tests::OwnedRoot::new();
    let (mut metadata, archive) = super::super::super::package::tests::fixture();
    metadata["artifacts"][0]["asset"] = json!("literal package+λ.tar.gz");
    let server = Server::new(move |request| {
        let path = request.split_whitespace().nth(1).unwrap();
        if path == "/repositories/12/releases/latest" {
            let mut document = release_document(&metadata);
            document["id"] = json!(700);
            document["assets"][0]["id"] = json!(111);
            document["assets"][1]["id"] = json!(112);
            return Reply::json(document);
        }
        let request = request
            .replace("/assets/111 ", "/assets/11 ")
            .replace("/assets/112 ", "/assets/12 ");
        package_route(metadata.clone(), archive.clone(), &request)
    });
    let inspection = server.inspect(true).unwrap();
    let snapshot = inspection.release.clone();
    assert_eq!(snapshot.snapshot.id, 700);
    assert_eq!(
        snapshot
            .snapshot
            .asset("literal package+λ.tar.gz")
            .unwrap()
            .id,
        111
    );
    assert_eq!(server.requests().len(), 5);
    registry::run(
        &root.0,
        Request::Add(Box::new(inspection)),
        || Ok(()),
        || Ok(()),
    )
    .unwrap();
    let registrations = match registry::run(&root.0, Request::List, || Ok(()), || Ok(())).unwrap() {
        Outcome::List(entries) => entries,
        _ => panic!("expected retained registration"),
    };
    let cached = match registry::run(
        &root.0,
        Request::Cached(registrations[0].clone()),
        || Ok(()),
        || Ok(()),
    )
    .unwrap()
    {
        Outcome::Cached { inspection, .. } => inspection,
        _ => panic!("expected offline release snapshot"),
    };
    assert_eq!(cached.release, snapshot);
    assert_eq!(server.requests().len(), 5, "cache reopen must stay offline");
    let committed = install_at(
        &server,
        cached.select("sample", "1.0").unwrap(),
        &root.0.join("managed"),
    )
    .unwrap();
    assert_eq!(committed.receipt.release_id, 700);
    assert_eq!(committed.receipt.asset_id, 111);
    assert_eq!(
        committed.receipt.source.as_ref().unwrap().release_binding,
        Some(snapshot.binding)
    );
    assert_eq!(server.requests().len(), 15);
    assert_eq!(
        server
            .requests()
            .iter()
            .filter(|request| request.contains("/releases/assets/112 "))
            .count(),
        1
    );
    assert!(server
        .requests()
        .iter()
        .all(|request| !request.contains("/commits/main")
            && !request.contains("/among-plugins.json ")
            && !request.contains("/releases/700 ")));
    assert!(!committed
        .receipt
        .package_path(&root.0.join("managed"))
        .join("executed")
        .exists());
}

#[test]
fn actual_latest_catalogue_transfer_and_binding_failures_never_fall_back_to_a_branch() {
    for case in [
        "absent-latest",
        "prerelease",
        "mutable",
        "missing-digest",
        "oversized-catalogue",
        "catalogue-digest",
        "catalogue-size",
        "catalogue-truncated",
        "catalogue-redirect",
        "schema-one",
        "embedded-release-id",
        "embedded-asset-id",
        "prefix-name",
        "package-size",
        "package-digest",
        "revision",
    ] {
        let (mut metadata, archive) = super::super::super::package::tests::fixture();
        match case {
            "schema-one" => metadata["schema_version"] = json!(1),
            "embedded-release-id" => metadata["release_id"] = json!(7),
            "embedded-asset-id" => metadata["artifacts"][0]["asset_id"] = json!(11),
            _ => {}
        }
        let server = Server::new(move |request| {
            let path = request.split_whitespace().nth(1).unwrap();
            if case == "revision" && path.starts_with("/repositories/12/git/commits/") {
                return Reply::json(json!({"sha":"b".repeat(40)}));
            }
            let mut reply = package_route(metadata.clone(), archive.clone(), request);
            if path == "/repositories/12/releases/latest" {
                let mut document: Value = serde_json::from_slice(&reply.body).unwrap();
                match case {
                    "absent-latest" => reply.status = 404,
                    "prerelease" => document["prerelease"] = json!(true),
                    "mutable" => document["immutable"] = json!(false),
                    "missing-digest" => document["assets"][2]["digest"] = Value::Null,
                    "oversized-catalogue" => {
                        document["assets"][1]["size"] =
                            json!(super::super::super::metadata::MAX_CATALOGUE_BYTES + 1)
                    }
                    "catalogue-digest" => {
                        document["assets"][1]["digest"] =
                            json!(format!("sha256:{}", "0".repeat(64)))
                    }
                    "catalogue-size" => {
                        document["assets"][1]["size"] = json!(metadata.to_string().len() + 1)
                    }
                    "prefix-name" => {
                        document["assets"][0]["name"] = json!("sample-amd64.tar.gz.backup")
                    }
                    "package-size" => document["assets"][0]["size"] = json!(archive.len() + 1),
                    "package-digest" => {
                        document["assets"][0]["digest"] =
                            json!(format!("sha256:{}", "0".repeat(64)))
                    }
                    _ => {}
                }
                reply.set_json(document);
            } else if path == "/repositories/12/releases/assets/12" {
                match case {
                    "catalogue-truncated" => {
                        reply.declared = Some(reply.body.len());
                        reply.body.truncate(reply.body.len() / 2);
                    }
                    "catalogue-redirect" => {
                        reply.status = 302;
                        reply.headers.push((
                            "Location".into(),
                            "https://unapproved.example/private?token=synthetic".into(),
                        ));
                    }
                    _ => {}
                }
            } else if case == "revision" && path == "/repositories/12/git/ref/tags/v1.0" {
                reply.set_json(
                    json!({"ref":"refs/tags/v1.0","object":{"type":"commit","sha":"b".repeat(40)}}),
                );
            }
            reply
        });
        let problem = server.inspect(true).unwrap_err();
        assert!(!problem.0.contains("synthetic"), "{case}");
        assert!(
            !server
                .requests()
                .iter()
                .any(|request| request.contains("/releases/assets/11 ")
                    || request.contains("/commits/main")
                    || request.contains("/among-plugins.json ")),
            "{case}"
        );
    }
}

#[test]
fn actual_latest_or_custom_publisher_drift_requires_fresh_review_before_package_transfer() {
    for case in ["repository", "publisher", "latest-id"] {
        let root = super::super::super::package::tests::OwnedRoot::new();
        std::fs::write(root.0.join("unrelated-settings"), b"keep exact bytes").unwrap();
        let changed = Arc::new(AtomicBool::new(false));
        let (metadata, archive) = super::super::super::package::tests::fixture();
        let server = Server::new({
            let changed = changed.clone();
            move |request| {
                let mut reply = package_route(metadata.clone(), archive.clone(), request);
                if changed.load(Ordering::Acquire) {
                    if request.starts_with("GET /repositories/12 ") {
                        let mut document: Value = serde_json::from_slice(&reply.body).unwrap();
                        match case {
                            "repository" => document["id"] = json!(99),
                            "publisher" => document["owner"]["id"] = json!(99),
                            _ => {}
                        }
                        reply.set_json(document);
                    } else if case == "latest-id"
                        && request.starts_with("GET /repositories/12/releases/latest ")
                    {
                        let mut document: Value = serde_json::from_slice(&reply.body).unwrap();
                        document["id"] = json!(8);
                        reply.set_json(document);
                    }
                }
                reply
            }
        });
        let selection = server
            .inspect(true)
            .unwrap()
            .select("sample", "1.0")
            .unwrap();
        changed.store(true, Ordering::Release);
        let problem = install_at(&server, selection, &root.0).unwrap_err();
        assert!(problem.0.contains("review"), "{case}: {problem}");
        assert!(!root.0.join("installed.json").exists());
        assert_eq!(
            std::fs::read(root.0.join("unrelated-settings")).unwrap(),
            b"keep exact bytes"
        );
        assert!(
            !server
                .requests()
                .iter()
                .any(|request| request.contains("/releases/assets/11 ")),
            "{case}"
        );
    }
}
