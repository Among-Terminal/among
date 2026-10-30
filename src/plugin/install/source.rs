//! only explicit public GitHub origins are inspected. request errors never carry
//! raw URLs, response bodies, tokens or inherited authentication into diagnostics.

use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

use reqwest::{Client, Url};
use serde_json::Value;

use super::metadata::{Catalogue, Platform};
use super::{official, package, store};
use crate::plugin::data::{self, Problem};
use crate::plugin::inventory::release::ReleaseAsset;
use crate::plugin::inventory::{Receipt, SourceReceipt};
use crate::plugin::origin::Origin;

pub(super) mod release;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const READ_TIMEOUT: Duration = Duration::from_secs(10);
const OPERATION_TIMEOUT: Duration = Duration::from_secs(60);
const MAX_REDIRECTS: usize = 3;
static WORKERS: AtomicUsize = AtomicUsize::new(0);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Repository {
    pub owner: String,
    pub name: String,
}

impl Repository {
    pub fn parse(input: &str) -> Result<Self, Problem> {
        let input = input.trim();
        let path = if input.starts_with("https://") {
            let url = Url::parse(input).map_err(|_| {
                Problem("use owner/repository or a public HTTPS GitHub repository URL")
            })?;
            if !plain_https(&url) || url.host_str() != Some("github.com") {
                return Err(Problem(
                    "only public HTTPS github.com repository origins are supported",
                ));
            }
            url.path().trim_matches('/').to_string()
        } else {
            input.to_string()
        };
        let mut components = path.split('/');
        let owner = components
            .next()
            .ok_or(Problem("repository owner is missing"))?;
        let name = components
            .next()
            .ok_or(Problem("repository name is missing"))?;
        if !component(owner) || !component(name) || components.next().is_some() {
            return Err(Problem(
                "use an owner and repository name, without refs, query parameters or credentials",
            ));
        }
        Ok(Self {
            owner: owner.into(),
            name: name.into(),
        })
    }
}

#[derive(Clone, Debug)]
pub struct Inspection {
    pub repository_id: u64,
    pub publisher_id: u64,
    pub retrieved_at: u64,
    pub repository: Repository,
    pub catalogue_revision: String,
    pub catalogue: Catalogue,
    pub(super) catalogue_bytes: Vec<u8>,
    pub(super) release: release::ResolvedRelease,
    compatibility: std::collections::BTreeMap<(String, String), Result<usize, Problem>>,
}

impl Inspection {
    pub fn validate_source(&self) -> Result<(), Problem> {
        official::validate(&self.origin())
    }

    pub fn origin(&self) -> Origin {
        Origin {
            repository_id: self.repository_id,
            publisher_id: self.publisher_id,
            owner: self.repository.owner.clone(),
            repository: self.repository.name.clone(),
        }
    }

