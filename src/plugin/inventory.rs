//! only an authoritative committed record selects managed code. package files,
//! retained settings and a manifest by themselves never imply installation.

use super::data::{self, Problem};
use super::manifest::{self, Inspected, ManagedIdentity};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

pub const MAX_INSTALLATIONS: usize = 128;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Receipt {
    pub repository_id: u64,
    pub owner: String,
    pub repository: String,
    pub plugin_id: String,
    pub revision: String,
    pub manifest_sha256: String,
    pub package_sha256: String,
    pub release_id: u64,
    pub asset_id: u64,
    pub generation: String,
}

impl Receipt {
    pub fn identity(&self) -> Result<ManagedIdentity, Problem> {
        ManagedIdentity::new(self.repository_id, self.plugin_id.clone())
    }

    fn validate(&self) -> Result<(), Problem> {
        self.identity()?;
        let github_component = |text: &str| {
            !text.is_empty()
                && text.len() <= 100
                && text
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
                && !matches!(text, "." | "..")
        };
        if !github_component(&self.owner)
            || !github_component(&self.repository)
            || !hex(&self.revision, 40)
            || !hex(&self.manifest_sha256, 64)
            || !hex(&self.package_sha256, 64)
            || !hex(&self.generation, 64)
            || self.release_id == 0
            || self.asset_id == 0
        {
            return Err(Problem("invalid committed installation receipt"));
        }
        Ok(())
    }

    pub fn package_path(&self, root: &Path) -> PathBuf {
        root.join("generations")
            .join(self.repository_id.to_string())
            .join(&self.plugin_id)
            .join(&self.generation)
    }
}

fn hex(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Selection {
    version: u32,
    installations: Vec<Receipt>,
}

#[derive(Clone, Debug)]
pub struct Entry {
    pub receipt: Receipt,
    pub inspected: Result<Inspected, Problem>,
    store: PathBuf,
}

impl Entry {
    pub fn key(&self) -> String {
        format!(
            "github:{}/{}",
            self.receipt.repository_id, self.receipt.plugin_id
        )
    }

    pub fn executable_files(&self) -> Result<(std::fs::File, std::fs::File), Problem> {
        use std::os::unix::fs::MetadataExt;
        let inspected = self.inspected.as_ref().map_err(|problem| *problem)?;
        let root = data::owned_directory(&self.store)?;
        let directory = data::contained_directory(
            &root,
            &format!(
                "generations/{}/{}/{}",
                self.receipt.repository_id, self.receipt.plugin_id, self.receipt.generation
            ),
        )?;
        let bytes = data::read_bounded(
            &data::contained_file(&directory, "among-plugin.json")?,
            manifest::MAX_MANIFEST_BYTES,
        )?;
        if checksum(&bytes)? != self.receipt.manifest_sha256 {
            return Err(Problem("selected manifest changed before execution"));
        }
        let executable = data::contained_file(&directory, &inspected.manifest.entrypoint)?;
        if executable
            .metadata()
            .map_err(|_| Problem("cannot inspect entrypoint"))?
            .mode()
            & 0o111
            == 0
        {
            return Err(Problem("plugin entrypoint is not executable"));
        }
        Ok((directory, executable))
    }
}

pub fn root() -> Option<PathBuf> {
    std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|path| !path.as_os_str().is_empty())
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/share")))
        .map(|path| path.join("among/managed"))
}

pub fn checksum(bytes: &[u8]) -> Result<String, Problem> {
    let mut checksum =
        glib::Checksum::new(glib::ChecksumType::Sha256).ok_or(Problem("SHA-256 is unavailable"))?;
    checksum.update(bytes);
    checksum.string().ok_or(Problem("SHA-256 is unavailable"))
}

