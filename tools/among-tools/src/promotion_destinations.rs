//! Concrete publication destinations.
//!
//! Both adapters fail closed. A missing or malformed credential, an unexpected
//! endpoint override, or an unverifiable remote identity stops the promotion
//! instead of degrading it. No credential-bearing output is ever placed in an
//! error, a log or a command line.

use crate::{
    process::OwnedProcess,
    promotion::{Destinations, Group},
    sha3, storage, Result,
};
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    time::Duration,
};

/// Environment overrides that would point a public promotion at a destination
/// other than the real one. Their presence is a refusal, not a preference.
pub const FORBIDDEN_ENVIRONMENT: [&str; 3] = [
    "STORE_DASHBOARD_URL",
    "STORE_UPLOAD_URL",
    "SNAPCRAFT_STORE_AUTH",
];

/// `owner/name`, nothing that could be interpreted as an option or a path.
///
/// A repository name reaches a command line, so a value containing spaces, a
/// slash or shell punctuation would let a misconfigured job promote into a
/// different repository than the one the release record was built for.
pub fn valid_repository(value: &str) -> Result<()> {
    let parts = value.split('/').collect::<Vec<_>>();
    let valid = |part: &str| {
        let mut bytes = part.bytes();
        // A leading `-` would make the value read as an option to `gh` rather
        // than as a repository, so the first byte must be alphanumeric even
        // though `-`, `_` and `.` are legal afterwards.
        bytes
            .next()
            .is_some_and(|byte| byte.is_ascii_alphanumeric())
            && bytes.all(|byte| byte.is_ascii_alphanumeric() || b"-_.".contains(&byte))
    };
    if parts.len() != 2 || parts.iter().any(|part| !valid(part)) {
        return Err("invalid GitHub repository".into());
    }
    Ok(())
}

/// A credential must be present and not blank.
///
/// Blank is treated as absent on purpose: a masked CI secret that expands to
/// whitespace produces an authenticating call that fails far away from its
/// cause, and it is indistinguishable from a real token in a log line.
pub fn require_credential(value: Option<String>, name: &str) -> Result<String> {
    value
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| format!("protected {name} credential is unavailable").into())
}

/// Refuse when any endpoint override is in play.
pub fn reject_named_overrides(present: &[&str]) -> Result<()> {
    match present.first() {
        Some(name) => Err(format!("{name} override is not permitted for public promotion").into()),
        None => Ok(()),
    }
}

