use super::*;
use serde_json::json;

type Pages = Arc<Mutex<Vec<Vec<Value>>>>;

fn listed(id: u64, prerelease: bool, draft: bool, created_at: &str) -> Value {
    json!({"id":id,"tag_name":"v1.0","draft":draft,"prerelease":prerelease,
        "immutable":true,"created_at":created_at,"assets":[]})
}

fn stable_page(first_id: u64, count: u64) -> Vec<Value> {
    (first_id..first_id + count)
        .map(|id| listed(id, false, false, "2026-01-01T00:00:00Z"))
        .collect()
}

fn path(request: &str) -> &str {
    request.split_whitespace().nth(1).unwrap()
}

// Serves the paged list from `pages` and every listed release as its own
// complete document; all other paths are the ordinary fixture repository.
fn route(
    metadata: Value,
    archive: Vec<u8>,
    pages: Pages,
    defect: impl Fn(&mut Value) + Send + 'static,
) -> impl Fn(&str) -> Reply + Send + 'static {
    move |request| {
        let path = path(request);
        if let Some(page) = path.strip_prefix("/repositories/12/releases?per_page=100&page=") {
            let page = page.parse::<usize>().unwrap();
            let listed = pages
                .lock()
                .unwrap()
                .get(page - 1)
                .cloned()
                .unwrap_or_default();
            return Reply::json(json!(listed));
        }
        if let Some(id) = path
            .strip_prefix("/repositories/12/releases/")
            .and_then(|id| id.parse::<u64>().ok())
        {
            let entry = pages
                .lock()
                .unwrap()
                .iter()
                .flatten()
                .find(|entry| entry["id"] == json!(id))
                .cloned()
                .unwrap();
            let mut document = release_document(&metadata);
            document["id"] = json!(id);
            document["prerelease"] = entry["prerelease"].clone();
            document["draft"] = entry["draft"].clone();
            document["created_at"] = entry["created_at"].clone();
            defect(&mut document);
            return Reply::json(document);
        }
        package_route(metadata.clone(), archive.clone(), request)
    }
}

fn prerelease_server(pages: Vec<Vec<Value>>) -> Server {
    let (metadata, archive) = super::super::super::package::tests::fixture();
    Server::new(route(
        metadata,
        archive,
        Arc::new(Mutex::new(pages)),
        |_| {},
    ))
}

fn list_requests(server: &Server) -> Vec<String> {
    server
        .requests()
        .iter()
        .map(|request| path(request).to_string())
        .filter(|path| path.starts_with("/repositories/12/releases?"))
        .collect()
}

#[test]
fn creation_times_are_exact_utc_seconds_and_impossible_dates_are_refused() {
    assert_eq!(
        release::created_at("2024-02-29T23:59:59Z"),
        Some((2024, 2, 29, 23, 59, 59))
    );
    for bad in [
        "",
        "2026-10-30T10:00:00",
        "2026-10-30T10:00:00.123Z",
        "2026-10-30T10:00:00+00:00",
        "2026-10-30 10:00:00Z",
        "2026-10-30t10:00:00z",
        "2026-02-29T00:00:00Z",
        "2026-02-30T00:00:00Z",
        "2026-04-31T00:00:00Z",
        "2026-13-01T00:00:00Z",
        "2026-00-01T00:00:00Z",
        "2026-10-00T00:00:00Z",
        "2026-10-30T24:00:00Z",
        "2026-10-30T10:60:00Z",
        "2026-10-30T10:00:60Z",
        "2026-1a-30T10:00:00Z",
        "+026-10-30T10:00:00Z",
        "2100-02-29T00:00:00Z",
    ] {
        assert_eq!(release::created_at(bad), None, "{bad}");
    }
    assert!(release::created_at("2000-02-29T00:00:00Z").is_some());
    assert!(
        release::created_at("2026-10-30T09:59:59Z") < release::created_at("2026-10-30T10:00:00Z")
    );
    assert!(
        release::created_at("2025-12-31T23:59:59Z") < release::created_at("2026-01-01T00:00:00Z")
    );
}

