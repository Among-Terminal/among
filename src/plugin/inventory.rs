//! only an authoritative committed record selects managed code. package files,
//! retained settings and a manifest by themselves never imply installation.

use super::data::{self, Problem};
use super::manifest::{self, Inspected, ManagedIdentity};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

pub const MAX_INSTALLATIONS: usize = 128;

pub(crate) mod release;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ReleaseBinding {
    pub tag: String,
    pub object: String,
    pub commit: String,
}

impl ReleaseBinding {
    pub(crate) fn supported_tag(tag: &str) -> bool {
        !tag.is_empty()
            && tag.len() <= 256
            && tag.split('/').count() <= 16
            && !tag.contains("..")
            && tag.split('/').all(|part| {
                !part.is_empty()
                    && !part.starts_with('.')
                    && !part.ends_with('.')
                    && !part.ends_with(".lock")
                    && part.bytes().all(|byte| {
                        byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-')
                    })
            })
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SourceReceipt {
    pub publisher_id: u64,
    pub catalogue_revision: String,
    pub catalogue_sha256: String,
    pub release_snapshot: Box<release::ReleaseSnapshot>,
    pub release_binding: ReleaseBinding,
}

/// A generation published by a GitHub release of a public repository.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReleaseOrigin {
    pub repository_id: u64,
    pub owner: String,
    pub repository: String,
    pub release_id: u64,
    pub asset_id: u64,
    pub source: Option<SourceReceipt>,
}

/// A generation installed in development mode from a local schema2
/// catalogue. Only what the author's files state is recorded: there is no
/// repository, publisher, release, immutability or tag binding to record,
/// and none is invented.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LocalOrigin {
    pub catalogue_sha256: String,
    pub asset: String,
}

/// How a local development install is named wherever a repository would be.
pub const LOCAL_SOURCE_LABEL: &str = "Local development catalogue";

/// What a local development install lacks compared with a release install;
/// shown before installation and in the plugin details.
pub const LOCAL_MISSING_PROVENANCE: &str = "No repository or publisher identity, no release, no immutability and no tag-to-commit binding: only the local catalogue's digests are checked.";

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Provenance {
    Release(ReleaseOrigin),
    Local(LocalOrigin),
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(try_from = "StoredReceipt", into = "StoredReceipt")]
pub struct Receipt {
    pub origin: Provenance,
    pub plugin_id: String,
    pub revision: String,
    pub manifest_sha256: String,
    pub package_sha256: String,
    pub generation: String,
    // fresh installs cannot inherit an earlier installation's persisted switch.
    // absent tokens retain startup compatibility for pre-installer receipts.
    pub installation: Option<String>,
}

// The on-disk form keeps release receipts exactly as they were written before
// local development installs existed; a local receipt replaces every release
// field with one `development` object.
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct StoredReceipt {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    development: Option<LocalOrigin>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    repository_id: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    owner: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    repository: Option<String>,
    plugin_id: String,
    revision: String,
    manifest_sha256: String,
    package_sha256: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    release_id: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    asset_id: Option<u64>,
    generation: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    installation: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    source: Option<SourceReceipt>,
}

impl TryFrom<StoredReceipt> for Receipt {
    type Error = &'static str;

    fn try_from(stored: StoredReceipt) -> Result<Self, Self::Error> {
        let origin = match (
            stored.development,
            stored.repository_id,
            stored.owner,
            stored.repository,
            stored.release_id,
            stored.asset_id,
            stored.source,
        ) {
            (Some(local), None, None, None, None, None, None) => Provenance::Local(local),
            (
                None,
                Some(repository_id),
                Some(owner),
                Some(repository),
                Some(release_id),
                Some(asset_id),
                source,
            ) => Provenance::Release(ReleaseOrigin {
                repository_id,
                owner,
                repository,
                release_id,
                asset_id,
                source,
            }),
            _ => return Err("a receipt is either a complete release record or a local one"),
        };
        Ok(Self {
            origin,
            plugin_id: stored.plugin_id,
            revision: stored.revision,
            manifest_sha256: stored.manifest_sha256,
            package_sha256: stored.package_sha256,
            generation: stored.generation,
            installation: stored.installation,
        })
    }
}

