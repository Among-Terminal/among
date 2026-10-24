//! protocol-fixture and separately identified production-clock interoperability.
//! synthetic receipts do not establish authenticated release installation.

use super::*;
use crate::plugin::inventory::Receipt;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::PathBuf;

mod clock;

const KEY: &str = "github:1431055733/protocol-fixture";
const CLIENT_REVISION: &str = "39141b2d6eb7ce5374c0352f54cc41ec5650b6e1";

struct Fixture {
    host: PluginHost,
    root: PathBuf,
    key: String,
}

impl Fixture {
    fn new(plugin: &str, required: &[&str], optional: &[&str]) -> Self {
        Self::with_binary(
            plugin,
            required,
            optional,
            "AMONG_MANAGED_TRANSPORT_FIXTURE",
            "AMONG_MANAGED_TRANSPORT_FIXTURE_SHA256",
        )
    }

    fn with_binary(
        plugin: &str,
        required: &[&str],
        optional: &[&str],
        source_variable: &str,
        digest_variable: &str,
    ) -> Self {
        let parent =
            PathBuf::from(std::env::var_os("AMONG_CHECK_ROOT").expect("explicit owned check root"));
        assert!(parent.is_absolute());
        let root = parent.join(format!("managed-transport-{}", glib::uuid_string_random()));
        std::fs::create_dir(&root).unwrap();
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
        let source = PathBuf::from(
            std::env::var_os(source_variable).expect("build the pinned companion binary first"),
        );
        let metadata = std::fs::symlink_metadata(&source).unwrap();
        assert!(source.is_absolute() && metadata.is_file() && metadata.mode() & 0o111 != 0);
        assert!(metadata.len() <= 32 * 1024 * 1024);
        let bytes = std::fs::read(&source).unwrap();
        let checksum = inventory::checksum(&bytes).unwrap();
        assert_eq!(
            checksum,
            std::env::var(digest_variable).expect("explicit compiled companion binary digest")
        );
        let manifest = json!({"schema_version":1,"id":plugin,"version":"protocol-fixture",
            "name":"Managed transport fixture","description":"Not a production plugin package",
            "entrypoint":"plugin","api":{"major":1,"minor":2},
            "required_features":required,"optional_features":optional})
        .to_string();
        // receipt IDs other than the identity/source commit are synthetic. this
        // assembles a locally inspected fixture, never an authenticated release.
        let receipt = Receipt {
            repository_id: 1431055733,
            owner: "Among-Terminal".into(),
            repository: "among-plugins".into(),
            plugin_id: plugin.into(),
            revision: CLIENT_REVISION.into(),
            manifest_sha256: inventory::checksum(manifest.as_bytes()).unwrap(),
            package_sha256: checksum.clone(),
            release_id: 1,
            asset_id: 2,
            generation: "a".repeat(64),
            installation: None,
            source: None,
        };
        let package = receipt.package_path(&root);
        std::fs::create_dir_all(&package).unwrap();
        let mut path = package.as_path();
        while path.starts_with(&root) {
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
            if path == root {
                break;
            }
            path = path.parent().unwrap();
        }
        let executable = package.join("plugin");
        std::fs::write(&executable, &bytes).unwrap();
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert_eq!(
            inventory::checksum(&std::fs::read(&executable).unwrap()).unwrap(),
            checksum
        );
        assert_eq!(std::fs::metadata(&executable).unwrap().nlink(), 1);
        std::fs::write(package.join("among-plugin.json"), &manifest).unwrap();
        std::fs::set_permissions(
            package.join("among-plugin.json"),
            std::fs::Permissions::from_mode(0o600),
        )
        .unwrap();
        let selection = root.join("installed.json");
        std::fs::write(
            &selection,
            json!({"version":1,"installations":[receipt]}).to_string(),
        )
        .unwrap();
        std::fs::set_permissions(&selection, std::fs::Permissions::from_mode(0o600)).unwrap();
        let mut entries = inventory::read(&root).unwrap();
        assert_eq!(entries.len(), 1);
        let entry = entries.remove(0);
        assert!(
            entry.inspected.is_ok(),
            "fixture must pass real static inspection: {:?}",
            entry.inspected
        );
        let key = entry.key();
        let mut host = PluginHost::new();
        host.update_context(vec![10, 20], context());
        host.register_managed(entry, None);
        assert!(matches!(host.listing()[0].1, PluginState::Disabled));
        assert_eq!(host.supervised_count(), 0);
        println!("client_revision={CLIENT_REVISION} fixture_sha256={checksum} identity={key}");
        Self { host, root, key }
    }

    fn review(&mut self, features: &[&str]) {
        self.host
            .review_access(
                &self.key,
                json!({"owner":"Among-Terminal","repository":"among-plugins","features":features}),
            )
            .unwrap();
    }