#[test]
fn the_greatest_creation_time_wins_and_drafts_and_stable_releases_are_ignored() {
    let server = prerelease_server(vec![vec![
        // Newer than every prerelease, but stable: ignored, even with a
        // malformed creation time, because only candidates are compared.
        listed(50, false, false, "not a time"),
        // A draft prerelease is never a candidate.
        listed(60, true, true, "2026-10-30T00:00:00Z"),
        listed(40, true, false, "2026-10-20T00:00:00Z"),
        listed(39, true, false, "2026-10-25T08:00:00Z"),
        listed(41, true, false, "2026-10-21T00:00:00Z"),
    ]]);
    let inspection = server.inspect_prerelease().unwrap();
    assert!(inspection.is_prerelease());
    assert!(inspection.release.snapshot.is_prerelease());
    assert_eq!(inspection.release.snapshot.id, 39);
    let paths = server
        .requests()
        .iter()
        .map(|request| path(request).to_string())
        .collect::<Vec<_>>();
    let commit = format!(
        "/repositories/12/git/commits/{}",
        inspection.catalogue_revision
    );
    // Measured API cost of a prerelease refresh on a one-page list: identity,
    // one list page, the chosen release by id, the catalogue asset, the tag
    // reference and its commit.
    assert_eq!(
        paths,
        [
            "/repositories/12",
            "/repositories/12/releases?per_page=100&page=1",
            "/repositories/12/releases/39",
            "/repositories/12/releases/assets/12",
            "/repositories/12/git/ref/tags/v1.0",
            commit.as_str(),
        ]
    );
    assert!(paths.iter().all(|path| !path.ends_with("/releases/latest")));
}

#[test]
fn equal_creation_times_choose_the_greater_release_id_in_any_list_order() {
    for order in [[71, 70], [70, 71]] {
        let server = prerelease_server(vec![order
            .iter()
            .map(|id| listed(*id, true, false, "2026-10-29T12:00:00Z"))
            .collect()]);
        assert_eq!(
            server.inspect_prerelease().unwrap().release.snapshot.id,
            71,
            "{order:?}"
        );
    }
}

#[test]
fn every_page_up_to_the_third_is_read_and_a_fourth_is_never_requested() {
    let mut third = stable_page(201, 20);
    third.push(listed(900, true, false, "2026-10-01T00:00:00Z"));
    let server = prerelease_server(vec![stable_page(1, 100), stable_page(101, 100), third]);
    assert_eq!(
        server.inspect_prerelease().unwrap().release.snapshot.id,
        900
    );
    assert_eq!(
        list_requests(&server),
        [
            "/repositories/12/releases?per_page=100&page=1",
            "/repositories/12/releases?per_page=100&page=2",
            "/repositories/12/releases?per_page=100&page=3",
        ]
    );

    let server = prerelease_server(vec![
        stable_page(1, 100),
        stable_page(101, 100),
        stable_page(201, 100),
        vec![listed(900, true, false, "2026-10-01T00:00:00Z")],
    ]);
    assert_eq!(
        server.inspect_prerelease().unwrap_err(),
        Problem("no published prerelease within the first 300 releases; nothing was changed")
    );
    assert_eq!(list_requests(&server).len(), 3);
    assert!(server
        .requests()
        .iter()
        .all(|request| !request.contains("page=4")
            && !request.contains("/releases/900 ")
            && !request.contains("/releases/assets/")));

    // The bound is the bound even when a candidate was found: a newer
    // prerelease on the fourth page is not seen.
    let mut third = stable_page(201, 99);
    third.push(listed(800, true, false, "2026-09-01T00:00:00Z"));
    let server = prerelease_server(vec![
        stable_page(1, 100),
        stable_page(101, 100),
        third,
        vec![listed(900, true, false, "2026-10-01T00:00:00Z")],
    ]);
    assert_eq!(
        server.inspect_prerelease().unwrap().release.snapshot.id,
        800
    );
    assert_eq!(list_requests(&server).len(), 3);
}