impl From<Receipt> for StoredReceipt {
    fn from(receipt: Receipt) -> Self {
        let mut stored = Self {
            development: None,
            repository_id: None,
            owner: None,
            repository: None,
            plugin_id: receipt.plugin_id,
            revision: receipt.revision,
            manifest_sha256: receipt.manifest_sha256,
            package_sha256: receipt.package_sha256,
            release_id: None,
            asset_id: None,
            generation: receipt.generation,
            installation: receipt.installation,
            source: None,
        };
        match receipt.origin {
            Provenance::Local(local) => stored.development = Some(local),
            Provenance::Release(release) => {
                stored.repository_id = Some(release.repository_id);
                stored.owner = Some(release.owner);
                stored.repository = Some(release.repository);
                stored.release_id = Some(release.release_id);
                stored.asset_id = Some(release.asset_id);
                stored.source = release.source;
            }
        }
        stored
    }
}

impl SourceReceipt {
    fn validate(&self, release: &ReleaseOrigin, receipt: &Receipt) -> Result<(), Problem> {
        self.release_snapshot.validate()?;
        let binding = &self.release_binding;
        let catalogue = self.release_snapshot.asset("among-plugins.json")?;
        let package = self.release_snapshot.asset_by_id(release.asset_id)?;
        if self.publisher_id == 0
            || self.catalogue_revision != receipt.revision
            || self.release_snapshot.id != release.release_id
            || self.release_snapshot.tag != binding.tag
            || !ReleaseBinding::supported_tag(&binding.tag)
            || !hex(&binding.object, 40)
            || binding.commit != receipt.revision
            || catalogue.sha256 != self.catalogue_sha256
            || catalogue.size == 0
            || package.name == "among-plugins.json"
            || package.size == 0
            || package.sha256 != receipt.package_sha256
        {
            return Err(Problem(
                "source receipt disagrees with its release snapshot",
            ));
        }
        Ok(())
    }
}

impl Receipt {
    pub fn identity(&self) -> Result<ManagedIdentity, Problem> {
        match &self.origin {
            Provenance::Release(release) => {
                ManagedIdentity::new(release.repository_id, self.plugin_id.clone())
            }
            Provenance::Local(_) => ManagedIdentity::development(self.plugin_id.clone()),
        }
    }

    pub fn release(&self) -> Option<&ReleaseOrigin> {
        match &self.origin {
            Provenance::Release(release) => Some(release),
            Provenance::Local(_) => None,
        }
    }

    pub fn release_mut(&mut self) -> Option<&mut ReleaseOrigin> {
        match &mut self.origin {
            Provenance::Release(release) => Some(release),
            Provenance::Local(_) => None,
        }
    }

    pub fn local(&self) -> Option<&LocalOrigin> {
        match &self.origin {
            Provenance::Local(local) => Some(local),
            Provenance::Release(_) => None,
        }
    }

    /// Whether this generation may start only in a primary started with
    /// `--plugin-development`. A local catalogue has no publication behind it
    /// at all, so its generation never runs in an ordinary instance.
    pub fn requires_development(&self) -> bool {
        matches!(self.origin, Provenance::Local(_))
    }

    /// The relative directory of this generation below the store root.
    pub fn generation_directory(&self) -> String {
        format!(
            "generations/{}/{}/{}",
            self.namespace_component(),
            self.plugin_id,
            self.generation
        )
    }

    pub(super) fn validate(&self) -> Result<(), Problem> {
        self.identity()?;
        if !hex(&self.revision, 40)
            || !hex(&self.manifest_sha256, 64)
            || !hex(&self.package_sha256, 64)
            || !hex(&self.generation, 64)
            || self
                .installation
                .as_ref()
                .is_some_and(|token| !hex(token, 64))
        {
            return Err(Problem("invalid committed installation receipt"));
        }
        match &self.origin {
            Provenance::Release(release) => {
                if !super::origin::component(&release.owner)
                    || !super::origin::component(&release.repository)
                    || release.release_id == 0
                    || release.asset_id == 0
                {
                    return Err(Problem("invalid committed installation receipt"));
                }
                if let Some(source) = &release.source {
                    source.validate(release, self)?;
                }
            }
            // A local install always has its own consent token: there is no
            // earlier receipt format it could be compatible with.
            Provenance::Local(local) => {
                if self.installation.is_none()
                    || !hex(&local.catalogue_sha256, 64)
                    || !release::asset_name(&local.asset)
                    || local.asset == "among-plugins.json"
                {
                    return Err(Problem("invalid committed development receipt"));
                }
            }
        }
        Ok(())
    }

