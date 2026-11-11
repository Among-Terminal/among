//! A catalogue may carry an unchanged plugin over from an earlier release of
//! the same repository. The owned repository below publishes two releases:
//! the latest one ships `fresh` and refers to the earlier one for `kept`.

use super::*;
use serde_json::json;

const LATEST_ID: u64 = 7;
const LATEST_TAG: &str = "v1.0";
const EARLIER_ID: u64 = 5;
const EARLIER_TAG: &str = "v0.9";

fn latest_commit() -> String {
    "a".repeat(40)
}

fn earlier_commit() -> String {
    "b".repeat(40)
}

fn path(request: &str) -> &str {
    request.split_whitespace().nth(1).unwrap()
}

fn sha(bytes: &[u8]) -> String {
    crate::plugin::inventory::checksum(bytes).unwrap()
}

fn asset(id: u64, name: &str, bytes: &[u8]) -> Value {
    json!({"id":id,"name":name,"size":bytes.len(),"state":"uploaded",
        "digest":format!("sha256:{}",sha(bytes))})
}

// One installable version envelope and its package bytes.
fn entry(id: &str, revision: &str) -> (Value, Vec<u8>) {
    let (mut version, archive) = super::super::super::package::tests::fixture_for(id);
    version.as_object_mut().unwrap().remove("schema_version");
    version["revision"] = json!(revision);
    version["artifacts"][0]["asset"] = json!(format!("{id}-1.0.0.tar.gz"));
    (version, archive)
}

/// The release tool's carry-over, restated from its source: the entry of
/// every unchanged plugin is replaced by the previous catalogue's entry,
/// verbatim, and gains a reference to the previous release unless it already
/// carries one.
pub(crate) fn carry_over(
    previous: &Value,
    fresh: &Value,
    unchanged: &[&str],
    id: u64,
    tag: &str,
) -> Value {
    let mut result = fresh.clone();
    for entry in result["plugins"].as_array_mut().unwrap() {
        let plugin = entry["id"].as_str().unwrap().to_owned();
        if !unchanged.contains(&plugin.as_str()) {
            continue;
        }
        let mut old = previous["plugins"]
            .as_array()
            .unwrap()
            .iter()
            .find(|candidate| candidate["id"] == plugin)
            .unwrap()
            .clone();
        if old["versions"][0].get("release").is_none() {
            old["versions"][0]["release"] = json!({"id": id, "tag": tag});
        }
        *entry = old;
    }
    result
}

#[derive(Clone)]
struct World {
    prerelease_channel: bool,
    publisher: u64,
    latest: Value,
    earlier: Value,
    earlier_tag_commit: String,
    catalogue: Value,
    fresh_archive: Vec<u8>,
    kept_archive: Vec<u8>,
}

impl World {
    fn new() -> Self {
        let (kept, kept_archive) = entry("kept", &earlier_commit());
        let (fresh, fresh_archive) = entry("fresh", &latest_commit());
        let earlier_catalogue =
            json!({"schema_version":2,"plugins":[{"id":"kept","versions":[kept.clone()]}]});
        let earlier_bytes = earlier_catalogue.to_string().into_bytes();
        // What the tool generates before carrying over: every plugin as if
        // built at the new commit.
        let mut rebuilt = kept.clone();
        rebuilt["revision"] = json!(latest_commit());
        let generated = json!({"schema_version":2,"plugins":[
            {"id":"fresh","versions":[fresh.clone()]},{"id":"kept","versions":[rebuilt]}]});
        let catalogue = carry_over(
            &earlier_catalogue,
            &generated,
            &["kept"],
            EARLIER_ID,
            EARLIER_TAG,
        );
        let mut world = Self {
            prerelease_channel: false,
            publisher: 34,
            latest: json!({"id":LATEST_ID,"draft":false,"prerelease":false,"immutable":true,
                "tag_name":LATEST_TAG,"created_at":"2026-10-05T00:00:00Z","assets":[
                    asset(70,"among-plugins.json",b""),
                    asset(71,"fresh-1.0.0.tar.gz",&fresh_archive)]}),
            earlier: json!({"id":EARLIER_ID,"draft":false,"prerelease":false,"immutable":true,
                "tag_name":EARLIER_TAG,"created_at":"2026-10-01T00:00:00Z","assets":[
                    asset(50,"among-plugins.json",&earlier_bytes),
                    asset(51,"kept-1.0.0.tar.gz",&kept_archive)]}),
            earlier_tag_commit: earlier_commit(),
            catalogue,
            fresh_archive,
            kept_archive,
        };
        world.seal();
        world
    }