#[test]
fn a_short_page_ends_the_list_before_the_bound() {
    let mut first = stable_page(1, 98);
    first.push(listed(500, true, false, "2026-10-01T00:00:00Z"));
    // A newer prerelease behind a short page is not part of the list GitHub
    // reported, and is not requested.
    let server = prerelease_server(vec![
        first,
        vec![listed(600, true, false, "2026-10-30T00:00:00Z")],
    ]);
    assert_eq!(
        server.inspect_prerelease().unwrap().release.snapshot.id,
        500
    );
    assert_eq!(
        list_requests(&server),
        ["/repositories/12/releases?per_page=100&page=1"]
    );

    let server = prerelease_server(vec![Vec::new()]);
    assert_eq!(
        server.inspect_prerelease().unwrap_err(),
        Problem("no published prerelease within the first 300 releases; nothing was changed")
    );
    assert_eq!(list_requests(&server).len(), 1);
    assert_eq!(
        server.requests().len(),
        2,
        "identity and one empty page only"
    );
}

#[test]
fn an_unparsable_candidate_creation_time_refuses_instead_of_skipping_it() {
    for bad in [
        json!("2026-10-30T10:00:00.5Z"),
        json!("2026-02-30T00:00:00Z"),
        json!(1_761_820_800),
        Value::Null,
    ] {
        let mut odd = listed(31, true, false, "");
        odd["created_at"] = bad.clone();
        let server = prerelease_server(vec![vec![
            listed(30, true, false, "2026-10-01T00:00:00Z"),
            odd,
        ]]);
        assert_eq!(
            server.inspect_prerelease().unwrap_err(),
            Problem(
                "prerelease creation time is not an exact UTC timestamp; refusing to guess the newest"
            ),
            "{bad}"
        );
        assert!(
            server
                .requests()
                .iter()
                .all(|request| !request.contains("/releases/30 ")),
            "{bad}: the older candidate is not used instead"
        );
    }
}

#[test]
fn malformed_or_repeated_list_entries_refuse_the_whole_list() {
    for (case, pages) in [
        (
            "no id",
            vec![vec![json!({"draft":false,"prerelease":true})]],
        ),
        (
            "no draft state",
            vec![vec![
                json!({"id":5,"prerelease":true,"created_at":"2026-10-01T00:00:00Z"}),
            ]],
        ),
        (
            "string prerelease state",
            vec![vec![json!({"id":5,"draft":false,"prerelease":"true"})]],
        ),
        (
            "repeated across pages",
            vec![
                stable_page(1, 100),
                vec![listed(100, true, false, "2026-10-01T00:00:00Z")],
            ],
        ),
    ] {
        let server = prerelease_server(pages);
        assert!(server.inspect_prerelease().is_err(), "{case}");
        assert!(
            server
                .requests()
                .iter()
                .all(|request| !request.contains("/releases/assets/")),
            "{case}"
        );
    }
    let (metadata, archive) = super::super::super::package::tests::fixture();
    let inner = route(metadata, archive, Arc::new(Mutex::new(Vec::new())), |_| {});
    let server = Server::new(move |request| {
        if path(request).starts_with("/repositories/12/releases?") {
            return Reply::json(json!({"releases": []}));
        }
        inner(request)
    });
    assert_eq!(
        server.inspect_prerelease().unwrap_err(),
        Problem("release list page is not an array of at most 100 releases")
    );
}

