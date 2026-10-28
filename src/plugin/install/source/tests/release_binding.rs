use super::*;
use serde_json::json;

fn reference(tag: &str, kind: &str, sha: &str) -> Reply {
    Reply::json(json!({"ref":format!("refs/tags/{tag}"),"object":{"type":kind,"sha":sha}}))
}

#[test]
fn actual_https_release_binding_peels_lightweight_annotated_nested_and_bounded_depth_tags() {
    for (label, depth, tag) in [
        ("lightweight", 0, "v1.0"),
        ("annotated", 1, "v1.0"),
        ("nested annotations", 2, "releases/v1.0"),
        ("eight annotations", 8, "v1.0"),
    ] {
        let root = super::super::super::package::tests::OwnedRoot::new();
        let (mut metadata, archive) = super::super::super::package::tests::fixture();
        metadata["revision"] = json!("d".repeat(40));
        let server = Server::new(move |request| {
            let path = request.split_whitespace().nth(1).unwrap();
            if path == "/repositories/12/releases/latest" {
                let mut reply = package_route(metadata.clone(), archive.clone(), request);
                let mut release: Value = serde_json::from_slice(&reply.body).unwrap();
                release["tag_name"] = json!(tag);
                release["target_commitish"] = json!("an-unrelated-mutable-branch");
                reply.set_json(release);
                return reply;
            }
            if path.starts_with("/repositories/12/git/ref/tags/") {
                return reference(
                    tag,
                    if depth == 0 { "commit" } else { "tag" },
                    &if depth == 0 {
                        "d".repeat(40)
                    } else {
                        format!("{:040x}", 1)
                    },
                );
            }
            if let Some(sha) = path.strip_prefix("/repositories/12/git/tags/") {
                let index = u64::from_str_radix(sha, 16).unwrap();
                assert!(index <= depth);
                return Reply::json(
                    json!({"sha":sha,"tag":format!("inner-{index}"),"object":{"type":if index==depth {"commit"}else{"tag"},"sha":if index==depth {"d".repeat(40)}else{format!("{:040x}",index+1)}}}),
                );
            }
            package_route(metadata.clone(), archive.clone(), request)
        });
        let inspection = server.inspect(true).unwrap();
        assert_eq!(inspection.catalogue_revision, "d".repeat(40));
        let committed = install_at(
            &server,
            inspection.select("sample", "1.0").unwrap(),
            &root.0,
        )
        .unwrap();
        let source = committed.receipt.source.as_ref().unwrap();
        assert_eq!(source.catalogue_revision, "d".repeat(40));
        assert_eq!(committed.receipt.revision, "d".repeat(40));
        let binding = source.release_binding.as_ref().unwrap();
        assert_eq!(binding.tag, tag);
        assert_eq!(binding.commit, committed.receipt.revision);
        assert_eq!(
            binding.object,
            if depth == 0 {
                "d".repeat(40)
            } else {
                format!("{:040x}", 1)
            }
        );
        assert_eq!(server.requests().len(), 15 + 3 * depth as usize);
        assert_eq!(
            server
                .requests()
                .iter()
                .filter(|request| request.contains("/git/ref/tags/"))
                .count(),
            3
        );
        assert!(server
            .requests()
            .iter()
            .all(|request| !request.to_lowercase().contains("authorization:")
                && !request.to_lowercase().contains("cookie:")));
        let mut host = crate::plugin::host::PluginHost::new();
        let entry = crate::plugin::inventory::read(&root.0).unwrap().remove(0);
        assert_eq!(entry.receipt, committed.receipt);
        host.register_managed(entry, None);
        assert!(!host.desired_enabled("github:12/sample"));
        assert_eq!(host.supervised_count(), 0);
        assert!(!committed
            .receipt
            .package_path(&root.0)
            .join("executed")
            .exists());
        println!("PASS actual HTTPS {label}: release-asset catalogue revision equals peeled release tag; persisted/offline entry stays disabled");
    }
}

