use super::*;
use serde_json::json;

#[test]
fn actual_https_refuses_invalid_complete_release_inventory_before_package_transfer() {
    for case in [
        "draft",
        "prerelease",
        "mutable",
        "missing-immutable",
        "missing-catalogue",
        "duplicate-catalogue",
        "duplicate-id",
        "unselected-null-digest",
        "unselected-uppercase-digest",
        "unselected-not-uploaded",
        "unselected-invalid-size",
        "too-many-assets",
        "duplicate-release-key",
    ] {
        let root = super::super::super::package::tests::OwnedRoot::new();
        let (metadata, archive) = super::super::super::package::tests::fixture();
        let selection = selected(&metadata);
        let server = Server::new(move |request| {
            let mut reply = package_route(metadata.clone(), archive.clone(), request);
            if request.starts_with("GET /repositories/12/releases/latest ") {
                if case == "duplicate-release-key" {
                    let rest = std::str::from_utf8(&reply.body)
                        .unwrap()
                        .strip_prefix('{')
                        .unwrap();
                    reply.body = format!("{{\"draft\":false,{rest}").into_bytes();
                    return reply;
                }
                let mut value: Value = serde_json::from_slice(&reply.body).unwrap();
                match case {
                    "draft" => value["draft"] = json!(true),
                    "prerelease" => value["prerelease"] = json!(true),
                    "mutable" => value["immutable"] = json!(false),
                    "missing-immutable" => {
                        value.as_object_mut().unwrap().remove("immutable");
                    }
                    "missing-catalogue" => {
                        value["assets"].as_array_mut().unwrap().remove(1);
                    }
                    "duplicate-catalogue" => {
                        value["assets"][2]["name"] = json!("among-plugins.json")
                    }
                    "duplicate-id" => value["assets"][2]["id"] = json!(11),
                    "unselected-null-digest" => value["assets"][2]["digest"] = Value::Null,
                    "unselected-uppercase-digest" => {
                        value["assets"][2]["digest"] = json!(format!("sha256:{}", "A".repeat(64)));
                    }
                    "unselected-not-uploaded" => value["assets"][2]["state"] = json!("starter"),
                    "unselected-invalid-size" => value["assets"][2]["size"] = json!(-1),
                    "too-many-assets" => {
                        for id in 14..76 {
                            value["assets"].as_array_mut().unwrap().push(json!({
                                "id":id,"name":format!("extra-{id}"),"state":"uploaded",
                                "size":1,"digest":format!("sha256:{}","a".repeat(64))}));
                        }
                    }
                    _ => unreachable!(),
                }
                reply.set_json(value);
            }
            reply
        });
        let error = install_at(&server, selection, &root.0).unwrap_err();
        assert!(!error.0.contains("synthetic"));
        assert!(!root.0.join("installed.json").exists(), "{case}");
        assert!(crate::plugin::inventory::read(&root.0).unwrap().is_empty());
        assert!(
            !server
                .requests()
                .iter()
                .any(|request| request.contains("/releases/assets/")),
            "{case}: invalid release inventory must refuse before package transfer"
        );
    }
}

#[test]
fn actual_https_rechecks_unselected_assets_and_ignores_inventory_order_only() {
    for case in [
        "id",
        "name",
        "size",
        "digest",
        "catalogue-digest",
        "order-only",
    ] {
        let root = super::super::super::package::tests::OwnedRoot::new();
        let (metadata, archive) = super::super::super::package::tests::fixture();
        let selection = selected(&metadata);
        let reads = AtomicUsize::new(0);
        let server = Server::new(move |request| {
            let mut reply = package_route(metadata.clone(), archive.clone(), request);
            if request.starts_with("GET /repositories/12/releases/latest ")
                && reads.fetch_add(1, Ordering::AcqRel) > 0
            {
                let mut value: Value = serde_json::from_slice(&reply.body).unwrap();
                match case {
                    "id" => value["assets"][2]["id"] = json!(99),
                    "name" => value["assets"][2]["name"] = json!("new-NOTICES"),
                    "size" => value["assets"][2]["size"] = json!(2),
                    "digest" => {
                        value["assets"][2]["digest"] = json!(format!("sha256:{}", "b".repeat(64)))
                    }
                    "catalogue-digest" => {
                        value["assets"][1]["digest"] = json!(format!("sha256:{}", "b".repeat(64)))
                    }
                    "order-only" => value["assets"].as_array_mut().unwrap().reverse(),
                    _ => unreachable!(),
                }
                reply.set_json(value);
            }
            reply
        });
        let outcome = install_at(&server, selection, &root.0);
        assert!(
            server
                .requests()
                .iter()
                .any(|request| request.contains("/github-production-release-asset/12/")),
            "{case}: the recheck must follow a real package transfer"
        );
        assert_eq!(
            server
                .requests()
                .iter()
                .filter(|request| request.contains("/git/ref/tags/"))
                .count(),
            2,
            "{case}: both complete tag walks are retained"
        );
        if case == "order-only" {
            let committed = outcome.unwrap();
            assert!(root.0.join("installed.json").is_file());
            assert_eq!(committed.receipt.release().unwrap().asset_id, 11);
            assert!(!committed
                .receipt
                .package_path(&root.0)
                .join("executed")
                .exists());
        } else {
            assert!(outcome.unwrap_err().0.contains("snapshot"), "{case}");
            assert!(!root.0.join("installed.json").exists(), "{case}");
            assert!(crate::plugin::inventory::read(&root.0).unwrap().is_empty());
        }
    }
}
