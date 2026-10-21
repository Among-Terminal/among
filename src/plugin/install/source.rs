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
use super::{package, store};
use crate::plugin::data::{self, Problem};
use crate::plugin::inventory::Receipt;

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
    pub repository: Repository,
    pub catalogue_revision: String,
    pub catalogue: Catalogue,
}

impl Inspection {
    pub fn summary(&self, check: impl Fn() -> Result<(), Problem>) -> Result<String, Problem> {
        let platform = Platform::current();
        let mut lines = vec![format!("{}/{} (repository {}) at {}", self.repository.owner, self.repository.name,
            self.repository_id, self.catalogue_revision), "Metadata snapshot only; inspection does not install or enable plugins. Hashes establish content consistency, not publisher authentication.".into()];
        for plugin in &self.catalogue.plugins {
            for version in &plugin.versions {
                check()?;
                let selected = platform
                    .as_ref()
                    .map_err(|problem| *problem)
                    .and_then(|platform| version.select(platform));
                let state = match selected {
                    Ok(artifact) => format!(
                        "ready-built asset {}, {} bytes, {} files",
                        artifact.asset_id,
                        artifact.size,
                        artifact.members.len()
                    ),
                    Err(problem) => problem.to_string(),
                };
                lines.push(format!(
                    "{} — {} {}: {}\n{}\nrevision {}; release {}; manifest {}",
                    plugin.id,
                    version.manifest.name,
                    version.manifest.version,
                    state,
                    version.manifest.description,
                    version.revision,
                    version.release_id,
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
        let artifact = version.select(&Platform::current()?)?.clone();
        Ok(Selection {
            repository_id: self.repository_id,
            repository: self.repository.clone(),
            version,
            artifact,
        })
    }
}

#[derive(Clone, Debug)]
pub struct Selection {
    repository_id: u64,
    repository: Repository,
    version: super::metadata::Version,
    artifact: super::metadata::Artifact,
}

impl Selection {
    pub fn description(&self) -> String {
        format!(
            "github:{}/{} · asset {} · {} bytes",
            self.repository_id,
            self.version.manifest.id,
            self.artifact.asset_id,
            self.artifact.size
        )
    }
}

pub struct Job {
    cancelled: Arc<AtomicBool>,
    result: mpsc::Receiver<Result<Inspection, Problem>>,
}

struct Permit;

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
        WORKERS.fetch_update(Ordering::AcqRel, Ordering::Acquire, |count| (count < 2).then_some(count + 1))
            .map_err(|_| Problem("two repository operations are still active; cancel or wait before starting another"))?;
        let permit = Permit;
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
                        operation.inspect(repository).await
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

const DOWNLOADING: u8 = 0;
const VERIFYING: u8 = 1;
const STAGING: u8 = 2;
const COMMITTING: u8 = 3;
const FINISHED: u8 = 4;
const CANCELLED: u8 = 5;

pub struct InstallJob {
    state: Arc<AtomicU8>,
    cancelled: Arc<AtomicBool>,
    result: mpsc::Receiver<Result<store::Commit, Problem>>,
}

impl InstallJob {
    pub fn start(selection: Selection, root: PathBuf) -> Result<Self, Problem> {
        Self::start_with(selection, root, None)
    }

    fn start_with(
        selection: Selection,
        root: PathBuf,
        client: Option<Client>,
    ) -> Result<Self, Problem> {
        WORKERS.fetch_update(Ordering::AcqRel, Ordering::Acquire, |count| (count < 2).then_some(count + 1))
            .map_err(|_| Problem("two repository operations are still active; cancel or wait before starting another"))?;
        let permit = Permit;
        let state = Arc::new(AtomicU8::new(DOWNLOADING));
        let cancelled = Arc::new(AtomicBool::new(false));
        let worker_state = state.clone();
        let worker_cancelled = cancelled.clone();
        let (send, result) = mpsc::sync_channel(1);
        std::thread::Builder::new()
            .name("among-install".into())
            .spawn(move || {
                let _permit = permit;
                let outcome = (|| {
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
                            cancelled: worker_cancelled,
                            deadline: Instant::now() + OPERATION_TIMEOUT,
                        };
                        operation.install(selection, &root, &worker_state).await
                    });
                    drop(runtime);
                    outcome
                })();
                if outcome.is_ok() {
                    worker_state.store(FINISHED, Ordering::Release);
                }
                let _ = send.send(outcome);
            })
            .map_err(|_| Problem("cannot start the package network worker"))?;
        Ok(Self {
            state,
            cancelled,
            result,
        })
    }

    // the same atomic boundary gates selection rename and cancellation. after
    // commitment starts, cancellation cannot truthfully promise a rollback.
    pub fn cancel(&self) -> bool {
        let mut state = self.state.load(Ordering::Acquire);
        loop {
            if state >= COMMITTING {
                return state == CANCELLED;
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
            DOWNLOADING => "Downloading selected package",
            VERIFYING => "Verifying package digest",
            STAGING => "Validating contained package files",
            COMMITTING => "Committing installation; cancellation is too late",
            FINISHED => "Installation committed",
            _ => "Cancellation requested",
        }
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

    async fn install(
        &self,
        selected: Selection,
        root: &std::path::Path,
        state: &AtomicU8,
    ) -> Result<store::Commit, Problem> {
        let version = &selected.version;
        let artifact = &selected.artifact;
        artifact.compatibility(&Platform::current()?)?;
        let receipt = Receipt {
            repository_id: selected.repository_id,
            owner: selected.repository.owner.clone(),
            repository: selected.repository.name.clone(),
            plugin_id: version.manifest.id.clone(),
            revision: version.revision.clone(),
            manifest_sha256: version.manifest_sha256.clone(),
            package_sha256: artifact.sha256.clone(),
            release_id: version.release_id,
            asset_id: artifact.asset_id,
            generation: store::token(),
            installation: Some(store::token()),
        };
        let anticipated = artifact
            .members
            .iter()
            .map(|member| member.size)
            .sum::<u64>()
            .checked_add(artifact.size)
            .ok_or(Problem("package staging size overflow"))?;
        let mut transaction = store::Transaction::new(root, anticipated, || self.check())?;
        transaction.refuse_existing(&receipt)?;
        self.download(&selected, &mut transaction.archive).await?;
        self.progress(state, DOWNLOADING, VERIFYING)?;
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
        transaction.commit(
            receipt,
            || self.check(),
            || self.progress(state, STAGING, COMMITTING),
        )
    }

    async fn download(
        &self,
        selected: &Selection,
        output: &mut std::fs::File,
    ) -> Result<(), Problem> {
        let repository = selected.repository_id.to_string();
        let release = selected.version.release_id.to_string();
        let asset = selected.artifact.asset_id.to_string();
        let url = github_url(
            "api.github.com",
            &["repositories", &repository, "releases", &release],
        )?;
        let requested = url.clone();
        let (bytes, _) = self
            .get(url, |next| next == &requested, data::MAX_STATIC_BYTES)
            .await?;
        let release = data::parse_unique(&bytes, data::MAX_STATIC_BYTES)?;
        if release["id"].as_u64() != Some(selected.version.release_id)
            || release["draft"].as_bool() != Some(false)
        {
            return Err(Problem(
                "selected public release identity disagrees with the catalogue",
            ));
        }
        let assets = release["assets"]
            .as_array()
            .ok_or(Problem("selected release has no asset inventory"))?;
        let mut matches = assets
            .iter()
            .filter(|entry| entry["id"].as_u64() == Some(selected.artifact.asset_id));
        let entry = matches.next().ok_or(Problem(
            "selected asset does not belong to the selected release",
        ))?;
        if matches.next().is_some()
            || entry["size"].as_u64() != Some(selected.artifact.size)
            || entry["state"].as_str() != Some("uploaded")
        {
            return Err(Problem("selected release asset identity or size changed"));
        }
        if entry.get("digest").is_some_and(|digest| {
            !digest.is_null()
                && digest.as_str() != Some(format!("sha256:{}", selected.artifact.sha256).as_str())
        }) {
            return Err(Problem(
                "release asset digest disagrees with selected metadata",
            ));
        }
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
                if next != requested && !asset_delivery_url(&next, selected.repository_id) {
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
                    .is_some_and(|size| size != selected.artifact.size)
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
                    .filter(|size| *size <= selected.artifact.size)
                    .ok_or(Problem("package transfer exceeds its declared byte budget"))?;
                digest.update(&chunk);
                output
                    .write_all(&chunk)
                    .map_err(|_| Problem("cannot write downloaded package"))?;
            }
            self.check()?;
            if size != selected.artifact.size
                || digest.string().as_deref() != Some(selected.artifact.sha256.as_str())
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

    async fn inspect(&self, input: Repository) -> Result<Inspection, Problem> {
        let repository_url = github_url("api.github.com", &["repos", &input.owner, &input.name])?;
        let requested = repository_url.clone();
        let (bytes, resolved) = self
            .get(
                repository_url,
                |next| next == &requested || numeric_repository_url(next, None, ""),
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
        if resolved != requested && !numeric_repository_url(&resolved, Some(id), "") {
            return Err(Problem(
                "repository redirect and response identities disagree",
            ));
        }
        let owner = document["owner"]["login"]
            .as_str()
            .filter(|owner| component(owner))
            .ok_or(Problem("invalid canonical repository owner"))?;
        let name = document["name"]
            .as_str()
            .filter(|name| component(name))
            .ok_or(Problem("invalid canonical repository name"))?;
        let branch = document["default_branch"]
            .as_str()
            .filter(|branch| !branch.is_empty() && data::bounded_text(branch, 256, false))
            .ok_or(Problem("repository has no usable default branch"))?;
        let repository = Repository {
            owner: owner.into(),
            name: name.into(),
        };
        let commit_url = github_url("api.github.com", &["repos", owner, name, "commits", branch])?;
        let requested = commit_url.clone();
        let suffix = format!(
            "/commits/{}",
            requested
                .path()
                .rsplit('/')
                .next()
                .ok_or(Problem("invalid commit reference"))?
        );
        let (bytes, _) = self
            .get(
                commit_url,
                |next| next == &requested || numeric_repository_url(next, Some(id), &suffix),
                data::MAX_STATIC_BYTES,
            )
            .await?;
        let commit: Value = data::parse_unique(&bytes, data::MAX_STATIC_BYTES)?;
        let revision = commit["sha"]
            .as_str()
            .filter(|sha| {
                sha.len() == 40
                    && sha
                        .bytes()
                        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            })
            .ok_or(Problem("commit response lacks an exact revision"))?;
        let catalogue_url = github_url(
            "raw.githubusercontent.com",
            &[owner, name, revision, "among-plugins.json"],
        )?;
        let requested = catalogue_url.clone();
        let (bytes, _) = self
            .get(
                catalogue_url,
                |next| next == &requested,
                super::metadata::MAX_CATALOGUE_BYTES,
            )
            .await?;
        let catalogue = Catalogue::parse(&bytes)?;
        Ok(Inspection {
            repository_id: id,
            repository,
            catalogue_revision: revision.into(),
            catalogue,
        })
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