    // The latest release names its catalogue by the bytes actually served.
    fn seal(&mut self) {
        let bytes = self.catalogue.to_string().into_bytes();
        self.latest["assets"][0] = asset(70, "among-plugins.json", &bytes);
    }

    fn kept_version(&mut self) -> &mut Value {
        self.catalogue["plugins"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .find(|plugin| plugin["id"] == "kept")
            .map(|plugin| &mut plugin["versions"][0])
            .unwrap()
    }

    // Both releases become prereleases of the development channel.
    fn on_prerelease_channel(&mut self) {
        self.prerelease_channel = true;
        self.latest["prerelease"] = json!(true);
    }

    fn respond(&self, request: &str) -> Reply {
        let missing = Reply {
            status: 404,
            ..Reply::json(Value::Null)
        };
        let path = path(request);
        match path {
            "/repos/example/sample" | "/repositories/12" => Reply::json(
                json!({"id":12,"owner":{"login":"example","id":self.publisher},"name":"sample","private":false}),
            ),
            "/repositories/12/releases/latest" if !self.prerelease_channel => {
                Reply::json(self.latest.clone())
            }
            "/repositories/12/releases?per_page=100&page=1" => {
                Reply::json(json!([self.latest, self.earlier]))
            }
            "/repositories/12/releases/7" => Reply::json(self.latest.clone()),
            "/repositories/12/releases/5" if !self.earlier.is_null() => {
                Reply::json(self.earlier.clone())
            }
            "/repositories/12/releases/assets/70" => {
                Reply::bytes(self.catalogue.to_string().into_bytes())
            }
            "/repositories/12/releases/assets/71" => Reply::bytes(self.fresh_archive.clone()),
            "/repositories/12/releases/assets/51" => Reply {
                status: 302,
                headers: vec![(
                    "Location".into(),
                    "https://release-assets.githubusercontent.com/github-production-release-asset/12/aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee?token=synthetic-delivery-token".into(),
                )],
                ..Reply::json(Value::Null)
            },
            path if path.starts_with("/github-production-release-asset/12/") => {
                Reply::bytes(self.kept_archive.clone())
            }
            "/repositories/12/git/ref/tags/v1.0" => Reply::json(
                json!({"ref":"refs/tags/v1.0","object":{"type":"commit","sha":latest_commit()}}),
            ),
            "/repositories/12/git/ref/tags/v0.9" => Reply::json(
                json!({"ref":"refs/tags/v0.9","object":{"type":"commit","sha":self.earlier_tag_commit}}),
            ),
            path => match path.strip_prefix("/repositories/12/git/commits/") {
                Some(commit) => Reply::json(json!({"sha":commit})),
                None => missing,
            },
        }
    }
}

fn serve(world: World) -> (Server, Arc<Mutex<World>>) {
    let world = Arc::new(Mutex::new(world));
    let served = world.clone();
    (
        Server::new(move |request| served.lock().unwrap().respond(request)),
        world,
    )
}

fn altered(alter: impl FnOnce(&mut World)) -> World {
    let mut world = World::new();
    alter(&mut world);
    world.seal();
    world
}

fn refusal(alter: impl FnOnce(&mut World)) -> String {
    let (server, _) = serve(altered(alter));
    server.inspect(true).unwrap_err().0.to_string()
}

fn paths(server: &Server, from: usize) -> Vec<String> {
    server.requests()[from..]
        .iter()
        .map(|request| path(request).to_string())
        .collect()
}

#[test]
fn a_catalogue_with_one_fresh_and_one_carried_plugin_installs_both_from_their_own_releases() {
    let root = super::super::super::package::tests::OwnedRoot::new();
    let world = World::new();
    let earlier_catalogue_sha256 = world.earlier["assets"][0]["digest"]
        .as_str()
        .unwrap()
        .trim_start_matches("sha256:")
        .to_string();
    let latest_catalogue_sha256 = sha(world.catalogue.to_string().as_bytes());
    let (server, _) = serve(world);
    let inspection = server.inspect(true).unwrap();
    // Refresh cost: repository, latest release, catalogue, tag reference and
    // commit as before, then the earlier release with its tag reference and
    // commit.
    assert_eq!(
        paths(&server, 0),
        [
            "/repos/example/sample",
            "/repositories/12/releases/latest",
            "/repositories/12/releases/assets/70",
            "/repositories/12/git/ref/tags/v1.0",
            format!("/repositories/12/git/commits/{}", latest_commit()).as_str(),
            "/repositories/12/releases/5",
            "/repositories/12/git/ref/tags/v0.9",
            format!("/repositories/12/git/commits/{}", earlier_commit()).as_str(),
        ]
    );
    assert_eq!(inspection.catalogue_revision, latest_commit());

    let before = server.requests().len();
    let kept = install_at(
        &server,
        inspection.select("kept", "1.0.0").unwrap(),
        &root.0,
    )
    .unwrap()
    .receipt;
    let install = paths(&server, before);
    // The carried package is fetched from the earlier release, and both
    // releases are observed with their tag chains before the transfer and
    // again before the commit.
    assert_eq!(
        install
            .iter()
            .filter(|path| *path == "/repositories/12/releases/assets/51")
            .count(),
        1
    );
    assert!(install
        .iter()
        .all(|path| path != "/repositories/12/releases/assets/71"));
    for observed in [
        "/repositories/12/releases/latest",
        "/repositories/12/git/ref/tags/v1.0",
        "/repositories/12/releases/5",
        "/repositories/12/git/ref/tags/v0.9",
    ] {
        assert_eq!(
            install.iter().filter(|path| *path == observed).count(),
            2,
            "{observed}: {install:?}"
        );
    }
    // Identity, both releases with their tags and commits, the asset API and
    // its delivery redirect, both releases again, identity again.
    assert_eq!(install.len(), 16, "{install:?}");
    // The receipt is the one an installation from the earlier release itself
    // would have written.
    assert_eq!(kept.revision, earlier_commit());
    let release = kept.release().unwrap();
    assert_eq!((release.release_id, release.asset_id), (EARLIER_ID, 51));
    let source = release.source.as_ref().unwrap();
    assert_eq!(source.publisher_id, 34);
    assert_eq!(source.release_snapshot.id, EARLIER_ID);
    assert_eq!(source.release_binding.tag, EARLIER_TAG);
    assert_eq!(source.release_binding.commit, earlier_commit());
    assert_eq!(source.catalogue_revision, earlier_commit());
    assert_eq!(source.catalogue_sha256, earlier_catalogue_sha256);
    assert!(!kept.requires_development());

    let fresh = install_at(
        &server,
        inspection.select("fresh", "1.0.0").unwrap(),
        &root.0,
    )
    .unwrap()
    .receipt;
    let release = fresh.release().unwrap();
    assert_eq!((release.release_id, release.asset_id), (LATEST_ID, 71));
    let source = release.source.as_ref().unwrap();
    assert_eq!(source.release_binding.tag, LATEST_TAG);
    assert_eq!(source.catalogue_sha256, latest_catalogue_sha256);
    assert_eq!(fresh.revision, latest_commit());

    // Both receipts pass the offline validation that every start repeats.
    let entries = crate::plugin::inventory::read(&root.0).unwrap();
    assert_eq!(entries.len(), 2);
    for entry in &entries {
        assert!(entry.inspected.is_ok(), "{}", entry.receipt.plugin_id);
    }
    // A later check of the same catalogue offers nothing for the carried
    // plugin: its version, bytes and commit are the installed ones.
    let installed = entries
        .iter()
        .find(|entry| entry.receipt.plugin_id == "kept")
        .unwrap();
    let again = server.inspect(true).unwrap();
    assert!(matches!(
        again
            .select("kept", "1.0.0")
            .unwrap()
            .update_verdict(installed, None),
        UpdateVerdict::Current
    ));
}

#[test]
fn a_plugin_installed_from_its_own_release_stays_current_when_a_later_catalogue_carries_it() {
    let root = super::super::super::package::tests::OwnedRoot::new();
    // The earlier release while it was the latest one.
    let (kept, archive) = entry("kept", &earlier_commit());
    let alone = json!({"schema_version":2,"plugins":[{"id":"kept","versions":[kept]}]});
    let first = Server::new(move |request| match path(request) {
        "/repos/example/sample" | "/repositories/12" => normal(request),
        "/repositories/12/releases/latest" => {
            let bytes = alone.to_string().into_bytes();
            Reply::json(
                json!({"id":EARLIER_ID,"draft":false,"prerelease":false,"immutable":true,
                "tag_name":EARLIER_TAG,"assets":[asset(50,"among-plugins.json",&bytes),
                    asset(51,"kept-1.0.0.tar.gz",&archive)]}),
            )
        }
        "/repositories/12/releases/assets/50" => Reply::bytes(alone.to_string().into_bytes()),
        "/repositories/12/releases/assets/51" => Reply::bytes(archive.clone()),
        "/repositories/12/git/ref/tags/v0.9" => Reply::json(
            json!({"ref":"refs/tags/v0.9","object":{"type":"commit","sha":earlier_commit()}}),
        ),
        other => {
            Reply::json(json!({"sha":other.strip_prefix("/repositories/12/git/commits/").unwrap()}))
        }
    });
    let direct = install_at(
        &first,
        first
            .inspect(true)
            .unwrap()
            .select("kept", "1.0.0")
            .unwrap(),
        &root.0,
    )
    .unwrap()
    .receipt;
    drop(first);
    let installed = crate::plugin::inventory::read(&root.0).unwrap().remove(0);
    let (server, _) = serve(World::new());
    let carried = server
        .inspect(true)
        .unwrap()
        .select("kept", "1.0.0")
        .unwrap();
    assert!(matches!(
        carried.clone().update_verdict(&installed, None),
        UpdateVerdict::Current
    ));
    // Had the plugin not been installed, the carried selection would record
    // the very provenance the direct installation did.
    let other = super::super::super::package::tests::OwnedRoot::new();
    let again = install_at(&server, carried, &other.0).unwrap().receipt;
    assert_eq!(again.release(), direct.release());
    assert_eq!(again.revision, direct.revision);
    assert_eq!(again.package_sha256, direct.package_sha256);
}

#[test]
fn a_reference_must_name_another_published_immutable_release_of_the_same_repository() {
    for (label, expected, alter) in [
        (
            "the catalogue's own release",
            "its own catalogue",
            Box::new(|world: &mut World| {
                world.kept_version()["release"] = json!({"id":LATEST_ID,"tag":LATEST_TAG});
            }) as Box<dyn FnOnce(&mut World)>,
        ),
        (
            "the catalogue's own release id under another tag",
            "its own catalogue",
            Box::new(|world: &mut World| {
                world.kept_version()["release"] = json!({"id":LATEST_ID,"tag":EARLIER_TAG});
            }),
        ),
        (
            "the catalogue's own tag under another id",
            "its own catalogue",
            Box::new(|world: &mut World| {
                world.kept_version()["release"] = json!({"id":EARLIER_ID,"tag":LATEST_TAG});
            }),
        ),
        (
            "a release this repository does not have",
            "unavailable in this repository",
            Box::new(|world: &mut World| {
                world.kept_version()["release"] = json!({"id":999,"tag":EARLIER_TAG});
            }),
        ),
        (
            "a release with another tag than stated",
            "another identity or tag",
            Box::new(|world: &mut World| {
                world.kept_version()["release"] = json!({"id":EARLIER_ID,"tag":"v0.8"});
            }),
        ),
        (
            "a release answering with another id",
            "another identity or tag",
            Box::new(|world: &mut World| world.earlier["id"] = json!(6)),
        ),
        (
            "a draft",
            "is not published",
            Box::new(|world: &mut World| world.earlier["draft"] = json!(true)),
        ),
        (
            "a mutable release",
            "is not immutable",
            Box::new(|world: &mut World| world.earlier["immutable"] = json!(false)),
        ),
        (
            "a release without its immutability state",
            "is not immutable",
            Box::new(|world: &mut World| {
                world.earlier.as_object_mut().unwrap().remove("immutable");
            }),
        ),
        (
            "a prerelease behind an ordinary catalogue",
            "refers to a prerelease",
            Box::new(|world: &mut World| world.earlier["prerelease"] = json!(true)),
        ),
        (
            "a release without its prerelease state",
            "prerelease state",
            Box::new(|world: &mut World| {
                world.earlier.as_object_mut().unwrap().remove("prerelease");
            }),
        ),
    ] {
        let problem = refusal(alter);
        assert!(problem.contains(expected), "{label}: {problem}");
    }
}

#[test]
fn a_carried_version_is_bound_to_the_tag_commit_and_assets_of_the_release_it_names() {
    for (label, expected, alter) in [
        (
            "the earlier tag resolves to another commit",
            "disagrees with the resolved release-tag commit",
            Box::new(|world: &mut World| world.earlier_tag_commit = "c".repeat(40))
                as Box<dyn FnOnce(&mut World)>,
        ),
        (
            "the entry claims the new release's commit",
            "disagrees with the resolved release-tag commit",
            Box::new(|world: &mut World| {
                world.kept_version()["revision"] = json!(latest_commit());
            }),
        ),
        (
            "the earlier release holds a package of another size",
            "size or digest disagrees",
            Box::new(|world: &mut World| world.earlier["assets"][1]["size"] = json!(1)),
        ),
        (
            "the earlier release holds other package bytes",
            "size or digest disagrees",
            Box::new(|world: &mut World| {
                world.earlier["assets"][1]["digest"] = json!(format!("sha256:{}", "f".repeat(64)));
            }),
        ),
        (
            "the earlier release lacks the package",
            "does not belong to the selected release",
            Box::new(|world: &mut World| {
                world.earlier["assets"][1]["name"] = json!("other-1.0.0.tar.gz");
            }),
        ),
        (
            "the package is also shipped by the new release",
            "also an asset of the catalogue's own release",
            Box::new(|world: &mut World| {
                let package = world.earlier["assets"][1].clone();
                world.latest["assets"].as_array_mut().unwrap().push(package);
            }),
        ),
        (
            "nothing is shipped by the new release itself",
            "ships none of its own",
            Box::new(|world: &mut World| {
                world.catalogue["plugins"]
                    .as_array_mut()
                    .unwrap()
                    .retain(|plugin| plugin["id"] == "kept");
            }),
        ),
        (
            "an unknown key inside the reference",
            "invalid version fields",
            Box::new(|world: &mut World| {
                world.kept_version()["release"]["repository"] = json!("example/other");
            }),
        ),
    ] {
        let problem = refusal(alter);
        assert!(problem.contains(expected), "{label}: {problem}");
    }
}

#[test]
fn a_fresh_version_still_cannot_take_its_package_or_commit_from_the_earlier_release() {
    // Without a reference the entry is checked against the catalogue's own
    // release exactly as before, even though another release would match.
    let problem = refusal(|world| {
        world
            .kept_version()
            .as_object_mut()
            .unwrap()
            .remove("release");
    });
    assert!(
        problem.contains("disagrees with the resolved release-tag commit"),
        "{problem}"
    );
    let problem = refusal(|world| {
        let version = world.kept_version();
        version.as_object_mut().unwrap().remove("release");
        version["revision"] = json!(latest_commit());
    });
    assert!(
        problem.contains("does not belong to the selected release"),
        "{problem}"
    );
}

#[test]
fn the_number_of_earlier_releases_a_catalogue_may_name_is_bounded_before_any_is_requested() {
    let (server, _) = serve(altered(|world| {
        let kept = world.kept_version().clone();
        // With `kept` itself, one more release than the host follows.
        for index in 0..crate::plugin::install::metadata::MAX_CARRIED_RELEASES as u64 {
            let mut version = kept.clone();
            let id = format!("kept-{index}");
            version["manifest"]["id"] = json!(id);
            version["release"] = json!({"id": 100 + index, "tag": format!("v0.{index}")});
            world.catalogue["plugins"]
                .as_array_mut()
                .unwrap()
                .push(json!({"id": id, "versions": [version]}));
        }
    }));
    let problem = server.inspect(true).unwrap_err();
    assert!(problem.0.contains("more earlier releases"), "{problem}");
    assert_eq!(
        paths(&server, 0),
        [
            "/repos/example/sample",
            "/repositories/12/releases/latest",
            "/repositories/12/releases/assets/70",
            "/repositories/12/git/ref/tags/v1.0",
            format!("/repositories/12/git/commits/{}", latest_commit()).as_str(),
        ]
    );
}

#[test]
fn a_referenced_release_that_changes_after_review_is_not_installed() {
    for (label, alter) in [
        (
            "its inventory gained an asset",
            Box::new(|world: &mut World| {
                world.earlier["assets"]
                    .as_array_mut()
                    .unwrap()
                    .push(asset(52, "NOTICES", b"n"));
            }) as Box<dyn FnOnce(&mut World)>,
        ),
        (
            "its tag moved",
            Box::new(|world: &mut World| world.earlier_tag_commit = "c".repeat(40)),
        ),
        (
            "it was withdrawn",
            Box::new(|world: &mut World| world.earlier = Value::Null),
        ),
        (
            "it became a prerelease",
            Box::new(|world: &mut World| world.earlier["prerelease"] = json!(true)),
        ),
    ] {
        let root = super::super::super::package::tests::OwnedRoot::new();
        let (server, world) = serve(World::new());
        let selection = server
            .inspect(true)
            .unwrap()
            .select("kept", "1.0.0")
            .unwrap();
        alter(&mut world.lock().unwrap());
        let before = server.requests().len();
        assert!(install_at(&server, selection, &root.0).is_err(), "{label}");
        assert!(!root.0.join("installed.json").exists(), "{label}");
        assert!(
            paths(&server, before)
                .iter()
                .all(|path| path != "/repositories/12/releases/assets/51"),
            "{label}: nothing is transferred from a release that is no longer the reviewed one"
        );
    }
}

#[test]
fn a_carried_package_is_checked_against_its_digest_and_the_publisher_recheck() {
    // Other bytes than the earlier release's inventory names.
    let root = super::super::super::package::tests::OwnedRoot::new();
    let (server, world) = serve(World::new());
    let selection = server
        .inspect(true)
        .unwrap()
        .select("kept", "1.0.0")
        .unwrap();
    world.lock().unwrap().kept_archive[20] ^= 1;
    let problem = install_at(&server, selection, &root.0).unwrap_err();
    assert!(problem.0.contains("size or digest disagrees"), "{problem}");
    assert!(!root.0.join("installed.json").exists());

    // The repository changed hands between review and installation.
    let root = super::super::super::package::tests::OwnedRoot::new();
    let (server, world) = serve(World::new());
    let selection = server
        .inspect(true)
        .unwrap()
        .select("kept", "1.0.0")
        .unwrap();
    world.lock().unwrap().publisher = 35;
    let problem = install_at(&server, selection, &root.0).unwrap_err();
    assert!(
        problem.0.contains("publisher identity changed"),
        "{problem}"
    );
    assert!(!root.0.join("installed.json").exists());
}

#[test]
fn a_development_prerelease_may_carry_from_a_prerelease_or_from_a_stable_release() {
    // From an earlier prerelease: the generation is a development one.
    let root = super::super::super::package::tests::OwnedRoot::new();
    let (server, _) = serve(altered(|world| {
        world.on_prerelease_channel();
        world.earlier["prerelease"] = json!(true);
    }));
    let selection = server
        .inspect_prerelease()
        .unwrap()
        .select("kept", "1.0.0")
        .unwrap();
    assert!(selection.is_prerelease());
    let receipt = install_at(&server, selection, &root.0).unwrap().receipt;
    assert_eq!(receipt.release().unwrap().release_id, EARLIER_ID);
    assert!(receipt.requires_development());
    assert!(crate::plugin::inventory::read(&root.0).unwrap()[0]
        .inspected
        .is_ok());

    // From a stable release: the bytes are an ordinary release's, so the
    // generation is ordinary, but a prerelease catalogue offered it and only
    // a development instance may act on such a catalogue.
    let root = super::super::super::package::tests::OwnedRoot::new();
    let (server, _) = serve(altered(World::on_prerelease_channel));
    let selection = server
        .inspect_prerelease()
        .unwrap()
        .select("kept", "1.0.0")
        .unwrap();
    assert!(!selection.is_prerelease());
    assert_eq!(
        InstallJob::start(selection.clone(), root.0.clone())
            .err()
            .map(|problem| problem.0),
        Some(release::DEVELOPMENT_ONLY)
    );
    let receipt = install_at(&server, selection, &root.0).unwrap().receipt;
    assert!(!receipt.requires_development());
    let installed = paths(&server, 0);
    assert!(installed
        .iter()
        .all(|path| !path.ends_with("/releases/latest")));

    // A draft stays refused on the development channel too.
    let (server, _) = serve(altered(|world| {
        world.on_prerelease_channel();
        world.earlier["draft"] = json!(true);
    }));
    let problem = server.inspect_prerelease().unwrap_err();
    assert!(problem.0.contains("is not published"), "{problem}");
}

#[test]
fn requests_for_a_referenced_release_obey_rate_limits_cancellation_and_token_scope() {
    let _serial = JOB_TESTS.lock().unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    let wait = |job: &Job| loop {
        if let Some(outcome) = job.poll() {
            return outcome;
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(2));
    };

    // A refusal of the extra request is reported like any other.
    let world = World::new();
    let server = Server::new(move |request| {
        if path(request) == "/repositories/12/releases/5" {
            Reply::refused(403, Some("0"), None)
        } else {
            world.respond(request)
        }
    });
    let job = server.refresh(12).unwrap();
    let problem = wait(&job).unwrap_err();
    assert!(problem.0.contains("rate limit"), "{problem}");
    assert_eq!(
        job.rate_limit(),
        Some(RateLimit {
            remaining: Some(0),
            retry_after: None
        })
    );
    drop(server);

    // A pending extra request is abandoned on cancellation.
    let world = World::new();
    let server = Server::new(move |request| {
        if path(request) == "/repositories/12/releases/5" {
            Reply::held()
        } else {
            world.respond(request)
        }
    });
    let job = server.refresh(12).unwrap();
    while !server
        .requests()
        .iter()
        .any(|request| path(request) == "/repositories/12/releases/5")
    {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(2));
    }
    job.cancel();
    let problem = wait(&job).unwrap_err();
    assert!(problem.0.contains("cancelled"), "{problem}");
    drop(server);