    fn enable(&mut self, features: &[&str]) {
        self.review(features);
        self.host.enable(&self.key);
        self.wait(|host| matches!(host.listing()[0].1, PluginState::Running));
        assert!(self.host.running_generation(&self.key).is_some());
        assert_eq!(self.host.supervised_count(), 1);
        assert_eq!(
            self.host.plugins[0].observed,
            Some(crate::plugin::manifest::Api { major: 1, minor: 2 })
        );
    }

    fn wait(&mut self, predicate: impl Fn(&PluginHost) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(4);
        loop {
            self.host.poll();
            if predicate(&self.host) {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "interop host timed out: {:?}",
                self.host.listing()
            );
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    fn capture(&mut self, scope: &str) -> (u64, String) {
        assert!(self.host.results.is_empty());
        let id = self
            .host
            .request_status(&self.key, scope, &Context::default())
            .expect("live status request");
        let io = self.host.plugins[0].io.as_ref().unwrap();
        // do not poll: the worker delivers authentic stdout to this channel;
        // the test delays only its handoff to the normal managed response handler.
        let line = io
            .process
            .frames
            .recv_timeout(Duration::from_secs(3))
            .expect("actual child status frame");
        let response: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(response["type"], "status");
        assert_eq!(response["request_id"], id);
        assert_eq!(response["session"], self.host.world.session);
        assert_eq!(response["generation"], io.generation);
        assert!(self.host.plugins[0].inflight.contains_key(&id));
        assert!(self.host.results.is_empty());
        (id, line)
    }

    fn accept(&mut self, line: &str, scope: &str, expected: &str) {
        self.host.handle_managed_line(0, line);
        let results = self.host.drain_results();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].plugin, self.key);
        assert_eq!(results[0].scope, scope);
        assert_eq!(results[0].fragments[0].text.as_deref(), Some(expected));
        assert_eq!(results[0].refresh, Refresh::IntervalMs(1000));
    }

    fn stop(&mut self) {
        self.host.shutdown();
        self.wait(PluginHost::is_stopped);
        assert_eq!(self.host.supervised_count(), 0);
        assert!(!self.host.desired_enabled(&self.key));
        assert!(self.host.running_generation(&self.key).is_none());
        assert!(self.host.drain_results().is_empty());
        println!(
            "retirement_confirmed identity={} root={}",
            self.key,
            self.root.display()
        );
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.host.shutdown();
        let deadline = Instant::now() + Duration::from_secs(3);
        while !self.host.is_stopped() && Instant::now() < deadline {
            self.host.poll();
            std::thread::sleep(Duration::from_millis(2));
        }
        // retain the owned package/receipt even on failure; teardown is not
        // acceptance. each successful test explicitly asserts stopped ownership.
    }
}

fn context() -> Context {
    Context {
        active_workspace: 0,
        workspace_count: 2,
        focused_pane: Some(1),
        terminals: vec![
            crate::plugin::TerminalInfo {
                pane: 1,
                workspace: 0,
                cwd: Some("/данные/日本語".into()),
                title: Some("private title".into()),
                running: true,
            },
            crate::plugin::TerminalInfo {
                pane: 2,
                workspace: 1,
                cwd: Some("/sibling-private".into()),
                title: None,
                running: false,
            },
        ],
    }
}

#[test]
fn real_companion_admits_only_after_review_and_publishes_correlated_status() {
    let mut fixture = Fixture::new("protocol-fixture", &["status"], &[]);
    assert_eq!(fixture.key, KEY);
    fixture.host.enable(KEY);
    assert!(matches!(
        fixture.host.listing()[0].1,
        PluginState::Unavailable { .. }
    ));
    assert_eq!(fixture.host.supervised_count(), 0);
    assert!(fixture.host.running_generation(KEY).is_none());
    fixture.host.disable(KEY);
    fixture.enable(&["status"]);
    assert_eq!(
        fixture.host.plugins[0].runtime.as_ref().unwrap().grants,
        BTreeSet::from(["status".to_string()])
    );
    assert!(fixture
        .host
        .request_status(KEY, "pane_footer:1", &Context::default())
        .is_none());
    fixture
        .host
        .request_status(KEY, "app_footer", &Context::default())
        .unwrap();
    fixture.wait(|host| !host.results.is_empty());
    let results = fixture.host.drain_results();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].scope, "app_footer");
    assert_eq!(
        results[0].fragments[0].text.as_deref(),
        Some("1:app_footer")
    );
    assert!(results[0].fragments[0].tooltip.is_none());
    assert_eq!(fixture.host.configuration(KEY), Configuration::NotApplied);
    fixture.stop();
}

