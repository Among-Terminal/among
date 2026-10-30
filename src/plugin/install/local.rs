//! Development installation from a local schema2 catalogue.
//!
//! The author chooses an `among-plugins.json` with the package files next to
//! it. The catalogue supplies the expected manifest, members and digests,
//! and the package then passes the same archive, ELF, member, digest, grant
//! and sandbox checks as a release package. What a release would add cannot
//! be checked and is not invented: there is no repository or publisher
//! identity, no release, no immutability and no tag-to-commit binding. The
//! installation lives in the `development/<plugin-id>` namespace and runs only
//! in a primary started with `--plugin-development`.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::Arc;

use super::metadata::{Artifact, Catalogue, Platform, Version, MAX_CATALOGUE_BYTES};
use super::source::{InstallJob, COMMITTING, DOWNLOADING, STAGING, VERIFYING};
use super::{package, store};
use crate::plugin::data::{self, Problem};
use crate::plugin::inventory::{self, LocalOrigin, Provenance, Receipt};
use crate::plugin::manifest::ManagedIdentity;

const COPY_BLOCK: usize = 128 * 1024;

fn require_development() -> Result<(), Problem> {
    if !crate::plugin::development::enabled() {
        return Err(Problem(
            "local catalogue installation is available only when Among is started with --plugin-development",
        ));
    }
    Ok(())
}

/// One parsed local catalogue, fixed at the moment it was read. Installation
/// rereads it and refuses if its bytes changed since this review.
#[derive(Clone, Debug)]
pub struct LocalCatalogue {
    directory: PathBuf,
    file_name: String,
    pub catalogue: Catalogue,
    catalogue_sha256: String,
    compatibility: BTreeMap<(String, String), Result<usize, Problem>>,
}

fn split(path: &Path) -> Result<(PathBuf, String), Problem> {
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| inventory::release::asset_name(name))
        .ok_or(Problem("local catalogue path has no usable file name"))?;
    let directory = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .ok_or(Problem("local catalogue path has no directory"))?;
    Ok((directory.to_path_buf(), file_name.into()))
}

fn read_catalogue(directory: &File, file_name: &str) -> Result<(Vec<u8>, String), Problem> {
    let bytes = data::read_bounded(
        &data::contained_file(directory, file_name)?,
        MAX_CATALOGUE_BYTES,
    )?;
    let digest = inventory::checksum(&bytes)?;
    Ok((bytes, digest))
}

impl LocalCatalogue {
    pub fn open(path: &Path) -> Result<Self, Problem> {
        require_development()?;
        Self::read(path)
    }

    fn read(path: &Path) -> Result<Self, Problem> {
        let (directory_path, file_name) = split(path)?;
        let directory = data::owned_directory(&directory_path)?;
        let (bytes, catalogue_sha256) = read_catalogue(&directory, &file_name)?;
        let catalogue = Catalogue::parse(&bytes)?;
        let platform = Platform::current();
        let mut compatibility = BTreeMap::new();
        for plugin in &catalogue.plugins {
            for version in &plugin.versions {
                for artifact in &version.artifacts {
                    if !inventory::release::asset_name(&artifact.asset)
                        || artifact.asset == file_name
                    {
                        return Err(Problem(
                            "local catalogue names a package file that is unsafe or the catalogue itself",
                        ));
                    }
                }
                let selected = platform
                    .as_ref()
                    .map_err(|problem| *problem)
                    .and_then(|platform| version.select(platform))
                    .and_then(|selected| {
                        version
                            .artifacts
                            .iter()
                            .position(|artifact| artifact.asset == selected.asset)
                            .ok_or(Problem("selected package is unavailable"))
                    });
                compatibility.insert(
                    (plugin.id.clone(), version.manifest.version.clone()),
                    selected,
                );
            }
        }
        Ok(Self {
            directory: directory_path,
            file_name,
            catalogue,
            catalogue_sha256,
            compatibility,
        })
    }

    pub fn path(&self) -> PathBuf {
        self.directory.join(&self.file_name)
    }

    /// The package this platform would install for a catalogue entry, or why
    /// none can be installed.
    pub fn artifact(&self, plugin: &str, version: &str) -> Result<&Artifact, Problem> {
        let index = (*self
            .compatibility
            .get(&(plugin.into(), version.into()))
            .ok_or(Problem("selected catalogue version is unavailable"))?)?;
        self.version(plugin, version)?
            .artifacts
            .get(index)
            .ok_or(Problem("selected package is unavailable"))
    }