    /// Whether this inspection observed the newest development prerelease
    /// rather than the latest stable release. The channel lives in the
    /// observed snapshot itself, so a cached inspection keeps it as well.
    // `expect` rather than `allow`: once the Preferences development action
    // uses this, the unfulfilled expectation fails the build and is removed.
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "the development-mode Preferences action reads it")
    )]
    pub fn is_prerelease(&self) -> bool {
        self.release.snapshot.is_prerelease()
    }

    pub(super) fn from_release(
        origin: Origin,
        release: release::ResolvedRelease,
        bytes: Vec<u8>,
    ) -> Result<Self, Problem> {
        official::validate(&origin)?;
        release.validate()?;
        let catalogue_asset = release.snapshot.asset("among-plugins.json")?;
        if bytes.len() > super::metadata::MAX_CATALOGUE_BYTES
            || catalogue_asset.size != bytes.len() as u64
            || crate::plugin::inventory::checksum(&bytes)? != catalogue_asset.sha256
        {
            return Err(Problem(
                "catalogue bytes disagree with the observed release asset",
            ));
        }
        let catalogue = Catalogue::parse(&bytes)?;
        let platform = Platform::current();
        let mut compatibility = std::collections::BTreeMap::new();
        for plugin in &catalogue.plugins {
            for version in &plugin.versions {
                if version.revision != release.binding.commit {
                    return Err(Problem(
                        "catalogue version revision disagrees with the resolved release-tag commit",
                    ));
                }
                for artifact in &version.artifacts {
                    let observed = release.snapshot.asset(&artifact.asset)?;
                    if observed.size != artifact.size || observed.sha256 != artifact.sha256 {
                        return Err(Problem("catalogue package name, size or digest disagrees with its release asset"));
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
                            .ok_or(Problem("selected asset is unavailable"))
                    });
                compatibility.insert(
                    (plugin.id.clone(), version.manifest.version.clone()),
                    selected,
                );
            }
        }
        Ok(Self {
            repository_id: origin.repository_id,
            publisher_id: origin.publisher_id,
            retrieved_at: super::registry::now().unwrap_or(0),
            repository: Repository {
                owner: origin.owner,
                name: origin.repository,
            },
            catalogue_revision: release.binding.commit.clone(),
            catalogue,
            catalogue_bytes: bytes,
            release,
            compatibility,
        })
    }
    pub fn summary(&self, check: impl Fn() -> Result<(), Problem>) -> Result<String, Problem> {
        let mut lines = vec![format!("{}/{} (repository {}, publisher {}) at {}", self.repository.owner, self.repository.name,
            self.repository_id, self.publisher_id, self.catalogue_revision), "Metadata snapshot only; inspection does not install or enable plugins. Hashes establish content consistency, not publisher authentication.".into()];
        for plugin in &self.catalogue.plugins {
            for version in &plugin.versions {
                check()?;
                let selected = self
                    .artifact(&plugin.id, &version.manifest.version)
                    .and_then(|index| {
                        version
                            .artifacts
                            .get(index)
                            .ok_or(Problem("selected asset is unavailable"))
                    });
                let state = match selected {
                    Ok(artifact) => format!(
                        "ready-built asset {}, {} bytes, {} files",
                        artifact.asset,
                        artifact.size,
                        artifact.members.len()
                    ),
                    Err(problem) => problem.to_string(),
                };
                lines.push(format!(
                    "{} — {} {}: {}\n{}\ncatalogue-declared version commit {}; release {}; manifest {}. Exact release-tag binding is required during installation.",
                    plugin.id,
                    version.manifest.name,
                    version.manifest.version,
                    state,
                    version.manifest.description,
                    version.revision,
                    self.release.snapshot.id,
                    version.manifest_sha256
                ));
                if lines.iter().map(String::len).sum::<usize>() > 24 * 1024 {
                    lines.push(
                        "Further versions are omitted from this bounded inspection preview.".into(),
                    );
                    return Ok(lines.join("\n\n"));
                }
            }
        }
        Ok(lines.join("\n\n"))
    }

    pub fn select(&self, plugin: &str, version: &str) -> Result<Selection, Problem> {
        self.validate_source()?;
        let artifact_index = self.artifact(plugin, version)?;
        let version = self
            .catalogue
            .plugins
            .iter()
            .find(|entry| entry.id == plugin)
            .and_then(|entry| {
                entry
                    .versions
                    .iter()
                    .find(|entry| entry.manifest.version == version)
            })
            .ok_or(Problem("the selected catalogue version is unavailable"))?
            .clone();
        let artifact = version
            .artifacts
            .get(artifact_index)
            .ok_or(Problem("selected asset is unavailable"))?
            .clone();
        Ok(Selection {
            repository_id: self.repository_id,
            repository: self.repository.clone(),
            version,
            artifact,
            release: self.release.clone(),
            previous: None,
            source: SourceReceipt {
                publisher_id: self.publisher_id,
                catalogue_revision: self.catalogue_revision.clone(),
                catalogue_sha256: crate::plugin::inventory::checksum(&self.catalogue_bytes)?,
                release_snapshot: Box::new(self.release.snapshot.clone()),
                release_binding: self.release.binding.clone(),
            },
        })
    }

    fn artifact(&self, plugin: &str, version: &str) -> Result<usize, Problem> {
        *self
            .compatibility
            .get(&(plugin.into(), version.into()))
            .ok_or(Problem("selected catalogue version is unavailable"))?
    }
}

#[derive(Clone, Debug)]
pub struct Selection {
    repository_id: u64,
    repository: Repository,
    version: super::metadata::Version,
    artifact: super::metadata::Artifact,
    release: release::ResolvedRelease,
    source: SourceReceipt,
    previous: Option<Receipt>,
}

impl Selection {
    fn origin(&self) -> Origin {
        Origin {
            repository_id: self.repository_id,
            publisher_id: self.source.publisher_id,
            owner: self.repository.owner.clone(),
            repository: self.repository.name.clone(),
        }
    }

    pub fn for_update(
        mut self,
        entry: &crate::plugin::inventory::Entry,
        stored: Option<&Value>,
    ) -> Result<Self, Problem> {
        if entry.receipt.release().map(|release| release.repository_id) != Some(self.repository_id)
            || entry.receipt.plugin_id != self.version.manifest.id
        {
            return Err(Problem(
                "selected update belongs to a different authoritative identity",
            ));
        }
        // An update keeps the installation's switch and grants. A prerelease
        // may replace an install only after its own confirmation and without
        // them, so it can never travel this path.
        if self.is_prerelease() {
            return Err(Problem(
                "a prerelease replaces an installed plugin only through a confirmed replacement, never as an update",
            ));
        }
        if let Some(schema) = &self.version.manifest.settings {
            schema.effective(stored)?;
        }
        self.previous = Some(entry.receipt.clone());
        Ok(self)
    }