pub fn read(root: &Path) -> Result<Vec<Entry>, Problem> {
    if matches!(std::fs::symlink_metadata(root), Err(error) if error.kind() == std::io::ErrorKind::NotFound)
    {
        return Ok(Vec::new());
    }
    let directory = data::owned_directory(root)?;
    if matches!(std::fs::symlink_metadata(root.join("installed.json")), Err(error) if error.kind() == std::io::ErrorKind::NotFound)
    {
        return Ok(Vec::new());
    }
    let bytes = data::read_bounded(
        &data::contained_file(&directory, "installed.json")?,
        data::MAX_STATIC_BYTES,
    )?;
    let selection: Selection =
        serde_json::from_value(data::parse_unique(&bytes, data::MAX_STATIC_BYTES)?)
            .map_err(|_| Problem("invalid managed selection document"))?;
    if selection.version != 1 || selection.installations.len() > MAX_INSTALLATIONS {
        return Err(Problem(
            "unsupported selection version or installation count",
        ));
    }
    let mut identities = BTreeSet::new();
    let mut entries = Vec::new();
    for receipt in selection.installations {
        receipt.validate()?;
        let identity = receipt.identity()?;
        if !identities.insert(identity.key()) {
            return Err(Problem("duplicate committed plugin identity"));
        }
        let relative = format!(
            "generations/{}/{}/{}",
            receipt.repository_id, receipt.plugin_id, receipt.generation
        );
        let inspected = (|| {
            let package = data::contained_directory(&directory, &relative)?;
            let manifest = data::read_bounded(
                &data::contained_file(&package, "among-plugin.json")?,
                manifest::MAX_MANIFEST_BYTES,
            )?;
            if checksum(&manifest)? != receipt.manifest_sha256 {
                return Err(Problem("installed manifest does not match its receipt"));
            }
            manifest::inspect(&package, &receipt.package_path(root), identity)
        })();
        entries.push(Entry {
            receipt,
            inspected,
            store: root.into(),
        });
    }
    Ok(entries)
}

pub fn local<'a>(document: &'a Value, key: &str) -> Option<&'a Value> {
    document.get("plugins")?.get(key)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use serde_json::json;
    use std::os::unix::fs::PermissionsExt;

    pub fn fixture(root: &Path) -> Receipt {
        let manifest = json!({"schema_version":1,"id":"sample","version":"1","name":"Sample",
            "description":"No probe","entrypoint":"plugin","api":{"major":1,"minor":2},
            "required_features":["status"],"settings":{"version":1,"apply":"live","fields":[
                {"key":"label","label":"Label","kind":"string","default":"default"}]}})
        .to_string();
        let receipt = Receipt {
            repository_id: 12,
            owner: "example".into(),
            repository: "sample".into(),
            plugin_id: "sample".into(),
            revision: "a".repeat(40),
            manifest_sha256: checksum(manifest.as_bytes()).unwrap(),
            package_sha256: "b".repeat(64),
            release_id: 1,
            asset_id: 2,
            generation: "c".repeat(64),
        };
        let package = receipt.package_path(root);
        std::fs::create_dir_all(&package).unwrap();
        // test environments may use a group-writable umask; package ownership is
        // checked rather than weakening the production loader for test convenience.
        let mut path = package.as_path();
        while path.starts_with(root) {
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
            if path == root {
                break;
            }
            path = path.parent().unwrap();
        }
        std::fs::write(package.join("among-plugin.json"), manifest).unwrap();
        std::fs::set_permissions(
            package.join("among-plugin.json"),
            std::fs::Permissions::from_mode(0o600),
        )
        .unwrap();
        std::fs::write(
            package.join("plugin"),
            format!("#!/bin/sh\ntouch '{}'\n", root.join("executed").display()),
        )
        .unwrap();
        std::fs::set_permissions(
            package.join("plugin"),
            std::fs::Permissions::from_mode(0o700),
        )
        .unwrap();
        std::fs::write(
            root.join("installed.json"),
            json!({"version":1,"installations":[receipt]}).to_string(),
        )
        .unwrap();
        std::fs::set_permissions(
            root.join("installed.json"),
            std::fs::Permissions::from_mode(0o600),
        )
        .unwrap();
        receipt
    }

    #[test]
    fn committed_records_select_inert_metadata_and_keep_invalid_packages_visible() {
        let root = std::env::temp_dir().join(format!("among-inventory-{}", std::process::id()));
        let receipt = fixture(&root);
        let before = std::fs::read(root.join("installed.json")).unwrap();
        let entries = read(&root).unwrap();
        assert_eq!(entries[0].key(), "github:12/sample");
        assert_eq!(
            entries[0].inspected.as_ref().unwrap().manifest.name,
            "Sample"
        );
        assert!(!root.join("executed").exists());
        assert_eq!(before, std::fs::read(root.join("installed.json")).unwrap());
        std::fs::write(
            receipt.package_path(&root).join("among-plugin.json"),
            b"changed",
        )
        .unwrap();
        assert!(read(&root).unwrap()[0].inspected.is_err());
        std::fs::write(
            root.join("installed.json"),
            json!({"version":1,"installations":[receipt,receipt]}).to_string(),
        )
        .unwrap();
        assert!(read(&root).is_err());
        std::fs::remove_dir_all(root).unwrap();
    }
}