    pub fn origin_change(
        &self,
        candidate: &super::origin::Origin,
        plugin_id: &str,
    ) -> super::origin::Change {
        use super::origin::{Change, Origin};
        let Some(release) = self.release() else {
            return Change::IdentityChanged;
        };
        if release.repository_id != candidate.repository_id || self.plugin_id != plugin_id {
            return Change::IdentityChanged;
        }
        match &release.source {
            Some(source) => candidate.compare(&Origin {
                repository_id: release.repository_id,
                publisher_id: source.publisher_id,
                owner: release.owner.clone(),
                repository: release.repository.clone(),
            }),
            None => Change::PublisherChanged,
        }
    }

    pub fn review(&self, features: Vec<String>) -> Value {
        match &self.origin {
            Provenance::Release(release) => {
                let mut review = serde_json::json!({"owner":release.owner,"repository":release.repository,"features":features});
                if let Some(source) = &release.source {
                    review["repository_id"] = release.repository_id.into();
                    review["publisher_id"] = source.publisher_id.into();
                }
                review
            }
            // Without a publisher the only thing an approval can be bound to is
            // this installation's consent token, so a replacement from another
            // catalogue never inherits it.
            Provenance::Local(_) => serde_json::json!({
                "development": self.plugin_id,
                "installation": self.installation,
                "features": features
            }),
        }
    }

    pub fn matches_review(&self, review: &Value) -> bool {
        match &self.origin {
            Provenance::Release(release) => match &release.source {
                Some(source) => {
                    review["repository_id"].as_u64() == Some(release.repository_id)
                        && review["publisher_id"].as_u64() == Some(source.publisher_id)
                }
                None => {
                    review["owner"].as_str() == Some(&release.owner)
                        && review["repository"].as_str() == Some(&release.repository)
                }
            },
            Provenance::Local(_) => {
                review["development"].as_str() == Some(&self.plugin_id)
                    && self.installation.is_some()
                    && review["installation"].as_str() == self.installation.as_deref()
            }
        }
    }

    pub fn package_path(&self, root: &Path) -> PathBuf {
        root.join(self.generation_directory())
    }

    /// What the manager shows as the source: the GitHub repository, or the
    /// fact that a local catalogue has no repository at all.
    pub fn source_label(&self) -> String {
        match &self.origin {
            Provenance::Release(release) => {
                format!("GitHub: {}/{}", release.owner, release.repository)
            }
            Provenance::Local(_) => LOCAL_SOURCE_LABEL.into(),
        }
    }

    /// The settings and switch key of this identity, the same string as
    /// `ManagedIdentity::key` for a valid receipt.
    pub fn key(&self) -> String {
        match &self.origin {
            Provenance::Release(release) => {
                format!("github:{}/{}", release.repository_id, self.plugin_id)
            }
            Provenance::Local(_) => {
                format!("{}/{}", manifest::DEVELOPMENT_NAMESPACE, self.plugin_id)
            }
        }
    }

    /// The directory below `generations/` that holds this plugin's
    /// generations: the decimal repository ID or the development namespace.
    pub fn namespace_component(&self) -> String {
        match &self.origin {
            Provenance::Release(release) => release.repository_id.to_string(),
            Provenance::Local(_) => manifest::DEVELOPMENT_NAMESPACE.into(),
        }
    }
}