    pub fn is_update(&self) -> bool {
        self.previous.is_some()
    }

    /// Whether the selected package comes from a development prerelease. The
    /// pre-commit re-observation repeats this same channel.
    pub fn is_prerelease(&self) -> bool {
        self.release.snapshot.is_prerelease()
    }

    pub fn identity(&self) -> String {
        format!("github:{}/{}", self.repository_id, self.version.manifest.id)
    }

    pub fn review_description(&self) -> String {
        let change = self.previous.as_ref().map(|receipt| {
            receipt.origin_change(
                &Origin {
                    repository_id: self.repository_id,
                    publisher_id: self.source.publisher_id,
                    owner: self.repository.owner.clone(),
                    repository: self.repository.name.clone(),
                },
                &self.version.manifest.id,
            )
        });
        format!("{}\nSelected version {} · catalogue-declared version commit {}\n{}\nRequired access: {}\nOptional access: {}\nThe exact release tag must resolve to this commit and remain consistent through the pre-commit recheck. This is content consistency, not build reproducibility or independent publisher authentication. Only this plugin may restart. Publisher/access changes require renewed runtime review before new code starts. Local settings are retained; unsupported migrations are refused. The previous verified generation remains available. Independent filesystem/network effects cannot be rolled back.",self.description(),self.version.manifest.version,self.version.revision,change.map_or("New installation",|change|change.description()),self.version.manifest.required_features.join(", "),self.version.manifest.optional_features.join(", "))
    }
    pub fn description(&self) -> String {
        let architecture = match self.artifact.architecture {
            super::metadata::Architecture::Amd64 => "amd64",
            super::metadata::Architecture::Arm64 => "arm64",
        };
        format!(
            "github:{}/{} · version {} · {}/{} · asset {} · {} bytes",
            self.repository_id,
            self.version.manifest.id,
            self.version.manifest.version,
            self.artifact.os,
            architecture,
            self.artifact.asset,
            self.artifact.size
        )
    }
}

pub struct Job {
    cancelled: Arc<AtomicBool>,
    result: mpsc::Receiver<Result<Inspection, Problem>>,
}

pub(super) struct Permit;

impl Permit {
    pub(super) fn acquire() -> Result<Self, Problem> {
        WORKERS.fetch_update(Ordering::AcqRel, Ordering::Acquire, |count| (count < 2).then_some(count + 1))
            .map_err(|_| Problem("two repository operations are still active; cancel or wait before starting another"))?;
        Ok(Self)
    }
}

// A repository typed by address is always inspected through its latest stable
// release; only an already registered source can be asked for a prerelease.
enum Target {
    Address(Repository),
    Registered(u64, release::Channel),
}

impl Drop for Permit {
    fn drop(&mut self) {
        WORKERS.fetch_sub(1, Ordering::AcqRel);
    }
}

impl Job {
    pub fn start(repository: Repository) -> Result<Self, Problem> {
        Self::start_with(repository, None)
    }

    fn start_with(repository: Repository, client: Option<Client>) -> Result<Self, Problem> {
        Self::start_target(Target::Address(repository), client)
    }

    pub fn refresh(repository_id: u64) -> Result<Self, Problem> {
        Self::start_target(
            Target::Registered(repository_id, release::Channel::Latest),
            None,
        )
    }

    /// Inspect a registered source through its newest prerelease instead of
    /// `releases/latest`, with the same identity, inventory, catalogue and tag
    /// checks. An ordinary instance refuses before any thread, worker slot or
    /// request exists.
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "the development-mode Preferences action starts it"
        )
    )]
    pub fn refresh_prerelease(repository_id: u64) -> Result<Self, Problem> {
        if !crate::plugin::development::enabled() {
            return Err(Problem(release::DEVELOPMENT_ONLY));
        }
        Self::start_target(
            Target::Registered(repository_id, release::Channel::Prerelease),
            None,
        )
    }

    fn start_target(target: Target, client: Option<Client>) -> Result<Self, Problem> {
        let permit = Permit::acquire()?;
        let cancelled = Arc::new(AtomicBool::new(false));
        let worker_cancelled = cancelled.clone();
        let (send, result) = mpsc::sync_channel(1);
        std::thread::Builder::new()
            .name("among-repository".into())
            .spawn(move || {
                let _permit = permit;
                let outcome = (|| {
                    let runtime = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .map_err(|_| Problem("cannot start the repository network worker"))?;
                    let outcome = runtime.block_on(async {
                        let client = match client {
                            Some(client) => client,
                            None => client_builder()
                                .build()
                                .map_err(|_| Problem("cannot initialize the HTTPS client"))?,
                        };
                        let operation = Operation {
                            client,
                            cancelled: worker_cancelled,
                            deadline: Instant::now() + OPERATION_TIMEOUT,
                        };
                        operation.inspect_target(target).await
                    });
                    // retain the worker credit through runtime destruction as well:
                    // an uninterruptible system resolver must not accumulate workers.
                    drop(runtime);
                    outcome
                })();
                let _ = send.send(outcome);
            })
            .map_err(|_| Problem("cannot start the repository network worker"))?;
        Ok(Self { cancelled, result })
    }

    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
    }

    pub fn poll(&self) -> Option<Result<Inspection, Problem>> {
        match self.result.try_recv() {
            Ok(result) => Some(result),
            Err(mpsc::TryRecvError::Empty) => None,
            Err(mpsc::TryRecvError::Disconnected) => {
                Some(Err(Problem("repository worker stopped without a result")))
            }
        }
    }
}