    // The token reaches the API for the earlier release and never the
    // delivery origin its package redirects to.
    let root = super::super::super::package::tests::OwnedRoot::new();
    let (server, _) = serve(World::new());
    let selection = server
        .inspect_with_token(true, Some(SYNTHETIC_TOKEN))
        .unwrap()
        .select("kept", "1.0.0")
        .unwrap();
    install_with_token(&server, selection, &root.0, Some(SYNTHETIC_TOKEN)).unwrap();
    let mut earlier = 0;
    let mut delivery = 0;
    for request in server.requests() {
        let (host, credential) = host_and_credential(&request);
        assert_eq!(credential, host == "api.github.com", "{request}");
        if host != "api.github.com" {
            delivery += 1;
        } else if path(&request) == "/repositories/12/releases/5" {
            earlier += 1;
        }
    }
    assert_eq!((earlier, delivery), (3, 1));
}

#[test]
fn a_catalogue_without_references_makes_no_request_for_another_release() {
    let root = super::super::super::package::tests::OwnedRoot::new();
    let (server, _) = serve(altered(|world| {
        world.catalogue["plugins"]
            .as_array_mut()
            .unwrap()
            .retain(|plugin| plugin["id"] == "fresh");
    }));
    let inspection = server.inspect(true).unwrap();
    assert_eq!(server.requests().len(), 5);
    let before = server.requests().len();
    let receipt = install_at(
        &server,
        inspection.select("fresh", "1.0.0").unwrap(),
        &root.0,
    )
    .unwrap()
    .receipt;
    assert_eq!(paths(&server, before).len(), 9);
    assert!(paths(&server, 0)
        .iter()
        .all(|path| path != "/repositories/12/releases/5" && !path.contains("v0.9")));
    assert_eq!(receipt.release().unwrap().release_id, LATEST_ID);
}

