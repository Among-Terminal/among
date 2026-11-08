use super::*;
use crate::plugin::install::{
    metadata,
    package::tests::OwnedRoot,
    registry::{self, Outcome, Request},
};
use crate::plugin::inventory;
use serde_json::json;

const DAY: u64 = 24 * 60 * 60;

fn origin(repository_id: u64, owner: &str) -> Origin {
    Origin {
        repository_id,
        publisher_id: 34,
        owner: owner.into(),
        repository: "plugins".into(),
    }
}

/// One catalogue plugin: identity, display name, version, manifest digest.
pub(crate) type Spec<'a> = (&'a str, &'a str, &'a str, &'a str);

/// A synthetic release catalogue of the listed plugins, for views and models
/// that need saved snapshots without a network.
pub(crate) fn inspection(origin: Origin, plugins: &[Spec<'_>]) -> Inspection {
    Inspection::from_bytes(
        origin,
        "a".repeat(40),
        document(plugins).to_string().into_bytes(),
    )
    .unwrap()
}

/// The same catalogue bytes can be served by an owned TLS fixture, so views
/// exercise the real client instead of fabricating a finished network job.
pub(crate) fn document(plugins: &[Spec<'_>]) -> Value {
    let platform = metadata::Platform::current().unwrap();
    document_for_architecture(plugins, &platform.architecture)
}

fn document_for_architecture(plugins: &[Spec<'_>], architecture: &metadata::Architecture) -> Value {
    let architecture = match architecture {
        metadata::Architecture::Amd64 => "amd64",
        metadata::Architecture::Arm64 => "arm64",
    };
    let plugins: Vec<_> = plugins
        .iter()
        .map(|(id, name, version, manifest_sha256)| {
            let mut entry = metadata::tests::single();
            entry.as_object_mut().unwrap().remove("schema_version");
            entry["manifest"]["id"] = json!(id);
            entry["manifest"]["name"] = json!(name);
            entry["manifest"]["version"] = json!(version);
            entry["manifest_sha256"] = json!(manifest_sha256);
            // Listing selects against the real host before comparing versions.
            // Its fixture must offer that host's architecture on both CI runners.
            entry["artifacts"][0]["architecture"] = json!(architecture);
            // the packaged manifest file must carry the declared digest.
            entry["artifacts"][0]["members"][0]["sha256"] = json!(manifest_sha256);
            entry["artifacts"][0]["asset"] = json!(format!("{id}.tar.gz"));
            // the installed fixture was packaged with this digest.
            entry["artifacts"][0]["sha256"] = json!(digest('b'));
            json!({"id": id, "versions": [entry]})
        })
        .collect();
    json!({"schema_version": 2, "plugins": plugins})
}

#[test]
fn listing_catalogue_fixtures_offer_a_compatible_artifact_on_both_native_architectures() {
    for architecture in [metadata::Architecture::Amd64, metadata::Architecture::Arm64] {
        let document = document_for_architecture(
            &[("sample", "Sample", "1.0.0", &digest('b'))],
            &architecture,
        );
        let catalogue = metadata::Catalogue::parse(document.to_string().as_bytes()).unwrap();
        let platform = metadata::Platform {
            architecture,
            libc: Some((metadata::Libc::Glibc, "2.35".into())),
        };
        let selected = catalogue.plugins[0].versions[0].select(&platform).unwrap();
        assert_eq!(selected.architecture, platform.architecture);
    }
}

fn catalogue(origin: Origin, official: bool, plugins: &[Spec<'_>]) -> SourceCatalogue {
    let mut observed = inspection(origin, plugins);
    observed.retrieved_at = 1_000;
    SourceCatalogue::from_inspection(observed, official)
}

/// An installed plugin `sample` from repository 12 at the given version.
fn installed(version: &str) -> (OwnedRoot, Entry) {
    let root = OwnedRoot::new();
    inventory::tests::fixture_version(&root.0, version);
    let entry = inventory::read(&root.0).unwrap().remove(0);
    (root, entry)
}

fn plain(entry: &Entry) -> Installed<'_> {
    Installed {
        entry,
        stored: None,
    }
}

pub(crate) fn digest(letter: char) -> String {
    letter.to_string().repeat(64)
}

#[test]
fn equal_display_names_from_two_sources_stay_two_rows() {
    let sources = [
        catalogue(
            origin(12, "first"),
            false,
            &[("notes", "Notes", "1.0.0", &digest('b'))],
        ),
        catalogue(
            origin(13, "second"),
            false,
            &[("notes", "Notes", "1.0.0", &digest('b'))],
        ),
    ];
    let listing = assemble(&sources, &[], 1_000);
    assert!(listing.several_sources());
    assert_eq!(listing.available.len(), 2);
    let mut keys: Vec<_> = listing.available.iter().map(Offer::key).collect();
    keys.sort();
    assert_eq!(keys, ["github:12/notes", "github:13/notes"]);
    assert!(listing.available.iter().all(|offer| offer.name == "Notes"));
    let owners: Vec<_> = listing
        .available
        .iter()
        .map(|offer| offer.source.owner.as_str())
        .collect();
    assert_eq!(owners.len(), 2);
    assert_ne!(owners[0], owners[1]);
}

#[test]
fn one_source_needs_no_source_label() {
    let sources = [catalogue(
        origin(12, "first"),
        false,
        &[("notes", "Notes", "1.0.0", &digest('b'))],
    )];
    assert!(!assemble(&sources, &[], 1_000).several_sources());
}

#[test]
fn a_plugin_of_a_removed_source_is_marked_and_nothing_is_offered_for_it() {
    let (_root, entry) = installed("1.0.0");
    // only repository 13 is registered; the installed plugin came from 12.
    let sources = [catalogue(
        origin(13, "other"),
        false,
        &[("notes", "Notes", "1.0.0", &digest('b'))],
    )];
    let listing = assemble(&sources, &[plain(&entry)], 1_000);
    assert_eq!(listing.installed.len(), 1);
    assert_eq!(
        listing.installed[0].link,
        Link::SourceRemoved {
            repository_id: 12,
            owner: "example".into(),
            repository: "sample".into()
        }
    );
    assert!(matches!(listing.installed[0].standing, Standing::Unknown));
}

#[test]
fn a_local_install_is_neither_updated_nor_called_removed() {
    let root = OwnedRoot::new();
    inventory::tests::development_fixture(&root.0);
    let entry = inventory::read(&root.0).unwrap().remove(0);
    let listing = assemble(&[], &[plain(&entry)], 1_000);
    assert_eq!(listing.installed[0].link, Link::Local);
    assert!(matches!(listing.installed[0].standing, Standing::Unknown));
}

#[test]
fn a_newer_saved_version_is_an_update_and_leaves_available() {
    let (_root, entry) = installed("1.0.0");
    let sources = [catalogue(
        origin(12, "example"),
        false,
        &[
            ("sample", "Sample", "1.1.0", &digest('b')),
            ("other", "Other", "1.0.0", &digest('b')),
        ],
    )];
    let listing = assemble(&sources, &[plain(&entry)], 1_000);
    assert!(matches!(listing.installed[0].standing, Standing::Update(_)));
    assert!(matches!(listing.installed[0].link, Link::Source(_)));
    let offered: Vec<_> = listing
        .available
        .iter()
        .map(|offer| offer.plugin_id.as_str())
        .collect();
    assert_eq!(
        offered,
        ["other"],
        "an installed plugin is not offered again"
    );
}

#[test]
fn the_same_version_with_the_same_bytes_is_current() {
    let (_root, entry) = installed("1.0.0");
    let same = entry.receipt.manifest_sha256.clone();
    let sources = [catalogue(
        origin(12, "example"),
        false,
        &[("sample", "Sample", "1.0.0", &same)],
    )];
    let listing = assemble(&sources, &[plain(&entry)], 1_000);
    assert!(matches!(listing.installed[0].standing, Standing::Current));
}

#[test]
fn the_same_version_with_different_bytes_is_withheld_with_its_reason() {
    let (_root, entry) = installed("1.0.0");
    let sources = [catalogue(
        origin(12, "example"),
        false,
        &[("sample", "Sample", "1.0.0", &digest('e'))],
    )];
    let listing = assemble(&sources, &[plain(&entry)], 1_000);
    match &listing.installed[0].standing {
        Standing::Withheld(problem) => {
            assert!(problem.0.contains("publisher inconsistency"), "{problem}")
        }
        other => panic!("expected a withheld update, got {other:?}"),
    }
}

#[test]
fn a_lower_offer_is_current_and_an_unordered_one_is_withheld() {
    let (_root, entry) = installed("1.0.0");
    let lower = [catalogue(
        origin(12, "example"),
        false,
        &[("sample", "Sample", "0.9.0", &digest('b'))],
    )];
    assert!(matches!(
        assemble(&lower, &[plain(&entry)], 1_000).installed[0].standing,
        Standing::Current
    ));
    let unordered = [catalogue(
        origin(12, "example"),
        false,
        &[("sample", "Sample", "nightly", &digest('b'))],
    )];
    assert!(matches!(
        assemble(&unordered, &[plain(&entry)], 1_000).installed[0].standing,
        Standing::Withheld(_)
    ));
}

#[test]
fn a_source_without_a_snapshot_lists_nothing_and_decides_nothing() {
    let (_root, entry) = installed("1.0.0");
    let empty = SourceCatalogue {
        origin: origin(12, "example"),
        official: true,
        state: CatalogueState::NoSnapshot,
    };
    let listing = assemble(&[empty], &[plain(&entry)], 1_000);
    assert!(listing.available.is_empty());
    assert_eq!(listing.sources[0].state, SourceState::NoSnapshot);
    assert!(matches!(listing.installed[0].link, Link::Source(_)));
    assert!(matches!(listing.installed[0].standing, Standing::Unknown));
}

#[test]
fn a_stale_snapshot_still_lists_its_plugins_and_says_it_is_stale() {
    let sources = [catalogue(
        origin(12, "example"),
        false,
        &[("notes", "Notes", "1.0.0", &digest('b'))],
    )];
    let fresh = assemble(&sources, &[], 1_000 + DAY - 1);
    assert_eq!(
        fresh.sources[0].state,
        SourceState::Saved {
            retrieved_at: 1_000,
            stale: false
        }
    );
    let old = assemble(&sources, &[], 1_000 + DAY);
    assert_eq!(
        old.sources[0].state,
        SourceState::Saved {
            retrieved_at: 1_000,
            stale: true
        }
    );
    assert_eq!(old.available.len(), 1);
    // a clock set before the snapshot cannot establish an age.
    assert!(matches!(
        assemble(&sources, &[], 10).sources[0].state,
        SourceState::Saved { stale: true, .. }
    ));
}

#[test]
fn an_unreadable_source_is_reported_and_does_not_hide_the_others() {
    let sources = [
        SourceCatalogue {
            origin: origin(12, "example"),
            official: false,
            state: CatalogueState::Unreadable(Problem(
                "cached metadata timestamp or digest is invalid",
            )),
        },
        catalogue(
            origin(13, "other"),
            false,
            &[("notes", "Notes", "1.0.0", &digest('b'))],
        ),
    ];
    let listing = assemble(&sources, &[], 1_000);
    assert!(matches!(
        listing.sources[0].state,
        SourceState::Unreadable(_)
    ));
    assert_eq!(listing.available.len(), 1);
}

#[test]
fn official_offers_come_first() {
    let sources = [
        catalogue(
            origin(13, "other"),
            false,
            &[("aaa", "Aaa", "1.0.0", &digest('b'))],
        ),
        catalogue(
            origin(12, "example"),
            true,
            &[("zzz", "Zzz", "1.0.0", &digest('b'))],
        ),
    ];
    let listing = assemble(&sources, &[], 1_000);
    assert_eq!(listing.available[0].plugin_id, "zzz");
}

#[test]
fn the_list_is_built_from_saved_files_without_any_client() {
    let root = OwnedRoot::new();
    let observed = inspection(
        origin(12, "example"),
        &[("notes", "Notes", "1.0.0", &digest('b'))],
    );
    registry::run(
        &root.0,
        Request::Add(Box::new(observed)),
        || Ok(()),
        || Ok(()),
    )
    .unwrap();
    let Outcome::List(saved) = registry::run(&root.0, Request::List, || Ok(()), || Ok(())).unwrap()
    else {
        panic!("expected the saved registrations");
    };
    let mut registrations = registry::effective_sources(saved);
    assert_eq!(
        registrations.len(),
        2,
        "the official source is always present"
    );
    let sources: Vec<_> = registrations
        .drain(..)
        .map(|registration| SourceCatalogue::from_registration(&registration))
        .collect();
    // no `Client`, runtime or URL exists in this test: only files were read.
    let listing = assemble(&sources, &[], registry::now().unwrap());
    assert!(
        matches!(listing.sources[0].state, SourceState::NoSnapshot),
        "official, never checked"
    );
    assert!(matches!(
        listing.sources[1].state,
        SourceState::Saved { stale: false, .. }
    ));
    assert_eq!(listing.available.len(), 1);
    assert_eq!(listing.available[0].name, "Notes");
    assert!(listing.available[0].selection.is_ok());
}