impl Drop for Job {
    fn drop(&mut self) {
        self.cancel();
    }
}

pub(super) const DOWNLOADING: u8 = 0;
pub(super) const VERIFYING: u8 = 1;
pub(super) const STAGING: u8 = 2;
pub(super) const COMMITTING: u8 = 3;
const FINISHED: u8 = 4;
const CANCELLED: u8 = 5;
const FAILED: u8 = 6;
const CANCELLED_FINISHED: u8 = 7;

pub struct InstallJob {
    state: Arc<AtomicU8>,
    cancelled: Arc<AtomicBool>,
    result: mpsc::Receiver<Result<store::Commit, Problem>>,
    identity: String,
    generation: String,
}

impl InstallJob {
    pub fn start(selection: Selection, root: PathBuf) -> Result<Self, Problem> {
        // A prerelease selection can still come from a cached inspection made
        // in an earlier development run; an ordinary instance must not commit it.
        if selection.is_prerelease() && !crate::plugin::development::enabled() {
            return Err(Problem(release::DEVELOPMENT_ONLY));
        }
        Self::start_with(selection, root, None)
    }

    fn start_with(
        selection: Selection,
        root: PathBuf,
        client: Option<Client>,
    ) -> Result<Self, Problem> {
        official::validate(&selection.origin())?;
        let identity = crate::plugin::manifest::ManagedIdentity::new(
            selection.repository_id,
            selection.version.manifest.id.clone(),
        )?;
        Self::spawn(
            root.clone(),
            identity,
            "among-install",
            move |state, cancelled| {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .map_err(|_| Problem("cannot start the package network worker"))?;
                let outcome = runtime.block_on(async {
                    let client = match client {
                        Some(client) => client,
                        None => client_builder()
                            .build()
                            .map_err(|_| Problem("cannot initialize the HTTPS client"))?,
                    };
                    let operation = Operation {
                        client,
                        cancelled,
                        deadline: Instant::now() + OPERATION_TIMEOUT,
                    };
                    operation.install(selection, &root, state).await
                });
                drop(runtime);
                outcome
            },
        )
    }

    /// Runs one installation worker for `identity` with the shared phase,
    /// cancellation and operation-ticket rules. `work` advances the phases
    /// from downloading to committing through `progress`.
    pub(super) fn spawn(
        root: PathBuf,
        identity: crate::plugin::manifest::ManagedIdentity,
        thread: &str,
        work: impl FnOnce(&AtomicU8, Arc<AtomicBool>) -> Result<store::Commit, Problem> + Send + 'static,
    ) -> Result<Self, Problem> {
        let permit = Permit::acquire()?;
        let ticket = super::operations::Ticket::acquire(&root, identity)?;
        let identity = ticket.identity.clone();
        let generation = ticket.generation.clone();
        let state = Arc::new(AtomicU8::new(DOWNLOADING));
        let cancelled = ticket.cancelled.clone();
        let worker_state = state.clone();
        let worker_cancelled = cancelled.clone();
        let (send, result) = mpsc::sync_channel(1);
        std::thread::Builder::new()
            .name(thread.into())
            .spawn(move || {
                let _permit = permit;
                let _ticket = ticket;
                let outcome = work(&worker_state, worker_cancelled);
                if outcome.is_ok() {
                    worker_state.store(FINISHED, Ordering::Release);
                } else if worker_state.load(Ordering::Acquire) == CANCELLED
                    || (_ticket.cancelled.load(Ordering::Acquire)
                        && worker_state.load(Ordering::Acquire) < COMMITTING)
                {
                    worker_state.store(CANCELLED_FINISHED, Ordering::Release);
                } else {
                    worker_state.store(FAILED, Ordering::Release);
                }
                let _ = send.send(outcome);
            })
            .map_err(|_| Problem("cannot start the package network worker"))?;
        Ok(Self {
            state,
            cancelled,
            result,
            identity,
            generation,
        })
    }