#[test]
fn an_oversized_or_redirected_list_page_is_refused_and_never_truncated() {
    let (metadata, archive) = super::super::super::package::tests::fixture();
    for case in ["declared", "redirect", "missing"] {
        let inner = route(
            metadata.clone(),
            archive.clone(),
            Arc::new(Mutex::new(Vec::new())),
            |_| {},
        );
        let server = Server::new(move |request| {
            if !path(request).starts_with("/repositories/12/releases?") {
                return inner(request);
            }
            let mut reply = Reply::json(json!([]));
            match case {
                "declared" => reply.declared = Some(release::MAX_RELEASE_PAGE_BYTES + 1),
                "redirect" => {
                    reply.status = 302;
                    reply.headers.push((
                        "Location".into(),
                        "https://api.github.com/repositories/12/releases?per_page=100&page=1"
                            .into(),
                    ));
                }
                _ => reply.status = 404,
            }
            reply
        });
        let problem = server.inspect_prerelease().unwrap_err();
        assert_eq!(
            problem,
            Problem(match case {
                "declared" => "repository response exceeds its byte budget",
                "redirect" => "repository redirect changed the approved origin or resource",
                _ => "repository release list is unavailable; nothing was changed",
            }),
            "{case}"
        );
        assert_eq!(list_requests(&server).len(), 1, "{case}");
    }
}

#[test]
fn a_defective_newest_prerelease_fails_like_an_ordinary_release_without_falling_back() {
    type Defect = fn(&mut Value);
    let defects: [(&str, Defect); 5] = [
        ("mutable", |document| document["immutable"] = json!(false)),
        ("missing immutability", |document| {
            document.as_object_mut().unwrap().remove("immutable");
        }),
        ("package digest", |document| {
            document["assets"][0]["digest"] = json!(format!("sha256:{}", "A".repeat(64)))
        }),
        ("unselected digest", |document| {
            document["assets"][2]["digest"] = Value::Null
        }),
        ("no catalogue", |document| {
            document["assets"].as_array_mut().unwrap().remove(1);
        }),
    ];
    for (case, defect) in defects {
        let (metadata, archive) = super::super::super::package::tests::fixture();
        let pages = Arc::new(Mutex::new(vec![vec![
            listed(30, true, false, "2026-10-01T00:00:00Z"),
            listed(31, true, false, "2026-10-02T00:00:00Z"),
        ]]));
        let server = Server::new(route(
            metadata.clone(),
            archive.clone(),
            pages,
            move |document| {
                if document["id"] == json!(31) {
                    defect(document)
                }
            },
        ));
        let problem = server.inspect_prerelease().unwrap_err();
        // The same defect on `releases/latest` of an ordinary source.
        let ordinary = Server::new(move |request| {
            let mut reply = package_route(metadata.clone(), archive.clone(), request);
            if path(request) == "/repositories/12/releases/latest" {
                let mut document: Value = serde_json::from_slice(&reply.body).unwrap();
                defect(&mut document);
                reply.set_json(document);
            }
            reply
        });
        let expected = ordinary.inspect(true).unwrap_err();
        // Only the publication-channel refusal names its channel; every
        // inventory refusal is the same text on both paths.
        if matches!(case, "mutable" | "missing immutability") {
            assert_eq!(
                problem,
                Problem("development installation requires a published immutable prerelease"),
                "{case}"
            );
            assert_eq!(
                expected,
                Problem("ordinary installation requires a published stable immutable release"),
                "{case}"
            );
        } else {
            assert_eq!(problem, expected, "{case}");
        }
        assert!(
            server
                .requests()
                .iter()
                .all(|request| !request.contains("/releases/30 ")
                    && !request.contains("/releases/assets/")),
            "{case}: an older prerelease is not tried and nothing is transferred"
        );
    }
}

#[test]
fn a_listed_prerelease_that_is_no_longer_one_is_refused() {
    let (metadata, archive) = super::super::super::package::tests::fixture();
    for case in ["now stable", "other id", "other time", "gone"] {
        let pages = Arc::new(Mutex::new(vec![vec![listed(
            31,
            true,
            false,
            "2026-10-02T00:00:00Z",
        )]]));
        let inner = route(
            metadata.clone(),
            archive.clone(),
            pages,
            move |document| match case {
                "now stable" => document["prerelease"] = json!(false),
                "other id" => document["id"] = json!(32),
                _ => document["created_at"] = json!("2026-10-03T00:00:00Z"),
            },
        );
        let server = Server::new(move |request| {
            if case == "gone" && path(request) == "/repositories/12/releases/31" {
                return Reply {
                    status: 404,
                    ..Reply::json(Value::Null)
                };
            }
            inner(request)
        });
        let problem = server.inspect_prerelease().unwrap_err();
        assert_eq!(
            problem,
            Problem(match case {
                "now stable" =>
                    "development installation requires a published immutable prerelease",
                "gone" => "newest prerelease disappeared while it was read; refresh again",
                _ => "prerelease document disagrees with its listed identity or creation time",
            }),
            "{case}"
        );
    }
}