#[test]
fn a_saved_carried_catalogue_reopens_offline_only_with_its_referenced_releases() {
    use crate::plugin::install::registry::{run, Outcome, Request};
    let root = super::super::super::package::tests::OwnedRoot::new();
    let (server, _) = serve(World::new());
    let inspection = server.inspect(true).unwrap();
    let online = inspection.select("kept", "1.0.0").unwrap();
    drop(server);
    run(
        &root.0,
        Request::Add(Box::new(inspection)),
        || Ok(()),
        || Ok(()),
    )
    .unwrap();
    let Outcome::List(registrations) = run(&root.0, Request::List, || Ok(()), || Ok(())).unwrap()
    else {
        panic!("expected the saved source");
    };
    let Outcome::Cached { inspection, .. } = run(
        &root.0,
        Request::Cached(registrations[0].clone()),
        || Ok(()),
        || Ok(()),
    )
    .unwrap() else {
        panic!("expected the saved catalogue");
    };
    let offline = inspection.select("kept", "1.0.0").unwrap();
    assert_eq!(offline.description(), online.description());
    assert_eq!(offline.source, online.source);

    let path = root.0.join("sources.json");
    let original: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    for (label, alter) in [
        (
            "the referenced release is missing",
            Box::new(|snapshot: &mut Value| {
                snapshot.as_object_mut().unwrap().remove("carried");
            }) as Box<dyn Fn(&mut Value)>,
        ),
        (
            "the referenced release has another commit",
            Box::new(|snapshot: &mut Value| {
                snapshot["carried"][0]["binding"]["commit"] = json!("c".repeat(40));
            }),
        ),
        (
            "the referenced release holds other bytes",
            Box::new(|snapshot: &mut Value| {
                snapshot["carried"][0]["snapshot"]["assets"][1]["sha256"] = json!("f".repeat(64));
            }),
        ),
        (
            "a release the catalogue does not name is stored",
            Box::new(|snapshot: &mut Value| {
                let mut extra = snapshot["carried"][0].clone();
                extra["snapshot"]["id"] = json!(6);
                extra["snapshot"]["tag"] = json!("v0.8");
                extra["binding"]["tag"] = json!("v0.8");
                snapshot["carried"].as_array_mut().unwrap().push(extra);
            }),
        ),
    ] {
        let mut changed = original.clone();
        alter(&mut changed["registrations"][0]["snapshots"][0]);
        std::fs::write(&path, changed.to_string()).unwrap();
        assert!(
            run(&root.0, Request::List, || Ok(()), || Ok(())).is_err(),
            "{label}"
        );
    }
}