    // the same atomic boundary gates selection rename and cancellation. after
    // commitment starts, cancellation cannot truthfully promise a rollback.
    pub fn cancel(&self) -> bool {
        let mut state = self.state.load(Ordering::Acquire);
        loop {
            if state >= COMMITTING {
                return matches!(state, CANCELLED | CANCELLED_FINISHED);
            }
            match self
                .state
                .compare_exchange(state, CANCELLED, Ordering::AcqRel, Ordering::Acquire)
            {
                Ok(_) => {
                    self.cancelled.store(true, Ordering::Release);
                    return true;
                }
                Err(current) => state = current,
            }
        }
    }

    pub fn phase(&self) -> &'static str {
        match self.state.load(Ordering::Acquire) {
            DOWNLOADING
                if self
                    .identity
                    .strip_prefix(crate::plugin::manifest::DEVELOPMENT_NAMESPACE)
                    .is_some_and(|rest| rest.starts_with('/')) =>
            {
                "Reading selected local package file"
            }
            DOWNLOADING => "Downloading selected package",
            VERIFYING => "Verifying package digest",
            STAGING => "Validating contained package files",
            COMMITTING => "Committing installation; cancellation is too late",
            FINISHED => "Installation committed",
            FAILED => "Package installation failed",
            CANCELLED_FINISHED => "Installation cancelled before commitment",
            _ => "Cancellation requested",
        }
    }

    pub fn operation(&self) -> (&str, &str) {
        (&self.identity, &self.generation)
    }

    pub fn poll(&self) -> Option<Result<store::Commit, Problem>> {
        match self.result.try_recv() {
            Ok(result) => Some(result),
            Err(mpsc::TryRecvError::Empty) => None,
            Err(mpsc::TryRecvError::Disconnected) => {
                Some(Err(Problem("package worker stopped without a result")))
            }
        }
    }
}

impl Drop for InstallJob {
    fn drop(&mut self) {
        self.cancel();
    }
}

fn client_builder() -> reqwest::ClientBuilder {
    Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .referer(false)
        .https_only(true)
        .connect_timeout(CONNECT_TIMEOUT)
        .read_timeout(READ_TIMEOUT)
        .timeout(OPERATION_TIMEOUT)
        .user_agent("AmongTerminal-repository-inspection/1")
}

struct Operation {
    client: Client,
    cancelled: Arc<AtomicBool>,
    deadline: Instant,
}

impl Operation {
    fn progress(&self, state: &AtomicU8, previous: u8, next: u8) -> Result<(), Problem> {
        self.check()?;
        state
            .compare_exchange(previous, next, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| Problem("package installation cancelled"))?;
        Ok(())
    }

    async fn check_selection_identity(&self, selected: &Selection) -> Result<(), Problem> {
        official::validate(&selected.origin())?;
        let url = github_url(
            "api.github.com",
            &["repositories", &selected.repository_id.to_string()],
        )?;
        let requested = url.clone();
        let (bytes, _) = self
            .get(url, |next| next == &requested, data::MAX_STATIC_BYTES)
            .await?;
        let document = data::parse_unique(&bytes, data::MAX_STATIC_BYTES)?;
        if document["id"].as_u64() != Some(selected.repository_id)
            || document["owner"]["id"].as_u64() != Some(selected.source.publisher_id)
            || document["private"].as_bool() != Some(false)
        {
            return Err(Problem(
                "repository or publisher identity changed; refresh and review again",
            ));
        }
        Ok(())
    }