#[test]
fn a_reviewed_prerelease_installs_with_its_channel_and_reopens_offline() {
    let root = super::super::super::package::tests::OwnedRoot::new();
    let server = prerelease_server(vec![vec![
        listed(30, true, false, "2026-10-01T00:00:00Z"),
        listed(7, false, false, "2026-10-05T00:00:00Z"),
    ]]);
    let selection = server
        .inspect_prerelease()
        .unwrap()
        .select("sample", "1.0")
        .unwrap();
    assert!(selection.is_prerelease());
    let inspected = server.requests().len();
    let committed = install_at(&server, selection, &root.0).unwrap();
    let source = committed.receipt.source.as_ref().unwrap();
    assert_eq!(committed.receipt.release_id, 30);
    assert!(source.release_snapshot.is_prerelease());
    let offline = crate::plugin::inventory::read(&root.0).unwrap();
    assert_eq!(offline[0].receipt, committed.receipt);
    assert!(offline[0]
        .receipt
        .source
        .as_ref()
        .unwrap()
        .release_snapshot
        .is_prerelease());
    let install = server.requests()[inspected..]
        .iter()
        .map(|request| path(request).to_string())
        .collect::<Vec<_>>();
    // Measured API cost of the install: identity, the full prerelease
    // re-observation (list page, release by id, tag reference, commit) before
    // the transfer and again before the commit, and the package through the
    // asset API; the CDN transfer is not an API request.
    let api = install
        .iter()
        .filter(|path| !path.starts_with("/github-production-release-asset/"))
        .count();
    assert_eq!(api, 11, "{install:?}");
    assert_eq!(install.len(), 12, "{install:?}");
    assert_eq!(
        install
            .iter()
            .filter(|path| path.starts_with("/repositories/12/releases?"))
            .count(),
        2
    );
    assert_eq!(
        install
            .iter()
            .filter(|path| *path == "/repositories/12/releases/30")
            .count(),
        2
    );
    assert!(install
        .iter()
        .all(|path| !path.ends_with("/releases/latest")));
}

#[test]
fn a_newer_prerelease_before_commit_requires_a_new_review() {
    for moment in ["before transfer", "after transfer"] {
        let root = super::super::super::package::tests::OwnedRoot::new();
        let (metadata, archive) = super::super::super::package::tests::fixture();
        let pages = Arc::new(Mutex::new(vec![vec![listed(
            30,
            true,
            false,
            "2026-10-01T00:00:00Z",
        )]]));
        let inner = route(metadata, archive, pages.clone(), |_| {});
        let transferred = Arc::new(AtomicBool::new(false));
        let server = Server::new({
            let pages = pages.clone();
            let transferred = transferred.clone();
            move |request| {
                if path(request).starts_with("/github-production-release-asset/")
                    && moment == "after transfer"
                {
                    pages.lock().unwrap()[0].push(listed(31, true, false, "2026-10-02T00:00:00Z"));
                    transferred.store(true, Ordering::Release);
                }
                inner(request)
            }
        });
        let selection = server
            .inspect_prerelease()
            .unwrap()
            .select("sample", "1.0")
            .unwrap();
        if moment == "before transfer" {
            pages.lock().unwrap()[0].push(listed(31, true, false, "2026-10-02T00:00:00Z"));
        }
        let problem = install_at(&server, selection, &root.0).unwrap_err();
        assert_eq!(
            problem,
            Problem("newest prerelease snapshot changed; refresh and review again"),
            "{moment}"
        );
        assert_eq!(
            server
                .requests()
                .iter()
                .any(|request| request.contains("/releases/assets/11 ")),
            moment == "after transfer",
            "{moment}"
        );
        assert_eq!(
            transferred.load(Ordering::Acquire),
            moment == "after transfer"
        );
        assert!(!root.0.join("installed.json").exists(), "{moment}");
        assert!(crate::plugin::inventory::read(&root.0).unwrap().is_empty());
    }
}

