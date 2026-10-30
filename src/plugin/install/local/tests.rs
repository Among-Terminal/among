use super::*;
use crate::plugin::install::package::tests::{fixture, OwnedRoot};
use crate::plugin::install::source::tests::JOB_TESTS;
use serde_json::{json, Value};
use std::os::unix::fs::PermissionsExt;
use std::time::{Duration, Instant};

/// An author's build directory: the catalogue and its package file.
struct Author {
    directory: PathBuf,
    asset: String,
    package: Vec<u8>,
    version: Value,
}

impl Author {
    fn new(root: &OwnedRoot) -> Self {
        let directory = root.0.join("author");
        std::fs::create_dir(&directory).unwrap();
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o755)).unwrap();
        let (version, package) = fixture();
        let asset = version["artifacts"][0]["asset"]
            .as_str()
            .unwrap()
            .to_string();
        let author = Self {
            directory,
            asset,
            package,
            version,
        };
        author.write_catalogue(&author.version);
        std::fs::write(author.directory.join(&author.asset), &author.package).unwrap();
        std::fs::set_permissions(
            author.directory.join(&author.asset),
            std::fs::Permissions::from_mode(0o644),
        )
        .unwrap();
        author
    }

    fn write_catalogue(&self, version: &Value) {
        let mut version = version.clone();
        version.as_object_mut().unwrap().remove("schema_version");
        let catalogue =
            json!({"schema_version":2,"plugins":[{"id":"sample","versions":[version]}]});
        std::fs::write(self.catalogue(), catalogue.to_string()).unwrap();
        std::fs::set_permissions(self.catalogue(), std::fs::Permissions::from_mode(0o644)).unwrap();
    }

    fn catalogue(&self) -> PathBuf {
        self.directory.join("among-plugins.json")
    }

    fn selection(&self) -> LocalSelection {
        LocalCatalogue::read(&self.catalogue())
            .unwrap()
            .selection(
                "sample",
                self.version["manifest"]["version"].as_str().unwrap(),
            )
            .unwrap()
    }
}

fn finish(job: InstallJob) -> Result<store::Commit, Problem> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(outcome) = job.poll() {
            return outcome;
        }
        assert!(
            Instant::now() < deadline,
            "local installation did not finish"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
}

fn store(root: &OwnedRoot) -> PathBuf {
    let store = root.0.join("managed");
    if !store.exists() {
        std::fs::create_dir(&store).unwrap();
        std::fs::set_permissions(&store, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    store
}

#[test]
fn an_ordinary_instance_cannot_open_select_or_install_a_local_catalogue() {
    let root = OwnedRoot::new();
    let author = Author::new(&root);
    // Unit tests never fix development mode on, so this is the ordinary run.
    assert!(!crate::plugin::development::enabled());
    assert!(LocalCatalogue::open(&author.catalogue())
        .unwrap_err()
        .0
        .contains("--plugin-development"));
    let catalogue = LocalCatalogue::read(&author.catalogue()).unwrap();
    assert!(catalogue.select("sample", "2026.10").is_err());
    assert!(start(author.selection(), store(&root)).is_err());
    assert!(!store(&root).join("installed.json").exists());
}

#[test]
fn a_local_package_installs_into_the_development_namespace_without_invented_provenance() {
    let _serial = JOB_TESTS.lock().unwrap();
    let root = OwnedRoot::new();
    let author = Author::new(&root);
    let selection = author.selection();
    assert_eq!(selection.identity(), "development/sample");
    let review = selection.review_description();
    assert!(review.contains(inventory::LOCAL_MISSING_PROVENANCE));
    assert!(review.contains("--plugin-development"));
    let job = spawn(selection, store(&root)).unwrap();
    assert_eq!(job.operation().0, "development/sample");
    let commit = finish(job).unwrap();
    let receipt = &commit.receipt;
    assert!(receipt.release().is_none());
    assert!(receipt.requires_development());
    assert_eq!(receipt.key(), "development/sample");
    let local = receipt.local().unwrap();
    assert_eq!(local.asset, author.asset);
    assert_eq!(
        local.catalogue_sha256,
        inventory::checksum(&std::fs::read(author.catalogue()).unwrap()).unwrap()
    );
    assert_eq!(
        receipt.package_sha256,
        inventory::checksum(&author.package).unwrap()
    );
    assert!(receipt.installation.is_some());
    assert!(store(&root)
        .join("generations/development/sample")
        .join(&receipt.generation)
        .join("bin/plugin")
        .is_file());
    let entries = inventory::read(&store(&root)).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].key(), "development/sample");
    assert_eq!(entries[0].receipt, commit.receipt);
    assert!(entries[0].inspected.is_ok());
    // The stored record has no repository, release or asset fields at all.
    let stored: Value =
        serde_json::from_slice(&std::fs::read(store(&root).join("installed.json")).unwrap())
            .unwrap();
    let record = stored["installations"][0].as_object().unwrap();
    for absent in [
        "repository_id",
        "owner",
        "repository",
        "release_id",
        "asset_id",
        "source",
    ] {
        assert!(!record.contains_key(absent), "{absent}");
    }
    assert_eq!(record["development"]["asset"], json!(author.asset));
}

