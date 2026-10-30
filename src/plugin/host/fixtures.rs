//! managed packages for host/GTK tests. fixture commands cannot enter the
//! production host: tests publish receipts/manifests and grant access explicitly.

use super::*;
use serde_json::json;
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};

const FEATURES: [&str; 6] = [
    "status",
    "context.terminals",
    "context.cwd",
    "context.title",
    "context.process",
    "events.terminal",
];

fn fixture_entry(spec: &PluginSpec) -> inventory::Entry {
    let local_id = spec
        .name
        .strip_prefix("github:12/")
        .expect("explicit managed fixture key");
    assert!(super::super::manifest::local_id(local_id));
    let root = std::env::var_os("AMONG_CHECK_ROOT")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("opencode"))
        .join(format!(
            "managed-host-fixture-{}",
            glib::uuid_string_random()
        ));
    let mut receipt = inventory::tests::fixture(&root);
    let original = receipt.package_path(&root);
    receipt.plugin_id = local_id.into();
    let package = receipt.package_path(&root);
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(package.parent().unwrap())
        .unwrap();
    std::fs::rename(original, &package).unwrap();
    let manifest = json!({
            "schema_version":1,"id":local_id,"version":"1.0","name":local_id,
            "description":"Managed host test fixture","entrypoint":"plugin",
            "api":{"major":1,"minor":2},"required_features":["status"],
            "optional_features":["context.terminals","context.cwd","context.title","context.process","events.terminal"]
        }).to_string();
    std::fs::write(package.join("among-plugin.json"), &manifest).unwrap();
    receipt.manifest_sha256 = inventory::checksum(manifest.as_bytes()).unwrap();
    if let Some(program) = spec
        .command
        .first()
        .filter(|program| std::path::Path::new(program).is_file())
    {
        std::fs::copy(program, package.join("plugin")).unwrap();
        std::fs::set_permissions(
            package.join("plugin"),
            std::fs::Permissions::from_mode(0o700),
        )
        .unwrap();
        let mode = spec.command.get(1).map(String::as_str).unwrap_or("echo");
        std::fs::write(package.join("among-fake-mode"), format!("transport-{mode}")).unwrap();
    } else {
        std::fs::remove_file(package.join("plugin")).unwrap();
    }
    std::fs::write(
        root.join("installed.json"),
        json!({"version":1,"installations":[receipt]}).to_string(),
    )
    .unwrap();
    inventory::read(&root).unwrap().remove(0)
}

impl PluginHost {
    pub(crate) fn install_fixture_inventory(specs: &[PluginSpec]) {
        let root = inventory::root().unwrap();
        crate::test_isolation::assert_below_check_root(&root);
        assert!(!root.join("installed.json").exists());
        let directory = crate::settings::directory().unwrap();
        let preferences = directory.join("managed-plugins.json");
        assert!(!preferences.exists());
        let mut receipts = Vec::new();
        let mut local = serde_json::Map::new();
        for spec in specs {
            let entry = fixture_entry(spec);
            let package = entry
                .inspected
                .as_ref()
                .unwrap()
                .executable
                .parent()
                .unwrap();
            let target = entry.receipt.package_path(&root);
            std::fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(target.parent().unwrap())
                .unwrap();
            std::fs::rename(package, target).unwrap();
            local.insert(spec.name.clone(), json!({"enabled":spec.enabled,"review":{"owner":"example","repository":"sample","features":FEATURES}}));
            receipts.push(entry.receipt.clone());
        }
        std::fs::write(
            root.join("installed.json"),
            json!({"version":1,"installations":receipts}).to_string(),
        )
        .unwrap();
        std::fs::set_permissions(
            root.join("installed.json"),
            std::fs::Permissions::from_mode(0o600),
        )
        .unwrap();
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(directory)
            .unwrap();
        std::fs::write(
            &preferences,
            json!({"version":1,"plugins":local}).to_string(),
        )
        .unwrap();
        std::fs::set_permissions(preferences, std::fs::Permissions::from_mode(0o600)).unwrap();
    }

    pub(crate) fn register_fixture(&mut self, spec: PluginSpec) {
        let entry = fixture_entry(&spec);
        let local = json!({"enabled":spec.enabled,"review":{"owner":"example","repository":"sample","features":FEATURES}});
        if self.world.workspaces.is_empty() {
            self.update_context(
                vec![10],
                Context {
                    active_workspace: 0,
                    workspace_count: 1,
                    focused_pane: Some(1),
                    terminals: (1..=32)
                        .map(|pane| super::super::protocol::TerminalInfo {
                            pane,
                            workspace: 0,
                            cwd: Some("/fixture".into()),
                            title: Some("Fixture".into()),
                            running: true,
                        })
                        .collect(),
                },
            );
        }
        self.register_managed(entry, Some(&local));
    }
}