#[test]
fn an_ordinary_selection_still_follows_latest_and_ignores_newer_prereleases() {
    let root = super::super::super::package::tests::OwnedRoot::new();
    let server = prerelease_server(vec![vec![listed(30, true, false, "2026-10-30T00:00:00Z")]]);
    let selection = server
        .inspect(true)
        .unwrap()
        .select("sample", "1.0")
        .unwrap();
    assert!(!selection.is_prerelease());
    let committed = install_at(&server, selection, &root.0).unwrap();
    assert_eq!(committed.receipt.release_id, 7);
    assert!(!committed
        .receipt
        .source
        .as_ref()
        .unwrap()
        .release_snapshot
        .is_prerelease());
    assert!(list_requests(&server).is_empty());
    assert!(server
        .requests()
        .iter()
        .all(|request| !request.contains("/releases/30 ")));
}

#[test]
fn an_ordinary_instance_refuses_prerelease_refresh_and_install_before_any_work() {
    let _serial = JOB_TESTS.lock().unwrap();
    assert!(!crate::plugin::development::enabled());
    let refused = Job::refresh_prerelease(12).err().unwrap();
    // An Err at all proves no worker was started: past the gate the call
    // returns a Job and fails only later, inside the worker.
    assert_eq!(refused, Problem(release::DEVELOPMENT_ONLY));

    let server = prerelease_server(vec![vec![listed(30, true, false, "2026-10-01T00:00:00Z")]]);
    let selection = server
        .inspect_prerelease()
        .unwrap()
        .select("sample", "1.0")
        .unwrap();
    let observed = server.requests().len();
    let root = super::super::super::package::tests::OwnedRoot::new();
    let refused = InstallJob::start(selection, root.0.clone()).err().unwrap();
    assert_eq!(refused, Problem(release::DEVELOPMENT_ONLY));
    assert_eq!(server.requests().len(), observed);
    assert!(!root.0.join("installed.json").exists());
}

#[test]
fn a_cached_source_reopens_with_the_channel_of_its_newest_stored_inspection() {
    use super::super::super::registry::{self, Outcome, Request};
    let root = super::super::super::package::tests::OwnedRoot::new();
    let server = prerelease_server(vec![vec![listed(30, true, false, "2026-10-01T00:00:00Z")]]);
    let list = || match registry::run(&root.0, Request::List, || Ok(()), || Ok(())).unwrap() {
        Outcome::List(entries) => entries[0].clone(),
        _ => panic!("expected the retained registration"),
    };
    let cached =
        || match registry::run(&root.0, Request::Cached(list()), || Ok(()), || Ok(())).unwrap() {
            Outcome::Cached { inspection, .. } => inspection,
            _ => panic!("expected an offline snapshot"),
        };
    registry::run(
        &root.0,
        Request::Add(Box::new(server.inspect(true).unwrap())),
        || Ok(()),
        || Ok(()),
    )
    .unwrap();
    assert!(!cached().is_prerelease());

    let development = server.inspect_prerelease().unwrap();
    let observed = development.release.clone();
    registry::run(
        &root.0,
        Request::Refresh {
            expected: list(),
            inspection: Box::new(development),
        },
        || Ok(()),
        || Ok(()),
    )
    .unwrap();
    let reopened = cached();
    assert!(reopened.is_prerelease());
    assert_eq!(reopened.release, observed);

    let ordinary = server.inspect(true).unwrap();
    registry::run(
        &root.0,
        Request::Refresh {
            expected: list(),
            inspection: Box::new(ordinary),
        },
        || Ok(()),
        || Ok(()),
    )
    .unwrap();
    let reopened = cached();
    assert!(!reopened.is_prerelease());
    assert_eq!(reopened.release.snapshot.id, 7);
}