#[test]
fn local_packages_keep_every_package_check() {
    let _serial = JOB_TESTS.lock().unwrap();
    for case in [
        "package-link",
        "package-group-writable",
        "package-size",
        "package-digest",
        "package-missing",
        "directory-group-writable",
        "catalogue-link",
        "member-digest",
    ] {
        let root = OwnedRoot::new();
        let author = Author::new(&root);
        let package = author.directory.join(&author.asset);
        let outcome = (|| {
            match case {
                "package-link" => {
                    let moved = author.directory.join("elsewhere.tar.gz");
                    std::fs::rename(&package, &moved).unwrap();
                    std::os::unix::fs::symlink(&moved, &package).unwrap();
                }
                "package-group-writable" => {
                    std::fs::set_permissions(&package, std::fs::Permissions::from_mode(0o664))
                        .unwrap()
                }
                "package-size" => {
                    let mut bytes = author.package.clone();
                    bytes.push(0);
                    std::fs::write(&package, bytes).unwrap();
                }
                "package-digest" => {
                    let mut bytes = author.package.clone();
                    let last = bytes.len() - 1;
                    bytes[last] ^= 1;
                    std::fs::write(&package, bytes).unwrap();
                }
                "package-missing" => std::fs::remove_file(&package).unwrap(),
                "directory-group-writable" => std::fs::set_permissions(
                    &author.directory,
                    std::fs::Permissions::from_mode(0o775),
                )
                .unwrap(),
                "catalogue-link" => {
                    let moved = author.directory.join("moved.json");
                    std::fs::rename(author.catalogue(), &moved).unwrap();
                    std::os::unix::fs::symlink(&moved, author.catalogue()).unwrap();
                }
                "member-digest" => {
                    // A catalogue whose member digest disagrees with the archive:
                    // the archive and package digest match, the content does not.
                    let mut version = author.version.clone();
                    version["artifacts"][0]["members"][1]["sha256"] = json!("e".repeat(64));
                    author.write_catalogue(&version);
                }
                _ => unreachable!(),
            }
            let selection = LocalCatalogue::read(&author.catalogue())?.selection(
                "sample",
                author.version["manifest"]["version"].as_str().unwrap(),
            )?;
            finish(spawn(selection, store(&root))?)
        })();
        assert!(outcome.is_err(), "{case}");
        assert!(inventory::read(&store(&root)).unwrap().is_empty(), "{case}");
    }
}

#[test]
fn a_catalogue_rebuilt_after_review_is_not_committed() {
    let _serial = JOB_TESTS.lock().unwrap();
    let root = OwnedRoot::new();
    let author = Author::new(&root);
    let selection = author.selection();
    let mut version = author.version.clone();
    version["manifest"]["description"] = json!("Rebuilt after review");
    author.write_catalogue(&version);
    let problem = finish(spawn(selection, store(&root)).unwrap()).unwrap_err();
    assert!(problem.0.contains("changed since review"), "{problem}");
    assert!(inventory::read(&store(&root)).unwrap().is_empty());
}

#[test]
fn replacing_a_development_install_needs_confirmation_and_inherits_no_consent() {
    let _serial = JOB_TESTS.lock().unwrap();
    let root = OwnedRoot::new();
    let author = Author::new(&root);
    let first = finish(spawn(author.selection(), store(&root)).unwrap())
        .unwrap()
        .receipt;
    let grant = first.review(vec!["status".into()]);
    assert!(first.matches_review(&grant));
    let refused = finish(spawn(author.selection(), store(&root)).unwrap()).unwrap_err();
    assert!(refused.0.contains("already installed"), "{refused}");
    assert_eq!(inventory::read(&store(&root)).unwrap()[0].receipt, first);
    let second =
        finish(spawn(author.selection().replacing(&first).unwrap(), store(&root)).unwrap())
            .unwrap()
            .receipt;
    assert_eq!(second.key(), first.key());
    assert_ne!(second.generation, first.generation);
    assert_ne!(second.installation, first.installation);
    assert!(
        !second.matches_review(&grant),
        "an approval of the replaced install must not cover the replacement"
    );
    let entries = inventory::read(&store(&root)).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].receipt, second);
    assert_eq!(entries[0].previous.as_ref(), Some(&first));
    // A stale confirmation of a generation that is no longer selected fails.
    let stale = finish(spawn(author.selection().replacing(&first).unwrap(), store(&root)).unwrap())
        .unwrap_err();
    assert!(stale.0.contains("stale"), "{stale}");
    let release = inventory::tests::fixture(&root.0.join("release-store"));
    assert!(author.selection().replacing(&release).is_err());
}

#[test]
fn a_development_install_is_listed_but_not_started_in_an_ordinary_run() {
    let root = OwnedRoot::new();
    let receipt = inventory::tests::development_fixture(&root.0);
    let entry = inventory::read(&root.0).unwrap().remove(0);
    assert_eq!(entry.key(), "development/sample");
    let mut host = crate::plugin::host::PluginHost::new();
    host.register_managed(
        entry,
        Some(&json!({"enabled":true,"installation":receipt.installation,
            "review":receipt.review(vec!["status".into()])})),
    );
    assert_eq!(host.supervised_count(), 0);
    let listing = host.listing();
    assert_eq!(listing.len(), 1);
    assert!(matches!(&listing[0].1,
        crate::plugin::host::PluginState::Unavailable { reason }
            if reason == crate::plugin::host::DEVELOPMENT_ONLY));
    host.poll();
    assert_eq!(host.supervised_count(), 0);
    assert!(!root.0.join("executed").exists());
}