    fn version(&self, plugin: &str, version: &str) -> Result<&Version, Problem> {
        self.catalogue
            .plugins
            .iter()
            .find(|entry| entry.id == plugin)
            .and_then(|entry| {
                entry
                    .versions
                    .iter()
                    .find(|entry| entry.manifest.version == version)
            })
            .ok_or(Problem("selected catalogue version is unavailable"))
    }

    pub fn select(&self, plugin: &str, version: &str) -> Result<LocalSelection, Problem> {
        require_development()?;
        self.selection(plugin, version)
    }

    fn selection(&self, plugin: &str, version: &str) -> Result<LocalSelection, Problem> {
        let artifact = self.artifact(plugin, version)?.clone();
        let version = self.version(plugin, version)?.clone();
        ManagedIdentity::development(version.manifest.id.clone())?;
        Ok(LocalSelection {
            directory: self.directory.clone(),
            file_name: self.file_name.clone(),
            catalogue_sha256: self.catalogue_sha256.clone(),
            version,
            artifact,
            replaces: None,
        })
    }
}

/// A reviewed local package. Without `replacing` it installs only when no
/// development install of this ID exists; with it, it replaces exactly that
/// install with a fresh consent token.
#[derive(Clone, Debug)]
pub struct LocalSelection {
    directory: PathBuf,
    file_name: String,
    catalogue_sha256: String,
    version: Version,
    artifact: Artifact,
    replaces: Option<Receipt>,
}

impl LocalSelection {
    pub fn identity(&self) -> String {
        format!(
            "{}/{}",
            crate::plugin::manifest::DEVELOPMENT_NAMESPACE,
            self.version.manifest.id
        )
    }

    /// Confirms replacement of the current development install of this ID.
    /// The replacement inherits neither its switch nor its grants.
    pub fn replacing(mut self, current: &Receipt) -> Result<Self, Problem> {
        if !current.requires_development()
            || current.local().is_none()
            || current.plugin_id != self.version.manifest.id
        {
            return Err(Problem(
                "only a development install of the same plugin ID can be replaced",
            ));
        }
        self.replaces = Some(current.clone());
        Ok(self)
    }

    pub fn replaces(&self) -> Option<&Receipt> {
        self.replaces.as_ref()
    }

    pub fn review_description(&self) -> String {
        format!(
            "{} · version {} · package {} · {} bytes\n{}\nCatalogue {}\nCatalogue SHA-256 {}\n{}\nRequired access: {}\nOptional access: {}\n{}",
            self.identity(),
            self.version.manifest.version,
            self.artifact.asset,
            self.artifact.size,
            self.version.manifest.description,
            self.directory.join(&self.file_name).display(),
            self.catalogue_sha256,
            inventory::LOCAL_MISSING_PROVENANCE,
            self.version.manifest.required_features.join(", "),
            self.version.manifest.optional_features.join(", "),
            match &self.replaces {
                Some(_) => "Replaces the current development install of this ID. Its switch and granted access are not carried over; the replaced generation stays available as the previous one. It starts only with --plugin-development.",
                None => "New development install, disabled until you switch it on. It starts only with --plugin-development.",
            }
        )
    }
}

pub fn start(selection: LocalSelection, root: PathBuf) -> Result<InstallJob, Problem> {
    require_development()?;
    spawn(selection, root)
}

fn spawn(selection: LocalSelection, root: PathBuf) -> Result<InstallJob, Problem> {
    let identity = ManagedIdentity::development(selection.version.manifest.id.clone())?;
    let worker_root = root.clone();
    InstallJob::spawn(
        root,
        identity,
        "among-local-install",
        move |state, cancelled| install(selection, &worker_root, state, &cancelled),
    )
}

fn progress(
    state: &AtomicU8,
    check: &impl Fn() -> Result<(), Problem>,
    previous: u8,
    next: u8,
) -> Result<(), Problem> {
    check()?;
    state
        .compare_exchange(previous, next, Ordering::AcqRel, Ordering::Acquire)
        .map_err(|_| Problem("package installation cancelled"))?;
    Ok(())
}