#[test]
fn real_companion_negotiates_subset_and_scoped_cwd_without_sibling_fields() {
    let mut fixture = Fixture::new(
        "protocol-fixture",
        &["status"],
        &["settings", "context.terminals", "context.cwd"],
    );
    assert!(fixture.host.review_access(KEY,json!({"owner":"Among-Terminal","repository":"among-plugins","features":["status","context.cwd"]})).is_err());
    assert_eq!(fixture.host.supervised_count(), 0);
    fixture.enable(&["status", "settings", "context.terminals", "context.cwd"]);
    assert_eq!(
        fixture.host.plugins[0].runtime.as_ref().unwrap().grants,
        BTreeSet::from([
            "status".to_string(),
            "context.terminals".to_string(),
            "context.cwd".to_string()
        ])
    );
    assert_eq!(fixture.host.configuration(KEY), Configuration::NotApplied);
    let (id, line) = fixture.capture("pane_footer:1");
    let value: Value = serde_json::from_str(&line).unwrap();
    let terminals: Value =
        serde_json::from_str(value["fragments"][0]["tooltip"].as_str().unwrap()).unwrap();
    assert_eq!(
        terminals,
        json!([{"pane":"1","workspace":"10","cwd":"/данные/日本語"}])
    );
    assert!(!line.contains("/sibling-private") && !line.contains("private title"));
    fixture.accept(&line, "pane_footer:1", "1:pane_footer");
    assert!(!fixture.host.plugins[0].inflight.contains_key(&id));
    fixture.stop();
}

#[test]
fn real_companion_identity_mismatch_never_becomes_admitted() {
    let mut fixture = Fixture::new("wrong-protocol-fixture", &["status"], &[]);
    fixture.review(&["status"]);
    fixture.host.enable(&fixture.key);
    fixture.wait(|host| {
        host.plugins[0].io.is_none() && matches!(host.listing()[0].1, PluginState::Faulted { .. })
    });
    assert!(fixture.host.plugins[0].observed.is_none());
    assert!(fixture.host.running_generation(&fixture.key).is_none());
    assert!(fixture.host.results.is_empty());
    fixture.stop();
}

#[test]
fn real_companion_closed_target_reply_releases_credit_without_publishing() {
    let mut fixture = Fixture::new(
        "protocol-fixture",
        &["status", "context.terminals", "context.cwd"],
        &[],
    );
    fixture.enable(&["status", "context.terminals", "context.cwd"]);
    let (id, line) = fixture.capture("pane_footer:1");
    let mut closed = context();
    closed.terminals.remove(0);
    closed.focused_pane = Some(2);
    fixture.host.update_context(vec![10, 20], closed);
    assert!(fixture.host.plugins[0].inflight.contains_key(&id));
    assert!(!fixture.host.plugins[0]
        .runtime
        .as_ref()
        .unwrap()
        .scopes
        .contains_key(&id));
    fixture.host.handle_managed_line(0, &line);
    assert!(!fixture.host.plugins[0].inflight.contains_key(&id));
    assert!(fixture.host.results.is_empty());
    assert!(matches!(fixture.host.listing()[0].1, PluginState::Running));
    let (_, current) = fixture.capture("pane_footer:2");
    fixture.accept(&current, "pane_footer:2", "2:pane_footer");
    fixture.stop();
}

#[test]
fn real_companion_retired_generation_reply_cannot_consume_current_credit() {
    let mut fixture = Fixture::new("protocol-fixture", &["status"], &[]);
    fixture.enable(&["status"]);
    let (_, old) = fixture.capture("app_footer");
    let generation = fixture.host.running_generation(KEY).unwrap();
    let session = fixture.host.world.session.clone();
    fixture.stop();
    fixture.host.enable(KEY);
    fixture.wait(|host| matches!(host.listing()[0].1, PluginState::Running));
    assert_ne!(fixture.host.running_generation(KEY).unwrap(), generation);
    assert_eq!(fixture.host.world.session, session);
    let (current_id, current) = fixture.capture("app_footer");
    fixture.host.handle_managed_line(0, &old);
    assert!(fixture.host.plugins[0].inflight.contains_key(&current_id));
    assert!(fixture.host.results.is_empty());
    // the real old reply complements this isolated same-request check. retirement
    // also clears old credits, so replay alone cannot prove the generation guard.
    let mut stale: Value = serde_json::from_str(&current).unwrap();
    stale["generation"] = json!(generation);
    fixture.host.handle_managed_line(0, &stale.to_string());
    assert!(fixture.host.plugins[0].inflight.contains_key(&current_id));
    assert!(fixture.host.results.is_empty());
    stale = serde_json::from_str(&current).unwrap();
    stale["session"] = json!("retired-session");
    fixture.host.handle_managed_line(0, &stale.to_string());
    assert!(fixture.host.plugins[0].inflight.contains_key(&current_id));
    assert!(fixture.host.results.is_empty());
    fixture.accept(&current, "app_footer", "1:app_footer");
    fixture.host.handle_managed_line(0, &current);
    assert!(fixture.host.results.is_empty());
    fixture.stop();
}
