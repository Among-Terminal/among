//! managed packages for host/GTK tests. fixture commands cannot enter the
//! production host: tests publish receipts/manifests and grant access explicitly.

use super::*;
use serde_json::json;
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};

impl PluginHost {
    pub(crate) fn register_fixture(&mut self, spec: PluginSpec) {
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
        let features = [
            "status",
            "context.terminals",
            "context.cwd",
            "context.title",
            "context.process",
            "events.terminal",
        ];
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
        let entry = inventory::read(&root).unwrap().remove(0);
        let local = json!({"enabled":spec.enabled,"review":{"owner":"example","repository":"sample","features":features}});
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