    async fn install(
        &self,
        selected: Selection,
        root: &std::path::Path,
        state: &AtomicU8,
    ) -> Result<store::Commit, Problem> {
        self.check_selection_identity(&selected).await?;
        let version = &selected.version;
        let artifact = &selected.artifact;
        artifact.compatibility(&Platform::current()?)?;
        let mut receipt = Receipt {
            origin: crate::plugin::inventory::Provenance::Release(
                crate::plugin::inventory::ReleaseOrigin {
                    repository_id: selected.repository_id,
                    owner: selected.repository.owner.clone(),
                    repository: selected.repository.name.clone(),
                    release_id: selected.release.snapshot.id,
                    asset_id: selected.release.snapshot.asset(&artifact.asset)?.id,
                    source: Some(selected.source.clone()),
                },
            ),
            plugin_id: version.manifest.id.clone(),
            revision: version.revision.clone(),
            manifest_sha256: version.manifest_sha256.clone(),
            package_sha256: artifact.sha256.clone(),
            generation: store::token(),
            installation: selected.previous.as_ref().map_or_else(
                || Some(store::token()),
                |previous| previous.installation.clone(),
            ),
        };
        let anticipated = artifact
            .members
            .iter()
            .map(|member| member.size)
            .sum::<u64>()
            .checked_add(artifact.size)
            .ok_or(Problem("package staging size overflow"))?;
        let mut transaction = store::Transaction::new(root, anticipated, || self.check())?;
        if let Some(previous) = &selected.previous {
            transaction.expect_current(previous)?;
        } else {
            transaction.refuse_existing(&receipt)?;
        }
        let release = self.resolve_release(&selected).await?;
        if release != selected.release {
            return Err(Problem(
                "release tag or snapshot changed since review; refresh and review again",
            ));
        }
        receipt
            .release_mut()
            .and_then(|origin| origin.source.as_mut())
            .ok_or(Problem("selected package has no source receipt"))?
            .release_binding = release.binding.clone();
        self.download(&selected, &mut transaction.archive, state)
            .await?;
        transaction
            .archive
            .sync_all()
            .map_err(|_| Problem("cannot flush downloaded package"))?;
        self.progress(state, VERIFYING, STAGING)?;
        let payload_path = transaction.payload_path().to_path_buf();
        package::extract(
            &mut transaction.archive,
            &transaction.payload,
            &payload_path,
            receipt.identity()?,
            version,
            artifact,
            || self.check(),
        )?;
        if self.resolve_release(&selected).await? != release {
            return Err(Problem("release snapshot, tag or immutable peel chain changed during installation; selection was not committed"));
        }
        self.check_selection_identity(&selected).await?;
        match &selected.previous {
            Some(previous) => transaction.commit_update(
                receipt,
                previous,
                || self.check(),
                || self.progress(state, STAGING, COMMITTING),
            ),
            None => transaction.commit(
                receipt,
                || self.check(),
                || self.progress(state, STAGING, COMMITTING),
            ),
        }
    }

    async fn download(
        &self,
        selected: &Selection,
        output: &mut std::fs::File,
        state: &AtomicU8,
    ) -> Result<(), Problem> {
        let asset = selected.release.snapshot.asset(&selected.artifact.asset)?;
        self.download_asset(selected.repository_id, asset, output)
            .await?;
        self.progress(state, DOWNLOADING, VERIFYING)
    }

    async fn download_asset(
        &self,
        repository_id: u64,
        observed: &ReleaseAsset,
        output: &mut impl Write,
    ) -> Result<(), Problem> {
        let repository = repository_id.to_string();
        let asset = observed.id.to_string();
        let mut url = github_url(
            "api.github.com",
            &["repositories", &repository, "releases", "assets", &asset],
        )?;
        let requested = url.clone();
        for redirects in 0..=MAX_REDIRECTS {
            self.check()?;
            let mut response = self
                .wait(
                    self.client
                        .get(url.clone())
                        .header("Accept", "application/octet-stream")
                        .header("Accept-Encoding", "identity")
                        .header("Cache-Control", "no-cache")
                        .send(),
                )
                .await?;
            if response.status().is_redirection() {
                if redirects == MAX_REDIRECTS {
                    return Err(Problem("package redirect budget exceeded"));
                }
                let location = response
                    .headers()
                    .get(reqwest::header::LOCATION)
                    .and_then(|value| value.to_str().ok())
                    .filter(|value| value.len() <= 4096)
                    .ok_or(Problem("invalid package redirect"))?;
                let next = url
                    .join(location)
                    .map_err(|_| Problem("invalid package redirect"))?;
                if next != requested && !asset_delivery_url(&next, repository_id) {
                    return Err(Problem(
                        "package redirect changed the approved repository or delivery origin",
                    ));
                }
                url = next;
                continue;
            }
            if matches!(response.status().as_u16(), 403 | 429) {
                return Err(Problem(
                    "GitHub access or rate limit refused this request; retry later",
                ));
            }
            if !response.status().is_success() {
                return Err(Problem("selected package download was unsuccessful"));
            }
            if response
                .headers()
                .get(reqwest::header::CONTENT_ENCODING)
                .is_some_and(|value| value != "identity")
                || response
                    .content_length()
                    .is_some_and(|size| size != observed.size)
            {
                return Err(Problem(
                    "package transfer encoding or size disagrees with selected metadata",
                ));
            }
            let mut digest = glib::Checksum::new(glib::ChecksumType::Sha256)
                .ok_or(Problem("SHA-256 is unavailable"))?;
            let mut size = 0u64;
            while let Some(chunk) = self.wait(response.chunk()).await? {
                size = size
                    .checked_add(chunk.len() as u64)
                    .filter(|size| *size <= observed.size)
                    .ok_or(Problem("package transfer exceeds its declared byte budget"))?;
                digest.update(&chunk);
                output
                    .write_all(&chunk)
                    .map_err(|_| Problem("cannot write downloaded package"))?;
            }
            self.check()?;
            if size != observed.size || digest.string().as_deref() != Some(observed.sha256.as_str())
            {
                return Err(Problem(
                    "downloaded package size or digest disagrees with selected metadata",
                ));
            }
            return Ok(());
        }
        Err(Problem("package redirect budget exceeded"))
    }