fn install(
    selection: LocalSelection,
    root: &Path,
    state: &AtomicU8,
    cancelled: &Arc<AtomicBool>,
) -> Result<store::Commit, Problem> {
    let check = || {
        if cancelled.load(Ordering::Acquire) {
            return Err(Problem("local installation cancelled"));
        }
        Ok(())
    };
    let version = &selection.version;
    let artifact = &selection.artifact;
    artifact.compatibility(&Platform::current()?)?;
    let receipt = Receipt {
        origin: Provenance::Local(LocalOrigin {
            catalogue_sha256: selection.catalogue_sha256.clone(),
            asset: artifact.asset.clone(),
        }),
        plugin_id: version.manifest.id.clone(),
        revision: version.revision.clone(),
        manifest_sha256: version.manifest_sha256.clone(),
        package_sha256: artifact.sha256.clone(),
        generation: store::token(),
        // Always new: a local install never inherits a switch or a grant.
        installation: Some(store::token()),
    };
    let anticipated = artifact
        .members
        .iter()
        .map(|member| member.size)
        .sum::<u64>()
        .checked_add(artifact.size)
        .ok_or(Problem("package staging size overflow"))?;
    let mut transaction = store::Transaction::new(root, anticipated, check)?;
    match &selection.replaces {
        Some(current) => transaction.expect_current(current)?,
        None => transaction.refuse_existing(&receipt)?,
    }
    let directory = data::owned_directory(&selection.directory)?;
    unchanged_catalogue(&directory, &selection)?;
    copy_package(&directory, artifact, &mut transaction.archive, &check)?;
    progress(state, &check, DOWNLOADING, VERIFYING)?;
    transaction
        .archive
        .sync_all()
        .map_err(|_| Problem("cannot flush the copied local package"))?;
    progress(state, &check, VERIFYING, STAGING)?;
    let payload_path = transaction.payload_path().to_path_buf();
    package::extract(
        &mut transaction.archive,
        &transaction.payload,
        &payload_path,
        receipt.identity()?,
        version,
        artifact,
        check,
    )?;
    // The catalogue is the only authority a local package has, so a rebuild
    // during installation must not commit bytes the user did not review.
    unchanged_catalogue(&directory, &selection)?;
    let begin = || progress(state, &check, STAGING, COMMITTING);
    match &selection.replaces {
        Some(current) => transaction.commit_replacement(receipt, current, check, begin),
        None => transaction.commit(receipt, check, begin),
    }
}

fn unchanged_catalogue(directory: &File, selection: &LocalSelection) -> Result<(), Problem> {
    let (_, digest) = read_catalogue(directory, &selection.file_name)?;
    if digest != selection.catalogue_sha256 {
        return Err(Problem(
            "local catalogue changed since review; open it again and review the new contents",
        ));
    }
    Ok(())
}

/// Copies exactly the reviewed package file and checks its size and SHA-256
/// against the catalogue before any byte of it is unpacked.
fn copy_package(
    directory: &File,
    artifact: &Artifact,
    output: &mut File,
    check: &impl Fn() -> Result<(), Problem>,
) -> Result<(), Problem> {
    let mut input = data::contained_file(directory, &artifact.asset)?;
    let length = input
        .metadata()
        .map_err(|_| Problem("cannot inspect the local package file"))?
        .len();
    if length != artifact.size {
        return Err(Problem(
            "local package size disagrees with its catalogue entry",
        ));
    }
    let mut digest =
        glib::Checksum::new(glib::ChecksumType::Sha256).ok_or(Problem("SHA-256 is unavailable"))?;
    let mut buffer = vec![0; COPY_BLOCK];
    let mut copied = 0u64;
    loop {
        check()?;
        let read = input
            .read(&mut buffer)
            .map_err(|_| Problem("cannot read the local package file"))?;
        if read == 0 {
            break;
        }
        copied = copied
            .checked_add(read as u64)
            .filter(|copied| *copied <= artifact.size)
            .ok_or(Problem("local package grew beyond its catalogue size"))?;
        digest.update(&buffer[..read]);
        output
            .write_all(&buffer[..read])
            .map_err(|_| Problem("cannot write the copied local package"))?;
    }
    if copied != artifact.size || digest.string().as_deref() != Some(artifact.sha256.as_str()) {
        return Err(Problem(
            "local package size or digest disagrees with its catalogue entry",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests;