fn hex(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Selection {
    pub version: u32,
    pub installations: Vec<Receipt>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub previous: Vec<Receipt>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision: Option<String>,
}

impl Selection {
    pub fn validate(&self) -> Result<(), Problem> {
        if self.version != 1
            || self.installations.len() > MAX_INSTALLATIONS
            || self.previous.len() > MAX_INSTALLATIONS
            || self.revision.as_ref().is_some_and(|value| !hex(value, 64))
        {
            return Err(Problem(
                "unsupported selection version or installation count",
            ));
        }
        for receipts in [&self.installations, &self.previous] {
            let mut identities = BTreeSet::new();
            for receipt in receipts {
                receipt.validate()?;
                if !identities.insert(receipt.identity()?.key()) {
                    return Err(Problem("duplicate committed plugin identity"));
                }
            }
        }
        for previous in &self.previous {
            if !self.installations.iter().any(|current| {
                current.identity().ok() == previous.identity().ok()
                    && current.generation != previous.generation
            }) {
                return Err(Problem(
                    "retained generation has no distinct selected identity",
                ));
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug)]
pub struct Entry {
    pub receipt: Receipt,
    pub inspected: Result<Inspected, Problem>,
    pub previous: Option<Receipt>,
    store: PathBuf,
    // clones, open detail views and supervised processes retain this shared
    // lock. cleanup needs an exclusive lock before touching generation files.
    _lease: Option<std::sync::Arc<std::fs::File>>,
}

impl Entry {
    pub fn removal_pending(&self) -> bool {
        super::install::store::removal_pending(&self.store, &self.key())
    }
    pub fn generation_lease(&self) -> Option<std::sync::Arc<std::fs::File>> {
        self._lease.clone()
    }
    pub fn key(&self) -> String {
        self.receipt.key()
    }

    pub fn executable_files(&self) -> Result<(std::fs::File, std::fs::File), Problem> {
        use std::os::unix::fs::MetadataExt;
        let inspected = self.inspected.as_ref().map_err(|problem| *problem)?;
        let root = super::install::store::open_existing_root(&self.store)?;
        let directory = data::contained_directory(&root, &self.receipt.generation_directory())?;
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
    let directory = super::install::store::open_existing_root(root)?;
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
    selection.validate()?;
    let mut identities = BTreeSet::new();
    let mut entries = Vec::new();
    for receipt in selection.installations {
        receipt.validate()?;
        let identity = receipt.identity()?;
        if !identities.insert(identity.key()) {
            return Err(Problem("duplicate committed plugin identity"));
        }
        let relative = receipt.generation_directory();
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
        let (inspected, lease) = match super::install::store::lease(&directory, &receipt) {
            Ok(lease) => (inspected, lease),
            Err(problem) => (Err(problem), None),
        };
        entries.push(Entry {
            previous: selection
                .previous
                .iter()
                .find(|old| old.identity().ok() == receipt.identity().ok())
                .cloned(),
            receipt,
            inspected,
            store: root.into(),
            _lease: lease,
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

    /// The release part of a release-fixture receipt.
    pub(crate) fn release_of(receipt: &mut Receipt) -> &mut ReleaseOrigin {
        receipt
            .release_mut()
            .expect("fixture receipts come from a release")
    }

    pub(crate) fn source_receipt(receipt: &Receipt) -> SourceReceipt {
        let release = receipt
            .release()
            .expect("fixture receipts come from a release");
        // synthetic publication facts for inventory/origin controls, never API evidence.
        let snapshot = release::ReleaseSnapshot::from_value(&json!({
            "id":release.release_id,"tag_name":"releases/v1.0","draft":false,"prerelease":false,"immutable":true,
            "assets":[{"id":release.asset_id,"name":"sample.tar.gz","size":123,"state":"uploaded","digest":format!("sha256:{}",receipt.package_sha256)},
                {"id":release.asset_id+1,"name":"among-plugins.json","size":1,"state":"uploaded","digest":format!("sha256:{}","b".repeat(64))}]
        })).unwrap();
        SourceReceipt {
            publisher_id: 34,
            catalogue_revision: receipt.revision.clone(),
            catalogue_sha256: "b".repeat(64),
            release_snapshot: Box::new(snapshot),
            release_binding: ReleaseBinding {
                tag: "releases/v1.0".into(),
                object: "c".repeat(40),
                commit: receipt.revision.clone(),
            },
        }
    }

    pub fn fixture(root: &Path) -> Receipt {
        let manifest = json!({"schema_version":1,"id":"sample","version":"1","name":"Sample",
            "description":"No probe","entrypoint":"plugin","api":{"major":1,"minor":2},
            "required_features":["status"],"settings":{"version":1,"apply":"live","fields":[
                {"key":"label","label":"Label","kind":"string","default":"default"}]}})
        .to_string();
        let receipt = Receipt {
            origin: Provenance::Release(ReleaseOrigin {
                repository_id: 12,
                owner: "example".into(),
                repository: "sample".into(),
                release_id: 1,
                asset_id: 2,
                source: None,
            }),
            plugin_id: "sample".into(),
            revision: "a".repeat(40),
            manifest_sha256: checksum(manifest.as_bytes()).unwrap(),
            package_sha256: "b".repeat(64),
            generation: "c".repeat(64),
            installation: None,
        };
        write_fixture(root, receipt, &manifest)
    }

    /// A receipt like `fixture`, from a local development catalogue.
    pub fn development_fixture(root: &Path) -> Receipt {
        let manifest = json!({"schema_version":1,"id":"sample","version":"1","name":"Sample",
            "description":"No probe","entrypoint":"plugin","api":{"major":1,"minor":2},
            "required_features":["status"]})
        .to_string();
        let receipt = Receipt {
            origin: Provenance::Local(LocalOrigin {
                catalogue_sha256: "d".repeat(64),
                asset: "sample.tar.gz".into(),
            }),
            plugin_id: "sample".into(),
            revision: "a".repeat(40),
            manifest_sha256: checksum(manifest.as_bytes()).unwrap(),
            package_sha256: "b".repeat(64),
            generation: "e".repeat(64),
            installation: Some("f".repeat(64)),
        };
        write_fixture(root, receipt, &manifest)
    }

    fn write_fixture(root: &Path, receipt: Receipt, manifest: &str) -> Receipt {
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
    fn release_and_local_records_keep_their_exact_stored_shapes() {
        let root = std::env::temp_dir().join(format!(
            "among-receipt-shape-{}",
            glib::uuid_string_random()
        ));
        let mut release = fixture(&root);
        release_of(&mut release).source = Some(source_receipt(&release));
        // A release receipt is stored with exactly the fields it had before
        // local installs existed, so existing stores and tools still read it.
        let stored = serde_json::to_value(&release).unwrap();
        let mut keys = stored
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        keys.sort();
        assert_eq!(
            keys,
            [
                "asset_id",
                "generation",
                "manifest_sha256",
                "owner",
                "package_sha256",
                "plugin_id",
                "release_id",
                "repository",
                "repository_id",
                "revision",
                "source"
            ]
        );
        assert_eq!(
            serde_json::from_value::<Receipt>(stored.clone()).unwrap(),
            release
        );
        let local = development_fixture(&root.join("development"));
        let stored_local = serde_json::to_value(&local).unwrap();
        assert_eq!(
            serde_json::from_value::<Receipt>(stored_local.clone()).unwrap(),
            local
        );
        assert_eq!(local.key(), "development/sample");
        assert!(local
            .package_path(&root)
            .starts_with(root.join("generations/development/sample")));
        // A record is either one or the other; a half-release local record and a
        // release record without its repository are both refused.
        let mut mixed = stored_local.clone();
        mixed["repository_id"] = json!(12);
        assert!(serde_json::from_value::<Receipt>(mixed).is_err());
        let mut incomplete = stored.clone();
        incomplete.as_object_mut().unwrap().remove("repository_id");
        assert!(serde_json::from_value::<Receipt>(incomplete).is_err());
        // A local record always carries its own consent token.
        let mut tokenless = local.clone();
        tokenless.installation = None;
        assert!(tokenless.validate().is_err());
        let mut unsafe_asset = local.clone();
        unsafe_asset.origin = Provenance::Local(LocalOrigin {
            catalogue_sha256: "d".repeat(64),
            asset: "../sample.tar.gz".into(),
        });
        assert!(unsafe_asset.validate().is_err());
        // Approval of a local install is bound to its consent token, never to
        // the plugin ID alone.
        let review = local.review(vec!["status".into()]);
        assert!(local.matches_review(&review));
        let mut replaced = local.clone();
        replaced.installation = Some("9".repeat(64));
        assert!(!replaced.matches_review(&review));
        assert!(!release.matches_review(&review));
        assert!(!local.matches_review(&release.review(vec!["status".into()])));
        std::fs::remove_dir_all(root).unwrap();
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

    #[test]
    fn recorded_release_binding_is_validated_on_actual_offline_inventory_read() {
        let root = std::env::temp_dir().join(format!(
            "among-release-receipt-{}",
            glib::uuid_string_random()
        ));
        let mut receipt = fixture(&root);
        release_of(&mut receipt).source = Some(source_receipt(&receipt));
        for invalid in ["commit", "object", "tag"] {
            let mut candidate = receipt.clone();
            let binding = &mut release_of(&mut candidate)
                .source
                .as_mut()
                .unwrap()
                .release_binding;
            match invalid {
                "commit" => binding.commit = "e".repeat(40),
                "object" => binding.object = "invalid".into(),
                _ => binding.tag = "../heads/main".into(),
            }
            let bytes = json!({"version":1,"installations":[candidate]}).to_string();
            std::fs::write(root.join("installed.json"), &bytes).unwrap();
            assert!(
                read(&root).is_err(),
                "{invalid}: a persisted label cannot stand in for a consistent receipt"
            );
            assert_eq!(
                std::fs::read_to_string(root.join("installed.json")).unwrap(),
                bytes
            );
        }
        std::fs::write(
            root.join("installed.json"),
            json!({"version":1,"installations":[receipt]}).to_string(),
        )
        .unwrap();
        assert_eq!(read(&root).unwrap()[0].receipt, receipt);
        let legacy = fixture(&root);
        assert!(legacy.release().unwrap().source.is_none());
        assert_eq!(read(&root).unwrap()[0].receipt, legacy);
        for tag in [
            "",
            "../v1",
            "v1..0",
            "/v1",
            "v1/",
            "v1//x",
            ".hidden",
            "v1.",
            "v1.lock",
            "v1/x.lock",
            "v1@{1}",
            "v1^{}",
            "v1%2fx",
            "v1?token=synthetic",
            "v1 x",
            "версия",
        ] {
            assert!(!ReleaseBinding::supported_tag(tag), "{tag}");
        }
        assert!(!ReleaseBinding::supported_tag(&"x".repeat(257)));
        assert!(!ReleaseBinding::supported_tag(&vec!["x"; 17].join("/")));
        assert!(ReleaseBinding::supported_tag("releases/v1.0-rc_1"));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn complete_source_snapshots_are_required_and_revalidated_on_offline_receipt_read() {
        let root = std::env::temp_dir().join(format!(
            "among-snapshot-receipt-{}",
            glib::uuid_string_random()
        ));
        let mut receipt = fixture(&root);
        release_of(&mut receipt).source = Some(source_receipt(&receipt));
        let original = json!({"version":1,"installations":[receipt]});
        let settings = root.join("retained-settings");
        std::fs::write(&settings, b"unrelated user bytes").unwrap();
        for (pointer, value) in [
            ("/source/release_snapshot/id", json!(999)),
            ("/source/release_snapshot/tag", json!("v2.0")),
            ("/source/release_snapshot/assets/1/id", json!(999)),
            (
                "/source/release_snapshot/assets/1/name",
                json!("among-plugins.json"),
            ),
            (
                "/source/release_snapshot/assets/1/sha256",
                json!("e".repeat(64)),
            ),
            (
                "/source/release_snapshot/assets/0/sha256",
                json!("e".repeat(64)),
            ),
            ("/source/release_snapshot/assets/0/size", json!(0)),
            ("/source/release_snapshot/assets/1/size", json!(0)),
            ("/source/catalogue_revision", json!("d".repeat(40))),
            ("/source/release_binding", Value::Null),
        ] {
            let mut candidate = original.clone();
            *candidate["installations"][0].pointer_mut(pointer).unwrap() = value;
            let bytes = candidate.to_string();
            std::fs::write(root.join("installed.json"), &bytes).unwrap();
            assert!(read(&root).is_err(), "{pointer}");
            assert_eq!(
                std::fs::read_to_string(root.join("installed.json")).unwrap(),
                bytes
            );
            assert_eq!(std::fs::read(&settings).unwrap(), b"unrelated user bytes");
        }
        for case in [
            "missing-snapshot",
            "missing-binding",
            "unknown-field",
            "overflow",
            "noncanonical",
        ] {
            let mut candidate = original.clone();
            let source = &mut candidate["installations"][0]["source"];
            match case {
                "missing-snapshot" => {
                    source.as_object_mut().unwrap().remove("release_snapshot");
                }
                "missing-binding" => {
                    source.as_object_mut().unwrap().remove("release_binding");
                }
                "unknown-field" => {
                    source["release_snapshot"]["fallback_url"] =
                        json!("https://invalid.example/never")
                }
                "overflow" => {
                    let asset = source["release_snapshot"]["assets"][0].clone();
                    source["release_snapshot"]["assets"] = json!(vec![asset; 65]);
                }
                "noncanonical" => source["release_snapshot"]["assets"]
                    .as_array_mut()
                    .unwrap()
                    .reverse(),
                _ => unreachable!(),
            }
            let bytes = candidate.to_string();
            std::fs::write(root.join("installed.json"), &bytes).unwrap();
            assert!(read(&root).is_err(), "{case}");
            assert_eq!(
                std::fs::read_to_string(root.join("installed.json")).unwrap(),
                bytes
            );
            assert_eq!(std::fs::read(&settings).unwrap(), b"unrelated user bytes");
        }
        std::fs::write(root.join("installed.json"), original.to_string()).unwrap();
        assert_eq!(read(&root).unwrap()[0].receipt, receipt);
        assert!(!root.join("executed").exists());
        std::fs::remove_dir_all(root).unwrap();
    }
}