    fn check(&self) -> Result<(), Problem> {
        if self.cancelled.load(Ordering::Acquire) {
            return Err(Problem("repository inspection cancelled"));
        }
        if Instant::now() >= self.deadline {
            return Err(Problem(
                "repository operation exceeded its 60-second deadline",
            ));
        }
        Ok(())
    }

    async fn wait<T>(
        &self,
        future: impl std::future::Future<Output = Result<T, reqwest::Error>>,
    ) -> Result<T, Problem> {
        tokio::pin!(future);
        loop {
            self.check()?;
            tokio::select! {
                result = &mut future => return result.map_err(|error| if error.is_timeout() { Problem("repository request timed out") }
                    else if error.is_connect() { Problem("HTTPS connection or certificate verification failed") }
                    else { Problem("repository response was interrupted or invalid") }),
                _ = tokio::time::sleep(Duration::from_millis(20)) => {}
            }
        }
    }

    async fn get(
        &self,
        initial: Url,
        redirect: impl Fn(&Url) -> bool,
        limit: usize,
    ) -> Result<(Vec<u8>, Url), Problem> {
        let mut url = initial;
        for redirects in 0..=MAX_REDIRECTS {
            self.check()?;
            let request = self
                .client
                .get(url.clone())
                .header("Accept", "application/vnd.github+json")
                .header("Cache-Control", "no-cache")
                .header("Accept-Encoding", "identity");
            let mut response = self.wait(request.send()).await?;
            if response.status().is_redirection() {
                if redirects == MAX_REDIRECTS {
                    return Err(Problem("repository redirect budget exceeded"));
                }
                let location = response
                    .headers()
                    .get(reqwest::header::LOCATION)
                    .and_then(|value| value.to_str().ok())
                    .filter(|value| value.len() <= 4096)
                    .ok_or(Problem("invalid repository redirect"))?;
                let next = url
                    .join(location)
                    .map_err(|_| Problem("invalid repository redirect"))?;
                if !plain_https(&next) || !redirect(&next) {
                    return Err(Problem(
                        "repository redirect changed the approved origin or resource",
                    ));
                }
                url = next;
                continue;
            }
            if matches!(response.status().as_u16(), 403 | 429) {
                return Err(Problem(
                    "GitHub access or rate limit refused this request; retry later",
                ));
            }
            if response.status().as_u16() == 404 {
                return Err(Problem(
                    "public repository or its among-plugins.json metadata was not found",
                ));
            }
            if !response.status().is_success() {
                return Err(Problem(
                    "repository service returned an unsuccessful response",
                ));
            }
            if response
                .headers()
                .get(reqwest::header::CONTENT_ENCODING)
                .is_some_and(|value| value != "identity")
            {
                return Err(Problem(
                    "repository returned unsupported HTTP content encoding",
                ));
            }
            if response
                .content_length()
                .is_some_and(|length| length > limit as u64)
            {
                return Err(Problem("repository response exceeds its byte budget"));
            }
            let mut bytes = Vec::new();
            while let Some(chunk) = self.wait(response.chunk()).await? {
                if bytes
                    .len()
                    .checked_add(chunk.len())
                    .is_none_or(|length| length > limit)
                {
                    return Err(Problem("repository response exceeds its byte budget"));
                }
                bytes.extend_from_slice(&chunk);
            }
            self.check()?;
            return Ok((bytes, url));
        }
        Err(Problem("repository redirect budget exceeded"))
    }

    #[cfg(test)]
    async fn inspect(&self, input: Repository) -> Result<Inspection, Problem> {
        self.inspect_target(Target::Address(input)).await
    }

