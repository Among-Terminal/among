//! Synthetic schema2 release observation shared by in-crate tests. Nothing is
//! executed, downloaded or installed: the files only let a verifier reach the
//! check under test instead of stopping early on absent inputs.

use super::{PUBLISHER, REPOSITORY};
use crate::{storage, Result};
use serde_json::{json, Value};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

pub(crate) struct Fixture {
    pub(crate) root: PathBuf,
    pub(crate) catalogue: Value,
    pub(crate) inputs: Value,
}

fn sha(bytes: &[u8]) -> Result<String> {
    storage::sha256(bytes)
}

fn member(path: &str, bytes: &[u8], executable: bool) -> Result<Value> {
    Ok(json!({"path":path,"size":bytes.len(),"sha256":sha(bytes)?,"executable":executable}))
}

impl Fixture {
    pub(crate) fn new() -> Result<Self> {
        let root = storage::private_directory(&std::env::temp_dir(), "among-release-observation")?;
        let revision = "a".repeat(40);
        let architecture = crate::platform::discover()?.architecture;
        let source_bytes = b"synthetic source delivery";
        let source_name = format!("source-{revision}.tar.gz");
        let source = json!({"repository_id":REPOSITORY,"revision":revision,"source_archive":source_name,"source_sha256":sha(source_bytes)?});
        let source_record = serde_json::to_vec(&source)?;
        fs::write(root.join("source.tar.gz"), source_bytes)?;
        let mut assets = vec![
            json!({"id":500,"name":source_name,"size":source_bytes.len(),"sha256":sha(source_bytes)?}),
        ];
        let mut plugins = Vec::new();
        let mut prepared = Vec::new();
        for (index, id) in ["clock", "pane-pwd", "system-status"].iter().enumerate() {
            let manifest = json!({"schema_version":1,"id":id,"version":"0.2.0","entrypoint":"bin/plugin","api":{"major":1,"minor":2},"required_features":["status"],"optional_features":[]});
            let manifest_bytes = serde_json::to_vec(&manifest)?;
            let binary = b"synthetic non-executable payload";
            let package = format!("synthetic archive {id}").into_bytes();
            let mut artifacts = Vec::new();
            for (offset, arch) in ["amd64", "arm64"].iter().enumerate() {
                let name = format!("{id}-0.2.0-linux-{arch}.tar.gz");
                let asset_id = 100 + index as u64 * 2 + offset as u64;
                assets.push(
                    json!({"id":asset_id,"name":name,"size":package.len(),"sha256":sha(&package)?}),
                );
                artifacts.push(json!({"os":"linux","architecture":arch,"runtime":{"kind":"native","libc":"glibc","minimum":"2.34"},"dependencies":[],"format":"tar.gz","asset":name,"size":package.len(),"sha256":sha(&package)?,"members":[member("among-plugin.json", &manifest_bytes, false)?,member("SOURCE.json", &source_record, false)?,member("bin/plugin", binary, true)?]}));
                if *arch == architecture {
                    let directory = root.join(id);
                    fs::create_dir_all(directory.join("bin"))?;
                    fs::write(directory.join("among-plugin.json"), &manifest_bytes)?;
                    fs::write(directory.join("SOURCE.json"), &source_record)?;
                    fs::write(directory.join("bin/plugin"), binary)?;
                    fs::set_permissions(
                        directory.join("bin/plugin"),
                        fs::Permissions::from_mode(0o700),
                    )?;
                    fs::write(root.join(format!("{id}.tar.gz")), &package)?;
                    prepared.push(json!({"id":id,"version":"0.2.0","revision":revision,"release_id":900,"asset":name,"asset_id":asset_id,"sha256":sha(&package)?,"manifest_sha256":sha(&manifest_bytes)?,"entrypoint":"bin/plugin","architecture":architecture,"binary":directory.join("bin/plugin"),"binary_sha256":sha(binary)?,"package":root.join(format!("{id}.tar.gz"))}));
                }
            }
            plugins.push(json!({"id":id,"versions":[{"revision":revision,"manifest":manifest,"manifest_sha256":sha(&manifest_bytes)?,"artifacts":artifacts}]}));
        }
        let catalogue = json!({"schema_version":2,"plugins":plugins});
        let bytes = serde_json::to_vec(&catalogue)?;
        fs::write(root.join("among-plugins.json"), &bytes)?;
        assets.push(
            json!({"id":600,"name":"among-plugins.json","size":bytes.len(),"sha256":sha(&bytes)?}),
        );
        assets.sort_by(|left, right| left["name"].as_str().cmp(&right["name"].as_str()));
        let inputs = json!({"schema_version":2,"kind":"published_release_inputs_not_installed","repository_id":REPOSITORY,"publisher_id":PUBLISHER,"catalogue_revision":revision,"source_revision":revision,"catalogue_size":bytes.len(),"catalogue_sha256":sha(&bytes)?,"architecture":architecture,"release_id":900,"release_snapshot":{"id":900,"tag":"v0.2.0","assets":assets},"release_binding":{"tag":"v0.2.0","object":revision,"commit":revision},"tag_chain":[["commit",revision]],"plugins":prepared,"source_archive":source_name,"source_asset_id":500,"source_size":source_bytes.len(),"source_sha256":sha(source_bytes)?});
        let fixture = Self {
            root,
            catalogue,
            inputs,
        };
        fixture.save()?;
        Ok(fixture)
    }

    pub(crate) fn save(&self) -> Result<()> {
        let bytes = serde_json::to_vec(&self.inputs)?;
        fs::write(self.root.join("inputs.json"), &bytes)?;
        fs::write(self.root.join("release-inputs.json"), bytes)?;
        Ok(())
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}