#[test]
fn actual_https_unrelated_release_update_preserves_previous_bytes_token_settings_and_binding() {
    let root = super::super::super::package::tests::OwnedRoot::new();
    let (metadata, archive) = super::super::super::package::tests::fixture();
    let initial = Server::new({
        let metadata = metadata.clone();
        let archive = archive.clone();
        move |request| package_route(metadata.clone(), archive.clone(), request)
    });
    let first = install_at(
        &initial,
        initial
            .inspect(true)
            .unwrap()
            .select("sample", "1.0")
            .unwrap(),
        &root.0,
    )
    .unwrap()
    .receipt;
    let before = std::fs::read(root.0.join("installed.json")).unwrap();
    let executable = std::fs::read(first.package_path(&root.0).join("bin/plugin")).unwrap();
    let local = root.0.join("retained-preferences.json");
    std::fs::write(&local, b"opaque unrelated local settings bytes").unwrap();
    let mut metadata = metadata;
    metadata["revision"] = json!("b".repeat(40));
    let update = Server::new(move |request| {
        if request.starts_with("GET /repositories/12/git/ref/tags/v1.0 ") {
            reference("v1.0", "commit", &"a".repeat(40))
        } else if request.starts_with(&format!(
            "GET /repositories/12/git/commits/{} ",
            "a".repeat(40)
        )) {
            Reply::json(json!({"sha":"a".repeat(40)}))
        } else {
            package_route(metadata.clone(), archive.clone(), request)
        }
    });
    let problem = update.inspect(true).unwrap_err();
    assert!(problem.0.contains("disagrees"));
    assert_eq!(
        std::fs::read(root.0.join("installed.json")).unwrap(),
        before
    );
    assert_eq!(
        std::fs::read(first.package_path(&root.0).join("bin/plugin")).unwrap(),
        executable
    );
    assert_eq!(
        crate::plugin::inventory::read(&root.0).unwrap()[0].receipt,
        first
    );
    assert_eq!(
        std::fs::read(local).unwrap(),
        b"opaque unrelated local settings bytes"
    );
    assert!(!update
        .requests()
        .iter()
        .any(|request| request.contains("/releases/assets/11 ")));
    println!("PASS actual HTTPS unrelated release update fails before transfer; previous selection, executable bytes, token, binding and unrelated settings remain exact");
}