/// Whether a failure means "this does not exist" rather than "this could not be
/// checked".
///
/// The distinction decides whether a promotion gate runs at all. An API call
/// that could not reach GitHub must never read as "no release exists": that
/// would silently skip the monotonicity and immutability checks and let a
/// rollback through. Only a genuine 404 is an absence.
fn is_not_found(error: &(dyn std::error::Error + 'static)) -> bool {
    error.to_string().contains(NOT_FOUND)
}

/// The marker `gh` prints for a missing object. Only this substring is read
/// from the child's output; the rest of the output is discarded.
const NOT_FOUND: &str = "HTTP 404";

/// Run an external command with a scrubbed environment and return its stdout.
///
/// Failures are reported without the command's own output, because that output
/// can contain credential-dependent text from a failed authenticated call. The
/// one exception is a 404 marker, which carries no secret and is required to
/// tell "absent" from "unchecked".
fn quiet_output(
    program: &Path,
    args: &[String],
    workdir: &Path,
    extra: &BTreeMap<String, String>,
) -> Result<String> {
    let log = fs::File::create(workdir.join("destination.log"))?;
    let errors = fs::File::create(workdir.join("destination.stderr"))?;
    // Only PATH, locale and the credentials the caller names are visible to a
    // publication command. A destination that needs no credential must not be
    // forced to present one, so the set is supplied per call rather than
    // hardcoded to the GitHub token.
    let mut env: BTreeMap<std::ffi::OsString, std::ffi::OsString> = BTreeMap::from([
        (
            std::ffi::OsString::from("PATH"),
            std::ffi::OsString::from("/usr/bin:/bin"),
        ),
        (
            std::ffi::OsString::from("LC_ALL"),
            std::ffi::OsString::from("C.UTF-8"),
        ),
    ]);
    for (name, value) in extra {
        env.insert(name.into(), value.as_str().into());
    }
    let borrowed = args.iter().map(String::as_str).collect::<Vec<_>>();
    let mut process = OwnedProcess::spawn_split(program, &borrowed, &env, workdir, &log, &errors)?;
    if !process.wait(Duration::from_secs(120))?.success() {
        // A 404 is an expected answer, not a failure, and carries no secret.
        // Anything else keeps its text out of the error entirely.
        if fs::read_to_string(workdir.join("destination.stderr"))?.contains(NOT_FOUND) {
            return Err(NOT_FOUND.into());
        }
        return Err("protected destination operation failed; see the protected job log".into());
    }
    Ok(fs::read_to_string(workdir.join("destination.log"))?)
}

/// The environment a GitHub call runs with.
fn github_environment() -> Result<BTreeMap<String, String>> {
    Ok(BTreeMap::from([(
        "GH_TOKEN".to_string(),
        credential("GH_TOKEN")?,
    )]))
}

/// The environment a Store call runs with.
///
/// The exported credential is passed unchanged in `SNAPCRAFT_STORE_CREDENTIALS`,
/// which is how `snapcraft` reads it; it never becomes an argument or an error
/// message.
fn store_environment() -> Result<BTreeMap<String, String>> {
    Ok(BTreeMap::from([(
        "SNAPCRAFT_STORE_CREDENTIALS".to_string(),
        credential("SNAPCRAFT_STORE_CREDENTIALS")?,
    )]))
}

fn credential(name: &str) -> Result<String> {
    require_credential(std::env::var(name).ok(), name)
}

fn reject_overrides() -> Result<()> {
    let present = FORBIDDEN_ENVIRONMENT
        .iter()
        .copied()
        .filter(|name| std::env::var_os(name).is_some())
        .collect::<Vec<_>>();
    reject_named_overrides(&present)
}

/// GitHub release adapter driven through the supported `gh` CLI.
pub struct GitHubRelease {
    repository: String,
    workdir: PathBuf,
    /// The `gh` executable.
    ///
    /// Production must keep the absolute system path: resolving `gh` through
    /// PATH would let anything earlier in a release job's PATH substitute a
    /// different program for the one that creates releases. The indirection
    /// exists so tests can drive the real spawn path with a stand-in binary;
    /// it is never read from the environment.
    program: PathBuf,
}

/// The rolling tag that carries the latest successful nightly.
const NIGHTLY: &str = "nightly";

/// Absolute system paths. Publication commands are never resolved through
/// PATH, so nothing earlier in a release job's PATH can stand in for them.
const SYSTEM_GH: &str = "/usr/bin/gh";
const SYSTEM_CURL: &str = "/usr/bin/curl";
const SYSTEM_SNAPCRAFT: &str = "/usr/bin/snapcraft";

/// The record a published rolling release proves, if any.
///
/// A release counts as the success marker only when its body says "complete"
/// and it is not a draft. A body still marked "rolling" is what an interrupted
/// run leaves behind, and a draft was never published at all; treating either
/// as success would report a nightly as delivered when it never reached the
/// edge channel. An unreadable body is likewise not evidence of anything.
pub fn success_marker(release: &Value) -> Option<Value> {
    if release["draft"] == json!(true) {
        return None;
    }
    let body = serde_json::from_str::<Value>(release["body"].as_str()?).ok()?;
    if body["state"] != "complete" {
        return None;
    }
    Some(body["record"].clone())
}

impl GitHubRelease {
    pub fn new(repository: &str, workdir: &Path) -> Result<Self> {
        valid_repository(repository)?;
        // Refuse before any external command is reachable.
        credential("GH_TOKEN")?;
        Ok(Self {
            repository: repository.to_string(),
            workdir: workdir.to_path_buf(),
            program: PathBuf::from(SYSTEM_GH),
        })
    }

    /// Drive a different `gh` executable. For tests that need a stand-in.
    pub fn using_program(mut self, program: &Path) -> Self {
        self.program = program.to_path_buf();
        self
    }

    /// One release by tag, draft or published, or `None` when the tag has no
    /// release yet.
    ///
    /// GitHub's by-tag endpoint returns published releases only, so a draft
    /// this adapter created would read as absent there: preparation would
    /// report it was never created and a retry would create a second one. The
    /// paginated release list shows drafts to a token with push access, so
    /// the release is chosen from it by `tag_name`. Exactly one match is
    /// required; two releases with one tag leave no way to know which one a
    /// user downloads. A failed listing propagates: treating it as "absent"
    /// would let a failed check look like an empty repository.
    fn release(&self, tag: &str) -> Result<Option<Value>> {
        let listing = self.api(&[
            "--paginate",
            &format!("repos/{}/releases?per_page=100", self.repository),
        ])?;
        let mut matches = Vec::new();
        // `gh api --paginate` prints each page as its own JSON array.
        for page in serde_json::Deserializer::from_str(&listing).into_iter::<Value>() {
            for release in page?
                .as_array()
                .ok_or("GitHub release listing is not an array")?
            {
                if release["tag_name"] == tag {
                    matches.push(release.clone());
                }
            }
        }
        match matches.len() {
            0 => Ok(None),
            1 => Ok(matches.pop()),
            _ => Err("more than one GitHub release carries this tag".into()),
        }
    }

    /// The newest published release, which GitHub exposes under a dedicated
    /// path rather than as a tag called "latest".
    ///
    /// Using a tag path here would never resolve, so the previously published
    /// stable version would never be found and the monotonicity and immutability
    /// checks would silently never run.
    fn latest_release(&self) -> Result<Option<Value>> {
        self.release_at(&format!("repos/{}/releases/latest", self.repository))
    }

    /// One release by API path.
    ///
    /// Only "not found" means absent. A transport failure, a rejected credential
    /// or a rate limit must propagate: treating those as "nothing published"
    /// would disable exactly the checks that stop an older tag from replacing a
    /// newer release, and would let a marker be advanced on an unchecked state.
    fn release_at(&self, path: &str) -> Result<Option<Value>> {
        match self.api(&[path]) {
            Ok(body) => Ok(Some(storage::parse_json(body.as_bytes())?)),
            Err(error) if is_not_found(error.as_ref()) => Ok(None),
            Err(error) => Err(error),
        }
    }

    /// The assets currently attached to a release.
    fn assets(&self, tag: &str) -> Result<Vec<Value>> {
        let Some(release) = self.release(tag)? else {
            return Ok(Vec::new());
        };
        Ok(release["assets"]
            .as_array()
            .ok_or("release has no asset list")?
            .clone())
    }

    /// A temporary release body file carrying this record and state.
    ///
    /// The record is written to a file rather than a command argument because it
    /// contains the full asset map; an argument would risk exceeding the platform
    /// limit and would place record contents into a command line.
    fn body_file(&self, record: &Value, state: &str) -> Result<PathBuf> {
        let path = self.workdir.join(format!("release-body-{state}.json"));
        storage::atomic_write(
            &self.workdir,
            Path::new(&path.file_name().ok_or("body file has no name")?),
            &serde_json::to_vec(&json!({"state":state,"record":record}))?,
            0o600,
        )?;
        Ok(path)
    }

    fn api(&self, arguments: &[&str]) -> Result<String> {
        let mut args = vec!["api".to_string()];
        args.extend(arguments.iter().map(|argument| argument.to_string()));
        quiet_output(&self.program, &args, &self.workdir, &github_environment()?)
    }

    /// The immutable source commit the tag identifies, resolved through the ref,
    /// or `None` when the tag does not exist. Only "not found" means absent;
    /// any other failure propagates.
    fn tag_source(&self, tag: &str) -> Result<Option<String>> {
        let reference = match self.api(&[&format!("repos/{}/git/ref/tags/{tag}", self.repository)])
        {
            Ok(reference) => reference,
            Err(error) if is_not_found(error.as_ref()) => return Ok(None),
            Err(error) => return Err(error),
        };
        let mut object = storage::parse_json(reference.as_bytes())?["object"].clone();
        // Annotated tags point at another tag object; walk at most a bounded
        // chain so a maliciously deep reference cannot spin here.
        for _ in 0..8 {
            let kind = object["type"].as_str().unwrap_or_default();
            let sha = object["sha"].as_str().unwrap_or_default().to_string();
            if kind == "commit" {
                return Ok(Some(sha));
            }
            if kind != "tag" {
                return Err("release tag does not resolve to a commit".into());
            }
            let next = self.api(&[&format!("repos/{}/git/tags/{sha}", self.repository)])?;
            object = storage::parse_json(next.as_bytes())?["object"].clone();
        }
        Err("release tag does not resolve to a commit".into())
    }
}

impl Destinations for GitHubRelease {
    fn latest_stable(&mut self) -> Result<Option<Value>> {
        // A repository with no published release yet has no previous version; that is
        // the normal first-publication state.
        let Some(release) = self.latest_release()? else {
            return Ok(None);
        };
        // The release body is `{"state":…,"record":…}`; callers compare the
        // record itself, so returning the whole body would leave every field
        // lookup empty.
        Ok(release["body"]
            .as_str()
            .and_then(|body| serde_json::from_str::<Value>(body).ok())
            .map(|body| body["record"].clone()))
    }

    fn record(&mut self, tag: &str) -> Result<Option<Value>> {
        // A tag with no release yet is an expected state, not a failure: the
        // first publication must be able to create it.
        let Some(release) = self.release(tag)? else {
            return Ok(None);
        };
        Ok(release["body"]
            .as_str()
            .and_then(|body| serde_json::from_str::<Value>(body).ok())
            .map(|body| body["record"].clone()))
    }

    fn prepare(&mut self, record: &Value, assets: &BTreeMap<String, PathBuf>) -> Result<()> {
        let nightly = record["mode"] == "nightly";
        let tag = record["tag"]
            .as_str()
            .ok_or("promotion record has no tag")?;
        let expected_source = record["source_sha"]
            .as_str()
            .ok_or("promotion record has no source")?;
        let body = self.workdir.join("release-body.json");
        fs::write(
            &body,
            serde_json::to_vec(&json!({"state": "prepared", "record": record}))?,
        )?;

        let release = match self.record(tag)? {
            Some(_) => self
                .release(tag)?
                .ok_or("release disappeared while preparing")?,
            None => {
                // A stable release must already have its tag; a nightly creates
                // the tag as part of the release, so requiring it up front would
                // make the first nightly impossible. The nightly release is
                // also a prerelease so it never appears as the newest release.
                let mut arguments = vec![
                    "release".to_string(),
                    "create".to_string(),
                    tag.to_string(),
                    "--repo".to_string(),
                    self.repository.clone(),
                    "--target".to_string(),
                    expected_source.to_string(),
                    "--draft".to_string(),
                ];
                if nightly {
                    arguments.push("--prerelease".into());
                } else {
                    arguments.push("--verify-tag".into());
                }
                arguments.extend([
                    "--title".to_string(),
                    tag.to_string(),
                    "--notes-file".to_string(),
                    body.display().to_string(),
                ]);
                quiet_output(
                    &self.program,
                    &arguments,
                    &self.workdir,
                    &github_environment()?,
                )?;
                self.release(tag)?.ok_or("release was not created")?
            }
        };
        if release["body"]
            .as_str()
            .and_then(|body| serde_json::from_str::<Value>(body).ok())
            .map(|body| body["record"].clone())
            .as_ref()
            != Some(record)
        {
            return Err("GitHub release provenance conflict".into());
        }
        let draft = release["draft"].as_bool().unwrap_or(false);
        match self.tag_source(tag)? {
            Some(source) if source == expected_source => {}
            // GitHub creates a nightly's tag only when its draft release is
            // published, so a first nightly legitimately has no tag yet. A
            // stable release is cut from an existing tag and must have one.
            None if nightly && draft => {}
            _ => return Err("remote immutable release tag/source conflict".into()),
        }

        let existing = release["assets"]
            .as_array()
            .ok_or("GitHub release has no asset list")?
            .iter()
            .map(|asset| (asset["name"].as_str().unwrap_or_default(), asset))
            .collect::<BTreeMap<_, _>>();
        // Anything already attached that this run did not check makes the
        // release untrustworthy: a caller would be shipping bytes no digest was
        // ever verified against. This is deliberately the opposite test from
        // "a checked asset is not attached yet" — that case is handled by the
        // upload branch below, and failing on it would make a fresh draft
        // release impossible to populate.
        if existing.keys().any(|name| !assets.contains_key(*name)) {
            return Err("unexpected immutable release assets".into());
        }
        for (name, path) in assets {
            match existing.get(name.as_str()) {
                // The recorded digest is the only evidence accepted for an
                // attached asset. An API record without one is refused rather
                // than reconciled by downloading and hashing the asset: that
                // needs no download path here and fails safe.
                Some(asset) => {
                    let remote = asset["digest"].as_str().unwrap_or_default();
                    let expected = format!("sha256:{}", storage::sha256(fs::File::open(path)?)?);
                    if remote != expected {
                        return Err("existing GitHub asset digest conflict".into());
                    }
                }
                None => {
                    if !draft {
                        return Err("published immutable release is missing a checked asset".into());
                    }
                    quiet_output(
                        &self.program,
                        &[
                            "release".into(),
                            "upload".into(),
                            tag.into(),
                            path.display().to_string(),
                            "--repo".into(),
                            self.repository.clone(),
                        ],
                        &self.workdir,
                        &github_environment()?,
                    )?;
                }
            }
        }
        Ok(())
    }

    fn publish(&mut self, record: &Value) -> Result<()> {
        let tag = record["tag"]
            .as_str()
            .ok_or("promotion record has no tag")?;
        let expected_source = record["source_sha"]
            .as_str()
            .ok_or("promotion record has no source")?;
        let release = self
            .release(tag)?
            .ok_or("GitHub release to publish is absent")?;
        if release["draft"].as_bool() == Some(true) {
            quiet_output(
                &self.program,
                &[
                    "release".into(),
                    "edit".into(),
                    tag.into(),
                    "--repo".into(),
                    self.repository.clone(),
                    "--draft=false".into(),
                ],
                &self.workdir,
                &github_environment()?,
            )?;
        }
        // Corresponding sources must already be public before a Store channel
        // starts serving the binary.
        if self.tag_source(tag)?.as_deref() != Some(expected_source) {
            return Err(
                "published tag does not identify the checked source; Store promotion refused"
                    .into(),
            );
        }
        Ok(())
    }

    fn marker(&mut self) -> Result<Option<Value>> {
        // The rolling release carries the same `{"state","record"}` body, but
        // with state "complete" only once every destination succeeded. A body
        // left in the "rolling" state by an interrupted run is not a success
        // marker and must be treated as absent.
        let release = match self.release(NIGHTLY)? {
            Some(release) => release,
            None => return Ok(None),
        };
        // A marker counts only when the rolling release is genuinely published.
        Ok(success_marker(&release))
    }

    fn roll(&mut self, record: &Value, assets: &BTreeMap<String, PathBuf>) -> Result<()> {
        let source = record["source_sha"]
            .as_str()
            .ok_or("nightly record has no source")?;
        let pending = self.body_file(record, "rolling")?;
        let complete = self.body_file(record, "complete")?;
        let result = (|| -> Result<()> {
            match self.release(NIGHTLY)? {
                None => {
                    quiet_output(
                        &self.program,
                        &[
                            "release".into(),
                            "create".into(),
                            NIGHTLY.into(),
                            "--repo".into(),
                            self.repository.clone(),
                            "--target".into(),
                            source.into(),
                            "--draft".into(),
                            "--prerelease".into(),
                            "--latest=false".into(),
                            "--notes-file".into(),
                            pending.display().to_string(),
                        ],
                        &self.workdir,
                        &github_environment()?,
                    )?;
                }
                Some(_) => {
                    quiet_output(
                        &self.program,
                        &[
                            "release".into(),
                            "edit".into(),
                            NIGHTLY.into(),
                            "--repo".into(),
                            self.repository.clone(),
                            "--notes-file".into(),
                            pending.display().to_string(),
                        ],
                        &self.workdir,
                        &github_environment()?,
                    )?;
                }
            }
            // Invalidate the success marker BEFORE replacing assets. The body is
            // already "rolling", so an interruption here leaves the marker unset
            // and the next run reconciles instead of claiming success.
            for asset in self.assets(NIGHTLY)? {
                let id = asset["id"].as_u64().ok_or("nightly asset has no id")?;
                self.api(&[
                    "--method",
                    "DELETE",
                    &format!("repos/{}/releases/assets/{id}", self.repository),
                ])?;
            }
            for path in assets.values() {
                quiet_output(
                    &self.program,
                    &[
                        "release".into(),
                        "upload".into(),
                        NIGHTLY.into(),
                        path.display().to_string(),
                        "--repo".into(),
                        self.repository.clone(),
                    ],
                    &self.workdir,
                    &github_environment()?,
                )?;
            }
            self.api(&[
                "--method",
                "PATCH",
                &format!("repos/{}/git/refs/tags/{NIGHTLY}", self.repository),
                "-f",
                &format!("sha={source}"),
                "-F",
                "force=true",
            ])?;
            quiet_output(
                &self.program,
                &[
                    "release".into(),
                    "edit".into(),
                    NIGHTLY.into(),
                    "--repo".into(),
                    self.repository.clone(),
                    "--notes-file".into(),
                    complete.display().to_string(),
                    "--prerelease".into(),
                    "--latest=false".into(),
                    "--draft=false".into(),
                ],
                &self.workdir,
                &github_environment()?,
            )?;
            Ok(())
        })();
        // The body files carry the record either way; they must not outlive the
        // call, success or failure.
        let _ = fs::remove_file(&pending);
        let _ = fs::remove_file(&complete);
        result
    }

    fn ensure(
        &mut self,
        _path: &Path,
        _version: &str,
        _channel: &str,
        _architecture: &str,
    ) -> Result<()> {
        Err("GitHub release is not a Snap Store destination".into())
    }
}

/// Snap Store adapter.
///
/// The digest is the authority: the revision the Store serves must carry the
/// SHA3-384 of the exact bytes that were uploaded. Selecting a revision by
/// version and architecture alone, or trusting that an upload succeeded, would
/// let a different revision take the channel.
pub struct SnapStore {
    workdir: PathBuf,
    credentials: String,
    /// The `curl` executable used for the documented Store API reads.
    curl: PathBuf,
    /// The `snapcraft` executable used for upload and release.
    snapcraft: PathBuf,
}

/// The Store name being promoted.
const SNAP: &str = "among";

/// A credential file that exists only as long as this value does.
struct CredentialFile(PathBuf);

impl Drop for CredentialFile {
    fn drop(&mut self) {
        // Nothing to report from a destructor; a file already gone is the
        // state this guard exists to reach.
        let _ = fs::remove_file(&self.0);
    }
}

/// Revisions that carry this version and this architecture.
pub fn matching_revisions(metadata: &Value, version: &str, architecture: &str) -> Vec<Value> {
    metadata["revisions"]
        .as_array()
        .map(|revisions| {
            revisions
                .iter()
                .filter(|revision| {
                    revision["version"] == version
                        && revision["architectures"]
                            .as_array()
                            .is_some_and(|architectures| {
                                architectures.iter().any(|value| value == architecture)
                            })
                })
                .cloned()
                .collect()
        })
        .unwrap_or_default()
}

/// Whether the channel map shows this exact channel/architecture/revision.
///
/// The triple is matched rather than the revision alone: the same revision can
/// be released to `latest/stable` on one architecture while `latest/stable` on
/// another architecture still points at an older build.
pub fn channel_is(metadata: &Value, channel: &str, architecture: &str, revision: u64) -> bool {
    metadata["channel-map"].as_array().is_some_and(|rows| {
        rows.iter().any(|row| {
            row["channel"] == channel
                && row["architecture"] == architecture
                && row["revision"].as_u64() == Some(revision)
        })
    })
}

impl SnapStore {
    pub fn new(workdir: &Path) -> Result<Self> {
        credential("SNAPCRAFT_STORE_CREDENTIALS")?;
        reject_overrides()?;
        // The variable holds the exported credential itself, as `snapcraft`
        // reads it, not a path to it. Its contents never appear in an
        // argument, an error or a log.
        let credentials = std::env::var("SNAPCRAFT_STORE_CREDENTIALS")
            .ok()
            .filter(|value| !value.trim().is_empty())
            .ok_or("scoped expiring Store credential is unavailable")?;
        // A credential the API reads cannot use must be refused here, before
        // any destination is written, not after GitHub already published.
        crate::store_credentials::authorization(&credentials)?;
        Ok(Self {
            workdir: workdir.to_path_buf(),
            credentials,
            curl: PathBuf::from(SYSTEM_CURL),
            snapcraft: PathBuf::from(SYSTEM_SNAPCRAFT),
        })
    }

    /// Drive different Store executables. For tests that need stand-ins.
    pub fn using_programs(mut self, curl: &Path, snapcraft: &Path) -> Self {
        self.curl = curl.to_path_buf();
        self.snapcraft = snapcraft.to_path_buf();
        self
    }

    /// Read the snap's current revisions and channel map from the documented API.
    ///
    /// The request carries the same scoped credential the upload uses. An
    /// unauthenticated read would report whatever the endpoint chose to return
    /// publicly, which is not evidence about a revision about to take a channel.
    pub fn metadata(&self) -> Result<Value> {
        let header = self.header_file()?;
        let body = quiet_output(
            &self.curl,
            &[
                "--silent".to_string(),
                "--show-error".to_string(),
                "--fail".to_string(),
                "--header".to_string(),
                "Accept: application/json".to_string(),
                // The macaroon is passed through a private header file, never as
                // an argument: a process command line is readable through /proc.
                format!("--header@{}", header.0.display()),
                // The channel-map document carries BOTH the revision list and the
                // channel map; the revisions-only endpoint has no channel rows,
                // so reading it would make a channel confirmation impossible.
                format!("https://dashboard.snapcraft.io/api/v2/snaps/{SNAP}/channel-map"),
            ],
            &self.workdir,
            &store_environment()?,
        )?;
        let document = storage::parse_json(body.as_bytes())?;
        for field in ["revisions", "channel-map"] {
            if document[field].as_array().is_none() {
                return Err(format!("Store metadata has no {field}").into());
            }
        }
        Ok(document)
    }

    /// A private file holding the `Authorization` header for `curl`.
    ///
    /// Writing the credential to a mode-0600 file keeps it out of the process
    /// argument list, where any local reader could see it through `/proc`. The
    /// file is removed when the returned value is dropped, on success and on
    /// error alike, so the credential does not outlive the request in the
    /// work directory.
    fn header_file(&self) -> Result<CredentialFile> {
        let path = self.workdir.join("store-authorization.header");
        storage::atomic_write(
            &self.workdir,
            Path::new(&path.file_name().ok_or("header file has no name")?),
            format!("Authorization: {}\n", self.authorization()?).as_bytes(),
            0o600,
        )?;
        Ok(CredentialFile(path))
    }

    /// The value used for the API's `Authorization` header: the root macaroon
    /// and the discharge bound to it. It is never logged or placed in an error.
    fn authorization(&self) -> Result<String> {
        crate::store_credentials::authorization(&self.credentials)
    }
}

impl Destinations for SnapStore {
    fn latest_stable(&mut self) -> Result<Option<Value>> {
        Ok(None)
    }
    // The rolling marker is a GitHub release body, not Store state: the Store
    // only ever learns whether a nightly succeeded, never that it did.
    fn marker(&mut self) -> Result<Option<Value>> {
        Ok(None)
    }
    fn roll(&mut self, _record: &Value, _assets: &BTreeMap<String, PathBuf>) -> Result<()> {
        Err("the Snap Store cannot roll the nightly marker".into())
    }
    fn record(&mut self, _tag: &str) -> Result<Option<Value>> {
        Ok(None)
    }
    fn prepare(&mut self, _record: &Value, _assets: &BTreeMap<String, PathBuf>) -> Result<()> {
        Ok(())
    }
    fn publish(&mut self, _record: &Value) -> Result<()> {
        Ok(())
    }

    fn ensure(
        &mut self,
        path: &Path,
        version: &str,
        channel: &str,
        architecture: &str,
    ) -> Result<()> {
        if !crate::promotion::ARCHITECTURES.contains(&architecture) {
            return Err("unsupported Store architecture".into());
        }
        let expected = sha3::hex(fs::File::open(path)?)?;

        // Reconcile against what the Store already serves before uploading.
        // Re-uploading a revision that already matches would create a second
        // revision for the same bytes and make "the highest matching revision"
        // ambiguous.
        let mut metadata = self.metadata()?;
        let mut revisions = matching_revisions(&metadata, version, architecture);
        if revisions.is_empty() {
            quiet_output(
                &self.snapcraft,
                &["upload".into(), path.display().to_string()],
                &self.workdir,
                &store_environment()?,
            )
            .map_err(|_| "Store upload failed; retry reconciles already uploaded revisions")?;
            metadata = self.metadata()?;
            revisions = matching_revisions(&metadata, version, architecture);
        }
        // Every candidate for this version/architecture must carry our digest.
        // Accepting one that matches while another conflicts would let a later
        // retry pick the wrong revision.
        if revisions.is_empty()
            || revisions
                .iter()
                .any(|revision| revision["sha3-384"] != expected)
        {
            return Err(
                "Store revision hash unavailable or conflicting; no channel is changed".into(),
            );
        }
        let revision = revisions
            .iter()
            .filter_map(|revision| revision["revision"].as_u64())
            .max()
            .ok_or("Store revision number absent")?;

        if !channel_is(&metadata, channel, architecture, revision) {
            quiet_output(
                &self.snapcraft,
                &[
                    "release".into(),
                    SNAP.into(),
                    revision.to_string(),
                    channel.into(),
                ],
                &self.workdir,
                &store_environment()?,
            )?;
        }
        // Confirm the channel itself moved. Re-reading the revision would prove
        // nothing: a correct revision can exist while the channel still points
        // elsewhere, which is the failure this check exists to catch.
        let refreshed = self.metadata()?;
        if !channel_is(&refreshed, channel, architecture, revision) {
            return Err("Store channel advancement was not observed".into());
        }
        Ok(())
    }
}

/// Groups are shared by the release inventory and both adapters.
pub fn groups_of(record: &Value) -> Result<BTreeMap<String, Group>> {
    crate::promotion::artifact_groups(record)
}