    async fn inspect_target(&self, target: Target) -> Result<Inspection, Problem> {
        let (expected_id, channel) = match &target {
            Target::Registered(id, channel) if *id > 0 => (Some(*id), *channel),
            Target::Registered(..) => {
                return Err(Problem("invalid registered repository identity"))
            }
            Target::Address(_) => (None, release::Channel::Latest),
        };
        let repository_url = match target {
            Target::Address(input) => {
                github_url("api.github.com", &["repos", &input.owner, &input.name])?
            }
            Target::Registered(id, _) => {
                github_url("api.github.com", &["repositories", &id.to_string()])?
            }
        };
        let requested = repository_url.clone();
        let (bytes, resolved) = self
            .get(
                repository_url,
                |next| next == &requested || numeric_repository_url(next, expected_id, ""),
                data::MAX_STATIC_BYTES,
            )
            .await?;
        let document = data::parse_unique(&bytes, data::MAX_STATIC_BYTES)?;
        if document["private"].as_bool() != Some(false) {
            return Err(Problem("only public repositories are supported"));
        }
        let id = document["id"].as_u64().filter(|id| *id > 0).ok_or(Problem(
            "repository response lacks an authoritative identity",
        ))?;
        if expected_id.is_some_and(|expected| expected != id) {
            return Err(Problem(
                "registered numeric repository identity changed; refusing to retarget source",
            ));
        }
        if resolved != requested && !numeric_repository_url(&resolved, Some(id), "") {
            return Err(Problem(
                "repository redirect and response identities disagree",
            ));
        }
        let owner = document["owner"]["login"]
            .as_str()
            .filter(|owner| component(owner))
            .ok_or(Problem("invalid canonical repository owner"))?;
        let publisher_id = document["owner"]["id"]
            .as_u64()
            .filter(|id| *id > 0)
            .ok_or(Problem(
                "repository response lacks an authoritative publisher identity",
            ))?;
        let name = document["name"]
            .as_str()
            .filter(|name| component(name))
            .ok_or(Problem("invalid canonical repository name"))?;
        let repository = Repository {
            owner: owner.into(),
            name: name.into(),
        };
        let origin = Origin {
            repository_id: id,
            publisher_id,
            owner: repository.owner,
            repository: repository.name,
        };
        official::validate(&origin)?;
        let snapshot = self.observe(id, channel).await?;
        let catalogue_asset = snapshot.asset("among-plugins.json")?;
        if catalogue_asset.size == 0
            || catalogue_asset.size > super::metadata::MAX_CATALOGUE_BYTES as u64
        {
            return Err(Problem("release catalogue exceeds its byte budget"));
        }
        let mut bytes = Vec::new();
        self.download_asset(id, catalogue_asset, &mut bytes).await?;
        // Check the transfer digest before a catalogue parser receives these bytes.
        let release = self.resolve_tag(id, snapshot).await?;
        let inspection = Inspection::from_release(origin, release, bytes)?;
        self.check()?;
        Ok(inspection)
    }
}

fn github_url(host: &str, components: &[&str]) -> Result<Url, Problem> {
    let mut url = Url::parse(&format!("https://{host}/"))
        .map_err(|_| Problem("invalid GitHub request origin"))?;
    url.path_segments_mut()
        .map_err(|_| Problem("invalid GitHub request path"))?
        .clear()
        .extend(components);
    Ok(url)
}

fn plain_https(url: &Url) -> bool {
    url.scheme() == "https"
        && url.port_or_known_default() == Some(443)
        && url.username().is_empty()
        && url.password().is_none()
        && url.query().is_none()
        && url.fragment().is_none()
}

fn asset_delivery_url(url: &Url, repository: u64) -> bool {
    if url.scheme() != "https"
        || url.port_or_known_default() != Some(443)
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
        || !matches!(
            url.host_str(),
            Some("release-assets.githubusercontent.com" | "objects.githubusercontent.com")
        )
    {
        return false;
    }
    let parts = url.path().split('/').collect::<Vec<_>>();
    parts.len() == 4
        && parts[0].is_empty()
        && matches!(
            parts[1],
            "github-production-release-asset" | "github-production-release-asset-2e65be"
        )
        && parts[2] == repository.to_string()
        && parts[3].len() == 36
        && parts[3].bytes().enumerate().all(|(index, byte)| {
            if [8, 13, 18, 23].contains(&index) {
                byte == b'-'
            } else {
                byte.is_ascii_hexdigit()
            }
        })
        && url.as_str().len() <= 4096
}

fn numeric_repository_url(url: &Url, expected: Option<u64>, suffix: &str) -> bool {
    if !plain_https(url) || url.host_str() != Some("api.github.com") {
        return false;
    }
    let Some(path) = url
        .path()
        .strip_prefix("/repositories/")
        .and_then(|path| path.strip_suffix(suffix))
    else {
        return false;
    };
    let Ok(id) = path.parse::<u64>() else {
        return false;
    };
    id > 0 && id.to_string() == path && expected.is_none_or(|expected| expected == id)
}

fn component(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 100
        && !matches!(value, "." | "..")
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

#[cfg(test)]
pub(crate) mod tests;
