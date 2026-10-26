//! inspection joins retained registration and observed admission without treating
//! a desired-on switch or a manifest declaration as a running process.

use super::*;

#[derive(Clone, Debug, PartialEq)]
pub struct Overview {
    pub key: String,
    pub name: String,
    pub description: String,
    pub origin: String,
    pub version: Option<String>,
    pub declared: String,
    pub observed: Option<super::super::manifest::Api>,
    pub enabled: bool,
    pub available: bool,
    pub state: PluginState,
    pub generation: Option<String>,
    pub running_version: Option<String>,
}

impl Overview {
    #[cfg(test)]
    pub fn fixture(key: &str, state: PluginState, enabled: bool) -> Self {
        Self {
            key: key.into(),
            name: key.into(),
            description: "Synthetic test registration".into(),
            origin: "Local test fixture".into(),
            version: None,
            declared: "Declared API unknown".into(),
            observed: None,
            enabled,
            available: true,
            state,
            generation: None,
            running_version: None,
        }
    }

    pub fn search_text(&self) -> String {
        format!(
            "{} {} {} {}",
            self.key, self.name, self.description, self.origin
        )
        .to_lowercase()
    }

    pub fn metadata(&self) -> String {
        let version = self
            .version
            .as_ref()
            .map(|version| format!(" · version {version}"))
            .unwrap_or_default();
        let observed = self
            .observed
            .as_ref()
            .map_or("Not observed in this session".into(), |api| {
                format!("Observed API {}.{}", api.major, api.minor)
            });
        format!(
            "{}\n{}{version}\n{} · {observed}{}",
            self.description,
            self.origin,
            self.declared,
            if self.available {
                ""
            } else {
                " · Files unavailable"
            }
        )
    }
}

impl PluginHost {
    pub fn overview(&self) -> Vec<Overview> {
        self.plugins
            .iter()
            .map(|plugin| {
                let mut overview = Overview {
                    key: plugin.spec.name.clone(),
                    name: plugin.spec.name.clone(),
                    description: "Managed installation unavailable".into(),
                    origin: "Managed installation".into(),
                    version: None,
                    declared: "Declared API unknown".into(),
                    observed: plugin.observed.clone(),
                    enabled: self.desired_enabled(&plugin.spec.name),
                    available: false,
                    state: plugin.state.clone(),
                    generation: None,
                    running_version: plugin.io.as_ref().and_then(|io| io.package_version.clone()),
                };
                if let Some(entry) = &plugin.managed {
                    overview.origin = format!(
                        "GitHub: {}/{} · {}",
                        entry.receipt.owner,
                        entry.receipt.repository,
                        entry.key()
                    );
                    overview.generation = Some(entry.receipt.generation.clone());
                    overview.available = entry.inspected.as_ref().is_ok_and(|inspected| {
                        inspected
                            .executable
                            .symlink_metadata()
                            .is_ok_and(|metadata| metadata.file_type().is_file())
                    });
                    match &entry.inspected {
                        Ok(inspected) => {
                            let manifest = &inspected.manifest;
                            overview.name = manifest.name.clone();
                            overview.description = manifest.description.clone();
                            overview.version = Some(manifest.version.clone());
                            overview.declared = manifest
                                .compatibility(managed::MAJOR, managed::MINOR, managed::FEATURES)
                                .map_or_else(
                                    |problem| format!("Declared API unavailable: {problem}"),
                                    |()| {
                                        format!(
                                            "Declared API {}.{} supported",
                                            manifest.api.major, manifest.api.minor
                                        )
                                    },
                                );
                        }
                        Err(problem) => {
                            overview.description = "Static metadata unavailable".into();
                            overview.declared = problem.to_string();
                        }
                    }
                }
                overview
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::os::unix::fs::DirBuilderExt;

    #[test]
    fn inventory_keeps_same_name_origins_missing_and_invalid_entries_without_execution() {
        let root = std::env::temp_dir()
            .join("opencode")
            .join(format!("among-overview-{}", glib::uuid_string_random()));
        let mut host = PluginHost::new();
        host.register_fixture(PluginSpec {
            name: "github:12/missing".into(),
            command: vec![root.join("absent").display().to_string()],
            enabled: false,
        });
        for repository in [12, 13, 14] {
            let directory = root.join(repository.to_string());
            let mut receipt = inventory::tests::fixture(&directory);
            if repository != 12 {
                let previous = receipt.package_path(&directory);
                receipt.repository_id = repository;
                let selected = receipt.package_path(&directory);
                std::fs::DirBuilder::new()
                    .recursive(true)
                    .mode(0o700)
                    .create(selected.parent().unwrap())
                    .unwrap();
                std::fs::rename(previous, &selected).unwrap();
                std::fs::write(
                    directory.join("installed.json"),
                    json!({"version":1,"installations":[receipt]}).to_string(),
                )
                .unwrap();
            }
            if repository == 14 {
                std::fs::write(
                    receipt.package_path(&directory).join("among-plugin.json"),
                    b"invalid",
                )
                .unwrap();
            }
            host.register_managed(inventory::read(&directory).unwrap().remove(0), None);
        }
        let overview = host.overview();
        assert_eq!(overview.len(), 4);
        assert!(overview
            .iter()
            .all(|entry| entry.observed.is_none() && !entry.enabled));
        assert_eq!(host.supervised_count(), 0);
        assert!(overview
            .iter()
            .any(|entry| entry.key == "github:12/missing" && !entry.available));
        assert!(overview.iter().any(|entry| entry.key == "github:12/sample"
            && entry.name == "Sample"
            && entry.available));
        assert!(overview.iter().any(|entry| entry.key == "github:13/sample"
            && entry.name == "Sample"
            && entry.available));
        assert!(overview
            .iter()
            .any(|entry| entry.key == "github:14/sample" && !entry.available));
        for repository in [12, 13, 14] {
            assert!(!root.join(repository.to_string()).join("executed").exists());
        }
        host.enable("github:14/sample");
        let invalid = host
            .overview()
            .into_iter()
            .find(|entry| entry.key == "github:14/sample")
            .unwrap();
        assert!(invalid.enabled && matches!(invalid.state, PluginState::Unavailable { .. }));
        host.disable("github:14/sample");
        assert!(!host.desired_enabled("github:14/sample"));
        assert_eq!(host.supervised_count(), 0);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn observed_api_requires_a_real_handshake_and_survives_a_runtime_fault_separately_from_desired_state(
    ) {
        let binary = std::env::current_exe()
            .unwrap()
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("examples/fake-plugin");
        let mut host = PluginHost::new();
        host.register_fixture(PluginSpec {
            name: "github:12/observed".into(),
            command: vec![binary.display().to_string(), "echo".into()],
            enabled: true,
        });
        assert!(host.overview()[0].observed.is_none());
        let deadline = Instant::now() + Duration::from_secs(3);
        while !host.overview()[0].state.is_healthy() {
            host.poll();
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(2));
        }
        let observed = host.overview()[0].observed.clone();
        assert_eq!(
            observed,
            Some(super::super::super::manifest::Api { major: 1, minor: 2 })
        );
        host.fault(0, "controlled test fault");
        assert!(host.overview()[0].enabled);
        assert_eq!(host.overview()[0].observed, observed);
        host.disable("github:12/observed");
        while !host.is_stopped() {
            host.poll();
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(2));
        }
        assert!(!host.overview()[0].enabled);
        assert_eq!(host.overview()[0].observed, observed);
    }
}