#[test]
fn actual_install_worker_cancellation_during_tag_peeling_prevents_asset_download_and_commit() {
    let _serial = JOB_TESTS.lock().unwrap();
    let root = super::super::super::package::tests::OwnedRoot::new();
    let (metadata, archive) = super::super::super::package::tests::fixture();
    let selection = selected(&metadata);
    let server = Server::new(move |request| {
        if request.starts_with("GET /repositories/12/git/ref/tags/v1.0 ") {
            reference("v1.0", "tag", &"b".repeat(40))
        } else if request.contains("/git/tags/") {
            Reply::held()
        } else {
            package_route(metadata.clone(), archive.clone(), request)
        }
    });
    let job = server.install(selection, root.0.clone()).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while !server
        .requests()
        .iter()
        .any(|request| request.contains("/git/tags/"))
    {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(job.cancel());
    loop {
        if let Some(outcome) = job.poll() {
            assert!(outcome.is_err());
            break;
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(job.phase(), "Installation cancelled before commitment");
    assert!(!root.0.join("installed.json").exists());
    assert!(!server
        .requests()
        .iter()
        .any(|request| request.contains("/releases/assets/")));
    println!("PASS actual held HTTPS annotated-object resolution is cancelled without asset transfer, selected code or execution");
}

#[test]
fn actual_https_release_binding_refuses_unrelated_unavailable_ambiguous_and_moving_tags() {
    let cases = [
        "unrelated lightweight target",
        "unrelated annotated target",
        "missing exact tag",
        "prefix-match array",
        "wrong reference namespace",
        "invalid object sha",
        "tree target",
        "missing annotated object",
        "wrong annotated object identity",
        "tag cycle",
        "annotation depth overflow",
        "unavailable commit",
        "wrong commit response",
        "missing release tag",
        "unsafe release tag",
        "tag moved during transfer",
        "annotated head changed with same peeled commit",
        "tag deleted during transfer",
        "release tag changed during transfer",
        "immutable peel chain changed",
        "rate-limited resolution",
        "cross-origin resolution redirect",
        "truncated reference",
        "oversized reference",
        "duplicate reference keys",
        "release asset changed during transfer",
        "release became draft during transfer",
        "release identity changed during transfer",
    ];
    let mut accepted = Vec::new();
    for case in cases {
        let root = super::super::super::package::tests::OwnedRoot::new();
        let (metadata, archive) = super::super::super::package::tests::fixture();
        let ref_reads = AtomicUsize::new(0);
        let release_reads = AtomicUsize::new(0);
        let server = Server::new(move |request| {
            let path = request.split_whitespace().nth(1).unwrap();
            if path == "/repositories/12/releases/latest" {
                let mut reply = package_route(metadata.clone(), archive.clone(), request);
                let mut value: Value = serde_json::from_slice(&reply.body).unwrap();
                let later = release_reads.fetch_add(1, Ordering::AcqRel) > 1;
                if case == "missing release tag" {
                    value.as_object_mut().unwrap().remove("tag_name");
                }
                if case == "unsafe release tag" {
                    value["tag_name"] = json!("../heads/main");
                }
                if case == "release tag changed during transfer" && later {
                    value["tag_name"] = json!("v2.0");
                }
                value["target_commitish"] = json!("a".repeat(40));
                if case == "release asset changed during transfer" && later {
                    value["assets"][0]["digest"] = json!(format!("sha256:{}", "0".repeat(64)));
                }
                if case == "release became draft during transfer" && later {
                    value["draft"] = json!(true);
                }
                if case == "release identity changed during transfer" && later {
                    value["id"] = json!(8);
                }
                reply.set_json(value);
                return reply;
            }
            if let Some(tag) = path.strip_prefix("/repositories/12/git/ref/tags/") {
                let later = ref_reads.fetch_add(1, Ordering::AcqRel) > 1;
                let good = "a".repeat(40);
                let other = "b".repeat(40);
                return match case {
                    "unrelated lightweight target" => reference(tag,"commit",&other),
                    "prefix-match array" => Reply::json(json!([{"ref":"refs/tags/v1.0-extra","object":{"type":"commit","sha":good}}])),
                    "wrong reference namespace" => Reply::json(json!({"ref":"refs/heads/v1.0","object":{"type":"commit","sha":good}})),
                    "invalid object sha" => reference(tag,"commit","abc"),
                    "tree target" => reference(tag,"tree",&good),
                    "missing exact tag" => Reply {status:404,..Reply::json(Value::Null)},
                    "tag deleted during transfer" if later => Reply {status:404,..Reply::json(Value::Null)},
                    "tag moved during transfer" if later => reference(tag,"commit",&other),
                    "annotated head changed with same peeled commit" => reference(tag,"tag",&if later {"c".repeat(40)} else {other}),
                    "unrelated annotated target" | "missing annotated object" | "wrong annotated object identity" | "tag cycle" | "annotation depth overflow" | "immutable peel chain changed" => reference(tag,"tag",&other),
                    "rate-limited resolution" => Reply::rate_limited(),
                    "cross-origin resolution redirect" => Reply {status:302,headers:vec![("Location".into(),"https://unapproved.example/private?token=synthetic".into())],..Reply::json(Value::Null)},
                    "truncated reference" => Reply {declared:Some(1000),..Reply::json(Value::Null)},
                    "oversized reference" => Reply {body:vec![b' ';data::MAX_STATIC_BYTES+1],chunked:true,..Reply::json(Value::Null)},
                    "duplicate reference keys" => Reply {body:format!("{{\"ref\":\"refs/tags/v1.0\",\"ref\":\"refs/tags/v1.0\",\"object\":{{\"type\":\"commit\",\"sha\":\"{good}\"}}}}").into_bytes(),..Reply::json(Value::Null)},
                    _ => reference(tag,"commit",&good),
                };
            }
            if let Some(sha) = path.strip_prefix("/repositories/12/git/tags/") {
                if case == "missing annotated object" {
                    return Reply {
                        status: 404,
                        ..Reply::json(Value::Null)
                    };
                }
                let later = ref_reads.load(Ordering::Acquire) > 2;
                let (kind, target) = match case {
                    "unrelated annotated target" => ("commit", "c".repeat(40)),
                    "tag cycle" => ("tag", sha.into()),
                    "annotation depth overflow" => (
                        "tag",
                        if sha == "b".repeat(40) {
                            format!("{:040x}", 1)
                        } else {
                            format!("{:040x}", u64::from_str_radix(sha, 16).unwrap() + 1)
                        },
                    ),
                    "immutable peel chain changed" if later && sha == "b".repeat(40) => {
                        ("tag", "c".repeat(40))
                    }
                    _ => ("commit", "a".repeat(40)),
                };
                return Reply::json(
                    json!({"sha":if case=="wrong annotated object identity" {"c".repeat(40)} else {sha.into()},"tag":"nested-annotation","object":{"type":kind,"sha":target}}),
                );
            }
            if let Some(sha) = path.strip_prefix("/repositories/12/git/commits/") {
                if case == "unavailable commit" {
                    return Reply {
                        status: 404,
                        ..Reply::json(Value::Null)
                    };
                }
                return Reply::json(
                    json!({"sha":if case=="wrong commit response" {"c".repeat(40)} else {sha.into()}}),
                );
            }
            package_route(metadata.clone(), archive.clone(), request)
        });
        let outcome = server
            .inspect(true)
            .and_then(|inspection| inspection.select("sample", "1.0"))
            .and_then(|selection| install_at(&server, selection, &root.0));
        println!(
            "actual HTTPS strict release boundary {case}: {}",
            if outcome.is_err() {
                "REFUSED"
            } else {
                "UNEXPECTED COMMIT"
            }
        );
        if let Err(problem) = outcome {
            assert!(!problem.0.contains("synthetic"));
            assert!(!root.0.join("installed.json").exists());
            assert!(crate::plugin::inventory::read(&root.0).unwrap().is_empty());
            if case.contains("during transfer")
                || case.contains("same peeled commit")
                || case == "immutable peel chain changed"
            {
                assert!(
                    server
                        .requests()
                        .iter()
                        .any(|request| request.contains("/github-production-release-asset/12/")),
                    "{case}: drift must be observed after a real transfer"
                );
            }
        } else {
            accepted.push(case);
        }
        assert!(!root.0.join("executed").exists());
    }
    assert!(
        accepted.is_empty(),
        "unbound or inconsistent release tags were committed: {accepted:?}"
    );
}
