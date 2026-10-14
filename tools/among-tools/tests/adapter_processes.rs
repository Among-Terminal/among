//! The real GitHub and Store adapters, driven against fake external commands.
//!
//! These are process-level fakes: `gh`, `snapcraft` and `curl` are real
//! executable files that the adapter actually spawns. Nothing is mocked inside
//! the process, so the argument vectors, the environment and the exit codes
//! under test are the ones a release job would produce.
//!
//! Fakes of the `Destinations` trait alone cannot catch a wrong argument, a
//! missing credential or a swallowed error; these tests can. They cover the
//! adapters' read paths and their write paths: GitHub prepare, publish and
//! roll, and the Store upload and release behind `ensure`, including retries
//! that must write nothing and failures part-way through.

use among_tools::{
    promotion::Destinations,
    promotion_destinations::{GitHubRelease, SnapStore},
    storage,
    store_credentials::base64_urlsafe_encode,
    Result,
};
use serde_json::{json, Value};
use std::{
    ffi::OsString,
    fs,
    os::unix::fs::{DirBuilderExt, PermissionsExt},
    path::{Path, PathBuf},
    sync::{Mutex, MutexGuard},
};

static CREDENTIAL_ENVIRONMENT: Mutex<()> = Mutex::new(());
const FIXTURE_ENVIRONMENT: [&str; 5] = [
    "GH_TOKEN",
    "SNAPCRAFT_STORE_CREDENTIALS",
    "STORE_DASHBOARD_URL",
    "STORE_UPLOAD_URL",
    "SNAPCRAFT_STORE_AUTH",
];

// These adapters read process credentials lazily. Each fixture must own that
// environment until its last child exits, or parallel tests erase each other's
// synthetic token and produce nondeterministic authentication failures.
struct Fixture(PathBuf, MutexGuard<'static, ()>, [Option<OsString>; 5]);

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
        // The guard field stays owned until environment restoration finishes.
        let Self(_, _guard, original) = self;
        for (name, value) in FIXTURE_ENVIRONMENT.into_iter().zip(original) {
            unsafe {
                match value {
                    Some(value) => std::env::set_var(name, value),
                    None => std::env::remove_var(name),
                }
            }
        }
    }
}

/// A directory holding fake `gh`/`snapcraft`/`curl` plus the log they append to.
///
/// Each fake appends its own argv and the presence of each credential to a
/// log, then emits a canned response. That is enough to assert what a real
/// release job would send and would have authenticated with.
fn fixture() -> Result<Fixture> {
    let guard = CREDENTIAL_ENVIRONMENT
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let original = FIXTURE_ENVIRONMENT.map(std::env::var_os);
    let root = storage::private_directory(&std::env::temp_dir(), "among-adapter-proc")?;
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(root.join("bin"))?;
    fs::write(root.join("gh.out"), b"")?;
    let fixture = Fixture(root, guard, original);
    // Never let an inherited real credential reach one of the fake programs.
    unsafe {
        for name in FIXTURE_ENVIRONMENT {
            std::env::remove_var(name);
        }
    }
    Ok(fixture)
}

#[test]
fn panicking_fixture_restores_environment_without_cascading_failures() -> Result<()> {
    let fixture = fixture()?;
    let original = fixture.2.clone();
    let unwound = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
        let _fixture = fixture;
        for name in FIXTURE_ENVIRONMENT {
            unsafe { std::env::set_var(name, "synthetic-panic-value") };
        }
        // An unwind is the event under test. `resume_unwind` starts one
        // without the panic hook, so the lint that keeps `panic!` out of
        // this crate still guards every place a panic would be a defect.
        std::panic::resume_unwind(Box::new("synthetic fixture unwind"));
    }));
    assert!(unwound.is_err());
    let restored = self::fixture()?;
    assert_eq!(restored.2, original);
    Ok(())
}

const GH: &str = r#"#!/bin/sh
log="$(dirname "$0")/../gh.out"
for arg in "$@"; do printf 'gh %s\n' "$arg" >> "$log"; done
[ -n "${GH_TOKEN:-}" ] && printf 'gh AUTH=%s\n' "${GH_TOKEN}" >> "$log"
case "$*" in
  *"releases/latest"*) printf '{"body":"{\"state\":\"complete\",\"record\":{\"tag\":\"v0.1.0\",\"source_sha\":\"%s\"}}"}' "$(cat "$(dirname "$0")/../source")" ;;
  *"git/ref/tags/"*) printf '{"object":{"type":"commit","sha":"%s"}}' "$(cat "$(dirname "$0")/../source")" ;;
  *"releases?per_page=100"*)
    # `gh api --paginate` prints one JSON array per page, back to back.
    printf '[{"tag_name":"v9.9.9","draft":false,"assets":[],"body":""}]'
    if [ -f "$(dirname "$0")/../release.json" ]; then printf '[%s]' "$(cat "$(dirname "$0")/../release.json")"; else printf '[]'; fi ;;
  *"releases/tags/"*) if [ -f "$(dirname "$0")/../release.json" ]; then cat "$(dirname "$0")/../release.json"; else printf 'HTTP 404: Not Found\n' >&2; exit 1; fi ;;
  *) printf '{}' ;;
esac
"#;

const SNAPCRAFT: &str = r#"#!/bin/sh
log="$(dirname "$0")/../gh.out"
for arg in "$@"; do printf 'snapcraft %s\n' "$arg" >> "$log"; done
[ -n "${SNAPCRAFT_STORE_CREDENTIALS:-}" ] && printf 'snapcraft AUTH=%s\n' "$SNAPCRAFT_STORE_CREDENTIALS" >> "$log"
printf 'uploaded\n'
"#;

const CURL: &str = r#"#!/bin/sh
log="$(dirname "$0")/../gh.out"
for arg in "$@"; do printf 'curl %s\n' "$arg" >> "$log"; done
cat "$(dirname "$0")/../channel-map.json"
"#;

/// What `snapcraft export-login` writes and the release job puts in
/// `SNAPCRAFT_STORE_CREDENTIALS`: base64 of an Ubuntu One credential document
/// holding a root and a discharge macaroon, here minimal version 1 macaroons
/// with synthetic signatures.
fn exported_login() -> String {
    let macaroon = |identifier: &str, signature: u8| {
        let mut bytes = Vec::new();
        for (key, value) in [
            (&b"location"[..], &b"login.ubuntu.com"[..]),
            (b"identifier", identifier.as_bytes()),
            (b"signature", &[signature; 32]),
        ] {
            let body = [key, b" ", value, b"\n"].concat();
            bytes.extend(format!("{:04x}", body.len() + 4).into_bytes());
            bytes.extend(body);
        }
        base64_urlsafe_encode(&bytes)
    };
    let document =
        json!({"t":"u1-macaroon","v":{"r":macaroon("root",1),"d":macaroon("discharge",2)}});
    base64_urlsafe_encode(document.to_string().as_bytes())
}

fn install(root: &Path, name: &str, body: &str) -> Result<()> {
    let path = root.join("bin").join(name);
    fs::write(&path, body)?;
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755))?;
    Ok(())
}

fn log_of(root: &Path) -> Result<String> {
    Ok(fs::read_to_string(root.join("gh.out"))?)
}

/// An amd64 release that GitHub already knows about, as a real response body.
fn release_body(source: &str) -> Result<String> {
    Ok(serde_json::to_string(&json!({
        "tag_name": "v0.1.0",
        "draft": false,
        "body": json!({"state":"complete","record":{"tag":"v0.1.0","source_sha":source}}).to_string(),
        "assets": []
    }))?)
}

#[test]
fn github_reads_a_published_record_with_a_credential_and_no_wrong_arguments() -> Result<()> {
    let fixture = fixture()?;
    let source = "c".repeat(40);
    fs::write(fixture.0.join("source"), &source)?;
    fs::write(fixture.0.join("release.json"), release_body(&source)?)?;
    install(&fixture.0, "gh", GH)?;

    unsafe { std::env::set_var("GH_TOKEN", "test-token") };
    let gh = fixture.0.join("bin/gh");
    let mut github = GitHubRelease::new("fixture/among", &fixture.0)?.using_program(&gh);
    let observed = github.record("v0.1.0")?;
    unsafe { std::env::remove_var("GH_TOKEN") };

    let record = observed.ok_or("a published release was not observed")?;
    assert_eq!(
        record["tag"], "v0.1.0",
        "the body must be unwrapped to the record"
    );
    assert_eq!(record["source_sha"], source);

    let log = log_of(&fixture.0)?;
    assert!(
        log.contains("gh AUTH=test-token"),
        "the credential was not passed to gh: {log}"
    );
    assert!(
        log.contains("repos/fixture/among/releases?per_page=100"),
        "the API path was wrong: {log}"
    );
    Ok(())
}

/// A tag with no release is an expected state, not a failure. `gh` exits
/// non-zero on HTTP 404, and treating that as an error would make the very
/// first publication impossible.
#[test]
fn a_missing_release_is_absent_rather_than_an_error() -> Result<()> {
    let fixture = fixture()?;
    fs::write(fixture.0.join("source"), "d".repeat(40))?;
    // No release.json: the fake answers HTTP 404 and exits 1.
    install(&fixture.0, "gh", GH)?;

    unsafe { std::env::set_var("GH_TOKEN", "test-token") };
    let gh = fixture.0.join("bin/gh");
    let mut github = GitHubRelease::new("fixture/among", &fixture.0)?.using_program(&gh);
    let observed = github.record("v9.9.9");
    unsafe { std::env::remove_var("GH_TOKEN") };

    let observed = observed?;
    assert!(observed.is_none(), "a 404 was reported as a release");
    Ok(())
}

/// The rolling marker counts only a genuinely published release. The fake
/// returns state "rolling", which is what an interrupted run leaves behind.
#[test]
fn a_rolling_release_body_is_not_read_as_a_success_marker() -> Result<()> {
    let fixture = fixture()?;
    fs::write(fixture.0.join("source"), "e".repeat(40))?;
    fs::write(
        fixture.0.join("release.json"),
        serde_json::to_string(&json!({
            "tag_name": "nightly",
            "draft": false,
            "body": json!({"state":"rolling","record":{"tag":"nightly","source_sha":"e"}}).to_string(),
        }))?,
    )?;
    install(&fixture.0, "gh", GH)?;

    unsafe { std::env::set_var("GH_TOKEN", "test-token") };
    let gh = fixture.0.join("bin/gh");
    let mut github = GitHubRelease::new("fixture/among", &fixture.0)?.using_program(&gh);
    let marker = github.marker();
    unsafe { std::env::remove_var("GH_TOKEN") };

    assert!(
        marker?.is_none(),
        "an interrupted rolling release was read as a completed nightly"
    );
    Ok(())
}

/// A Store read must be authenticated with the Store credential. An
/// unauthenticated read reports whatever the endpoint returns publicly, which
/// is not evidence about a revision about to take a channel.
/// A Store read must actually happen, carrying the Store credential and no
/// GitHub token.
///
/// An earlier version of this test only constructed the adapter and asserted
/// that nothing had been logged, so it would have passed even if every Store
/// call were unauthenticated. It now drives a real read.
#[test]
fn a_store_read_carries_the_store_credential_and_not_a_github_token() -> Result<()> {
    let fixture = fixture()?;
    install(&fixture.0, "snapcraft", SNAPCRAFT)?;
    // The curl fake logs whether an Authorization header was supplied and
    // whether a GitHub token leaked into the same call.
    fs::write(
        fixture.0.join("bin/curl"),
        r#"#!/bin/sh
log="$(dirname "$0")/../gh.out"
for arg in "$@"; do
  printf 'curl %s\n' "$arg" >> "$log"
  case "$arg" in *--header@*) printf 'curl AUTHORIZED\n' >> "$log" ;; esac
  case "$arg" in *GH_TOKEN*) printf 'curl LEAKED_GITHUB\n' >> "$log" ;; esac
done
[ -n "${GH_TOKEN:-}" ] && printf 'curl HAS_GITHUB_TOKEN\n' >> "$log"
header="$(dirname "$0")/../store-authorization.header"
[ -f "$header" ] && printf 'curl HEADER_FILE_PRESENT\n' >> "$log" && cp "$header" "$(dirname "$0")/../header.seen"
cat "$(dirname "$0")/../channel-map.json"
"#,
    )?;
    fs::set_permissions(
        fixture.0.join("bin/curl"),
        fs::Permissions::from_mode(0o755),
    )?;
    let credentials = exported_login();
    fs::write(
        fixture.0.join("channel-map.json"),
        json!({
            "revisions": [{"version":"0.1.13","architectures":["amd64"],"revision":41,"sha3-384":"deadbeef"}],
            "channel-map": [{"channel":"latest/stable","architecture":"amd64","revision":41}]
        })
        .to_string(),
    )?;

    // A GitHub token is deliberately absent: the Store must not need one.
    unsafe { std::env::remove_var("GH_TOKEN") };
    unsafe { std::env::set_var("SNAPCRAFT_STORE_CREDENTIALS", &credentials) };
    let built = SnapStore::new(&fixture.0)?.using_programs(
        &fixture.0.join("bin/curl"),
        &fixture.0.join("bin/snapcraft"),
    );
    // The macaroon must not be in the child's argv.
    let observed = built.metadata();
    unsafe { std::env::remove_var("SNAPCRAFT_STORE_CREDENTIALS") };
    observed?;

    let log = log_of(&fixture.0)?;
    assert!(
        log.contains("curl AUTHORIZED"),
        "the Store read was not authenticated: {log}"
    );
    assert!(
        !log.contains("curl HAS_GITHUB_TOKEN"),
        "the Store call was given a GitHub token: {log}"
    );
    assert!(
        !log.contains(&credentials) && !log.contains("Macaroon"),
        "the credential appeared in the child's arguments: {log}"
    );
    // curl received exactly the header the documented Store API expects:
    // the exported root and the discharge bound to it.
    assert_eq!(
        fs::read_to_string(fixture.0.join("header.seen"))?,
        format!(
            "Authorization: {}\n",
            among_tools::store_credentials::authorization(&credentials)?
        )
    );
    // The header file must exist while curl reads it and be gone afterwards:
    // a credential left in the work directory outlives the publication run.
    assert!(
        log.contains("curl HEADER_FILE_PRESENT"),
        "curl ran without the private header file: {log}"
    );
    assert!(
        !fixture.0.join("store-authorization.header").exists(),
        "the Store credential file was left behind after the request"
    );
    Ok(())
}

/// A failure that is NOT a 404 must never read as "no release exists".
///
/// This is the difference between "this release is absent" and "this could not
/// be checked". Treating the second as the first silently disables the stable
/// monotonicity and immutability checks, which is how an older tag ends up
/// replacing a newer release.
#[test]
fn a_transport_or_auth_failure_is_never_reported_as_an_absent_release() -> Result<()> {
    let fixture = fixture()?;
    fs::write(fixture.0.join("source"), "f".repeat(40))?;
    // The fake fails without mentioning a 404, as an expired credential or a
    // network fault would.
    fs::write(
        fixture.0.join("bin/gh"),
        r#"#!/bin/sh
printf 'gh API error: could not authenticate (token ghp_EXAMPLE)\n' >&2
exit 1
"#,
    )?;
    fs::set_permissions(fixture.0.join("bin/gh"), fs::Permissions::from_mode(0o755))?;

    unsafe { std::env::set_var("GH_TOKEN", "test-token") };
    let gh = fixture.0.join("bin/gh");
    let mut github = GitHubRelease::new("fixture/among", &fixture.0)?.using_program(&gh);
    let observed = github.record("v0.1.0");
    unsafe { std::env::remove_var("GH_TOKEN") };

    assert!(
        observed.is_err(),
        "an authentication failure was reported as an absent release"
    );
    // The credential must not be echoed into the error either.
    let message = observed.err().ok_or("expected a failure")?.to_string();
    assert!(
        !message.contains("ghp_EXAMPLE"),
        "the failure text leaked credential-bearing output: {message}"
    );
    Ok(())
}

/// The previous stable release must be found through GitHub's dedicated
/// latest-release path. Reading it as a tag called "latest" never resolves, so
/// the monotonicity check would never run at all.
#[test]
fn the_previous_stable_release_is_read_from_the_latest_release_path() -> Result<()> {
    let fixture = fixture()?;
    let source = "a".repeat(40);
    fs::write(fixture.0.join("source"), &source)?;
    fs::write(
        fixture.0.join("bin/gh"),
        r#"#!/bin/sh
log="$(dirname "$0")/../gh.out"
for arg in "$@"; do printf 'gh %s\n' "$arg" >> "$log"; done
case "$*" in
  *"/releases/latest"*) printf '{"body":"{\"state\":\"complete\",\"record\":{\"tag\":\"v9.9.9\"}}"}' ;;
  *) printf 'HTTP 404: Not Found\n' >&2; exit 1 ;;
esac
"#,
    )?;
    fs::set_permissions(fixture.0.join("bin/gh"), fs::Permissions::from_mode(0o755))?;

    unsafe { std::env::set_var("GH_TOKEN", "test-token") };
    let gh = fixture.0.join("bin/gh");
    let mut github = GitHubRelease::new("fixture/among", &fixture.0)?.using_program(&gh);
    let observed = github.latest_stable();
    unsafe { std::env::remove_var("GH_TOKEN") };

    let record = observed?.ok_or("the published stable release was not found")?;
    assert_eq!(record["tag"], "v9.9.9");
    let log = log_of(&fixture.0)?;
    assert!(
        log.contains("repos/fixture/among/releases/latest"),
        "the latest release was not read from its own path: {log}"
    );
    assert!(
        !log.contains("releases/tags/latest"),
        "the latest release was requested as if it were a tag: {log}"
    );
    Ok(())
}

/// The unsupported-architecture refusal must happen before any upload, so a
/// typo cannot push a revision to the Store under a wrong architecture.
#[test]
fn an_unsupported_architecture_never_reaches_the_store_command() -> Result<()> {
    let fixture = fixture()?;
    install(&fixture.0, "curl", CURL)?;
    install(&fixture.0, "snapcraft", SNAPCRAFT)?;
    let credentials = exported_login();
    fs::write(
        fixture.0.join("channel-map.json"),
        json!({"revisions": [], "channel-map": []}).to_string(),
    )?;
    unsafe { std::env::set_var("SNAPCRAFT_STORE_CREDENTIALS", &credentials) };
    let built = SnapStore::new(&fixture.0);
    unsafe { std::env::remove_var("SNAPCRAFT_STORE_CREDENTIALS") };
    let mut store = built?.using_programs(
        &fixture.0.join("bin/curl"),
        &fixture.0.join("bin/snapcraft"),
    );

    let snap = fixture.0.join("among.snap");
    fs::write(&snap, b"synthetic snap bytes")?;
    let refusal = store
        .ensure(&snap, "0.1.13", "latest/stable", "riscv64")
        .err()
        .ok_or("an unsupported architecture was accepted")?
        .to_string();
    assert!(refusal.contains("architecture"), "{refusal}");
    assert!(
        !log_of(&fixture.0)?.contains("snapcraft upload"),
        "an upload ran despite the architecture refusal"
    );
    Ok(())
}

/// A `gh` stand-in for a repository where the release and its tag do not
/// exist yet. `release create` records the draft release from the notes file
/// the adapter wrote; the tag stays absent, as it does for a draft until the
/// release is published. As GitHub documents, the by-tag endpoint returns only
/// published releases, so the draft is visible only in the release list.
const FIRST_RELEASE_GH: &str = r#"#!/bin/sh
root="$(dirname "$0")/.."
for arg in "$@"; do printf 'gh %s\n' "$arg" >> "$root/gh.out"; done
case "$*" in
  "release create"*)
    notes=""; previous=""
    for arg in "$@"; do
      [ "$previous" = "--notes-file" ] && notes="$arg"
      previous="$arg"
    done
    body="$(sed 's/\\/\\\\/g; s/"/\\"/g' "$notes")"
    printf '{"tag_name":"%s","draft":true,"assets":[],"body":"%s"}' "$3" "$body" > "$root/release.json" ;;
  *"git/ref/tags/"*) printf 'HTTP 404: Not Found\n' >&2; exit 1 ;;
  *"releases?per_page=100"*) if [ -f "$root/release.json" ]; then printf '[%s]' "$(cat "$root/release.json")"; else printf '[]'; fi ;;
  *"releases/tags/"*) printf 'HTTP 404: Not Found\n' >&2; exit 1 ;;
  *) printf '{}' ;;
esac
"#;

fn first_release(mode: Option<&str>) -> Result<(Fixture, Result<()>)> {
    let fixture = fixture()?;
    install(&fixture.0, "gh", FIRST_RELEASE_GH)?;
    let artifact = fixture.0.join("among-amd64.snap");
    fs::write(&artifact, b"synthetic nightly snap")?;
    let mut record = json!({
        "tag": "nightly-20261013",
        "source_sha": "f".repeat(40),
        "assets": {"among-amd64.snap": storage::sha256(fs::File::open(&artifact)?)?},
    });
    if let Some(mode) = mode {
        record["mode"] = json!(mode);
    }
    let assets = std::collections::BTreeMap::from([("among-amd64.snap".to_string(), artifact)]);
    unsafe { std::env::set_var("GH_TOKEN", "test-token") };
    let gh = fixture.0.join("bin/gh");
    let outcome = GitHubRelease::new("fixture/among", &fixture.0)?
        .using_program(&gh)
        .prepare(&record, &assets);
    unsafe { std::env::remove_var("GH_TOKEN") };
    Ok((fixture, outcome))
}

/// The first nightly creates its tag only when the draft release is
/// published, so preparing it must accept a tag that does not exist yet and
/// go on to attach the checked artifact.
#[test]
fn the_first_nightly_is_prepared_before_its_tag_exists() -> Result<()> {
    let (fixture, outcome) = first_release(Some("nightly"))?;
    outcome?;
    let log = log_of(&fixture.0)?;
    assert!(
        log.contains("gh create"),
        "no draft release was created: {log}"
    );
    assert!(
        log.contains("gh upload"),
        "the checked artifact was not attached: {log}"
    );
    Ok(())
}

/// A stable release is cut from an existing tag. If that tag is missing the
/// release cannot be bound to its source, so preparation must stop before any
/// artifact is attached.
#[test]
fn a_stable_release_without_its_tag_is_refused_before_upload() -> Result<()> {
    let (fixture, outcome) = first_release(None)?;
    assert_eq!(
        outcome.map_err(|error| error.to_string()),
        Err("remote immutable release tag/source conflict".to_string())
    );
    assert!(!log_of(&fixture.0)?.contains("gh upload"));
    Ok(())
}

/// Store stand-ins with state. `curl` serves `store.json`; `snapcraft upload`
/// replaces it with `after-upload.json` and `snapcraft release` with
/// `after-release.json`, so each test decides what the Store shows after
/// each write and every call is logged.
const STATEFUL_CURL: &str = r#"#!/bin/sh
root="$(dirname "$0")/.."
printf 'curl read\n' >> "$root/gh.out"
cat "$root/store.json"
"#;
const STATEFUL_SNAPCRAFT: &str = r#"#!/bin/sh
root="$(dirname "$0")/.."
printf 'snapcraft %s\n' "$*" >> "$root/gh.out"
case "$1" in
  upload) cp "$root/after-upload.json" "$root/store.json" ;;
  release) cp "$root/after-release.json" "$root/store.json" ;;
esac
"#;

struct Store {
    fixture: Fixture,
    snap: PathBuf,
    digest: String,
}

impl Store {
    fn new() -> Result<Self> {
        let fixture = fixture()?;
        install(&fixture.0, "curl", STATEFUL_CURL)?;
        install(&fixture.0, "snapcraft", STATEFUL_SNAPCRAFT)?;
        let snap = fixture.0.join("among.snap");
        fs::write(&snap, b"synthetic snap bytes")?;
        let digest = among_tools::sha3::hex(fs::File::open(&snap)?)?;
        Ok(Self {
            fixture,
            snap,
            digest,
        })
    }

    fn state(&self, name: &str, revisions: Value, channels: Value) -> Result<()> {
        fs::write(
            self.fixture.0.join(name),
            json!({"revisions": revisions, "channel-map": channels}).to_string(),
        )?;
        Ok(())
    }

    fn revision(&self, number: u64, architecture: &str, digest: &str) -> Value {
        json!({"revision": number, "version": "0.1.13", "architectures": [architecture], "sha3-384": digest})
    }

    fn ensure(&self, architecture: &str) -> Result<String> {
        // Every Store command reads the credential from the environment, so it
        // stays set for the whole operation; the fixture restores it on drop.
        unsafe { std::env::set_var("SNAPCRAFT_STORE_CREDENTIALS", exported_login()) };
        let outcome = SnapStore::new(&self.fixture.0).and_then(|store| {
            store
                .using_programs(
                    &self.fixture.0.join("bin/curl"),
                    &self.fixture.0.join("bin/snapcraft"),
                )
                .ensure(&self.snap, "0.1.13", "latest/stable", architecture)
        });
        unsafe { std::env::remove_var("SNAPCRAFT_STORE_CREDENTIALS") };
        let log = log_of(&self.fixture.0)?;
        outcome
            .map(|()| log.clone())
            .map_err(|error| format!("{error}; calls: {log}").into())
    }

    fn calls(&self) -> Result<String> {
        log_of(&self.fixture.0)
    }
}

fn channel(revision: u64, architecture: &str) -> Value {
    json!([{"channel": "latest/stable", "architecture": architecture, "revision": revision}])
}

/// A revision with these exact bytes already serving the channel is the end
/// state; a retry must neither upload a duplicate nor release again.
#[test]
fn an_already_released_matching_revision_is_reconciled_without_any_write() -> Result<()> {
    let store = Store::new()?;
    let revisions = json!([store.revision(41, "amd64", &store.digest)]);
    store.state("store.json", revisions, channel(41, "amd64"))?;
    let calls = store.ensure("amd64")?;
    assert!(!calls.contains("snapcraft"), "{calls}");
    Ok(())
}

/// The upload of an earlier attempt already landed: release that revision
/// rather than upload the same bytes again.
#[test]
fn an_uploaded_but_unreleased_revision_is_released_without_reupload() -> Result<()> {
    let store = Store::new()?;
    let revisions = json!([store.revision(41, "amd64", &store.digest)]);
    store.state("store.json", revisions.clone(), json!([]))?;
    store.state("after-release.json", revisions, channel(41, "amd64"))?;
    let calls = store.ensure("amd64")?;
    assert!(!calls.contains("snapcraft upload"), "{calls}");
    assert!(
        calls.contains("snapcraft release among 41 latest/stable"),
        "{calls}"
    );
    Ok(())
}

/// With nothing in the Store, upload, read the new revision's digest and only
/// then release exactly that revision.
#[test]
fn a_new_upload_is_released_only_after_its_digest_is_confirmed() -> Result<()> {
    let store = Store::new()?;
    store.state("store.json", json!([]), json!([]))?;
    let uploaded = json!([store.revision(42, "amd64", &store.digest)]);
    store.state("after-upload.json", uploaded.clone(), json!([]))?;
    store.state("after-release.json", uploaded, channel(42, "amd64"))?;
    let calls = store.ensure("amd64")?;
    let upload = calls.find("snapcraft upload").ok_or("no upload")?;
    let release = calls
        .find("snapcraft release among 42 latest/stable")
        .ok_or("no release")?;
    assert!(upload < release, "{calls}");
    Ok(())
}

/// A revision of this version and architecture with other bytes means the
/// Store holds something this run did not check: no channel may change.
#[test]
fn a_conflicting_store_digest_never_reaches_a_release() -> Result<()> {
    let store = Store::new()?;
    let revisions = json!([store.revision(41, "amd64", &"0".repeat(96))]);
    store.state("store.json", revisions, json!([]))?;
    let refusal = store
        .ensure("amd64")
        .err()
        .ok_or("a conflicting digest was accepted")?
        .to_string();
    assert!(refusal.contains("conflicting"), "{refusal}");
    assert!(!store.calls()?.contains("snapcraft"));
    Ok(())
}

/// An upload whose digest does not come back as the uploaded bytes' digest
/// is not released either.
#[test]
fn an_upload_with_an_unconfirmed_digest_is_not_released() -> Result<()> {
    let store = Store::new()?;
    store.state("store.json", json!([]), json!([]))?;
    store.state(
        "after-upload.json",
        json!([{"revision": 42, "version": "0.1.13", "architectures": ["amd64"]}]),
        json!([]),
    )?;
    assert!(store.ensure("amd64").is_err());
    let calls = store.calls()?;
    assert!(calls.contains("snapcraft upload") && !calls.contains("snapcraft release"));
    Ok(())
}

/// The arm64 channel must get an arm64 revision even when an amd64 revision
/// of the same version carries the same digest.
#[test]
fn arm64_never_reuses_an_amd64_revision() -> Result<()> {
    let store = Store::new()?;
    let amd64 = store.revision(41, "amd64", &store.digest);
    store.state("store.json", json!([amd64]), channel(41, "amd64"))?;
    let both = json!([amd64, store.revision(43, "arm64", &store.digest)]);
    store.state("after-upload.json", both.clone(), channel(41, "amd64"))?;
    store.state("after-release.json", both, channel(43, "arm64"))?;
    let calls = store.ensure("arm64")?;
    assert!(calls.contains("snapcraft upload"), "{calls}");
    assert!(
        calls.contains("snapcraft release among 43 latest/stable"),
        "{calls}"
    );
    Ok(())
}

/// A release command that exits successfully is not evidence: the channel
/// map must show the revision afterwards.
#[test]
fn a_release_the_channel_map_does_not_show_is_a_failure() -> Result<()> {
    let store = Store::new()?;
    let revisions = json!([store.revision(41, "amd64", &store.digest)]);
    store.state("store.json", revisions.clone(), json!([]))?;
    store.state("after-release.json", revisions, json!([]))?;
    let refusal = store
        .ensure("amd64")
        .err()
        .ok_or("an unobserved release was accepted")?
        .to_string();
    assert!(refusal.contains("not observed"), "{refusal}");
    Ok(())
}

/// A `gh` stand-in that keeps one release as state, so a sequence of writes
/// can be observed the way GitHub would answer it.
///
/// The release exists while `draft` exists; `tag`, `body` and `assets` are
/// what the API returns for it. As GitHub documents, `releases/tags/TAG`
/// returns a published release only, so a draft is visible only in the
/// paginated release list. `release create` and a `release edit` with a notes file
/// copy that file into `body` (the adapter deletes its body files after the
/// call, so the copy is the only record of what was sent), and `--draft=false`
/// publishes. `tag-source` is the commit the tag resolves to; without it the
/// tag is absent. `fail-upload` and `fail-edit` make those calls fail the way
/// a rejected `gh` call does. Every call is logged as one line, so tests can
/// compare the exact sequence.
const STATEFUL_GH: &str = r#"#!/bin/sh
root="$(dirname "$0")/.."
[ -n "${GH_TOKEN:-}" ] || { printf 'gh without credential\n' >> "$root/gh.out"; exit 3; }
printf 'gh %s\n' "$*" >> "$root/gh.out"
notes=""; previous=""
for arg in "$@"; do
  [ "$previous" = "--notes-file" ] && notes="$arg"
  previous="$arg"
done
release() {
  body="$(sed 's/\\/\\\\/g; s/"/\\"/g' "$root/body")"
  printf '{"tag_name":"%s","draft":%s,"assets":%s,"body":"%s"}' "$(cat "$root/tag")" \
    "$(cat "$root/draft")" "$(cat "$root/assets")" "$body"
}
case "$1 $2" in
  "release create")
    cp "$notes" "$root/body"; printf true > "$root/draft"; printf '[]' > "$root/assets"
    printf '%s' "$3" > "$root/tag" ;;
  "release edit")
    [ ! -f "$root/fail-edit" ] || { printf 'edit refused\n' >&2; exit 1; }
    [ -z "$notes" ] || cp "$notes" "$root/body"
    case " $* " in *" --draft=false "*) printf false > "$root/draft" ;; esac ;;
  "release upload")
    [ ! -f "$root/fail-upload" ] || { printf 'upload refused\n' >&2; exit 1; } ;;
  "api --method") ;;
  "api "*"/git/ref/tags/"*)
    if [ -f "$root/tag-source" ]; then
      printf '{"object":{"type":"commit","sha":"%s"}}' "$(cat "$root/tag-source")"
    else printf 'HTTP 404: Not Found\n' >&2; exit 1; fi ;;
  "api --paginate")
    # One JSON array per page, back to back, as `gh api --paginate` prints
    # them; the first page holds an unrelated release.
    printf '[{"tag_name":"v9.9.9","draft":false,"assets":[],"body":""}]'
    if [ -f "$root/draft" ]; then printf '[%s]' "$(release)"; else printf '[]'; fi ;;
  "api "*"/releases/tags/"*)
    # GitHub documents this endpoint as returning a published release only.
    if [ -f "$root/draft" ] && [ "$(cat "$root/draft")" = false ]; then
      release
    else printf 'HTTP 404: Not Found\n' >&2; exit 1; fi ;;
  *) printf '{}' ;;
esac
"#;

/// A repository driven through `STATEFUL_GH`, with one checked artifact.
struct Repository {
    fixture: Fixture,
    source: String,
    artifact: PathBuf,
}

impl Repository {
    fn new() -> Result<Self> {
        let fixture = fixture()?;
        install(&fixture.0, "gh", STATEFUL_GH)?;
        let artifact = fixture.0.join("among-amd64.snap");
        fs::write(&artifact, b"synthetic checked snap")?;
        Ok(Self {
            fixture,
            source: "b".repeat(40),
            artifact,
        })
    }

    fn root(&self) -> &Path {
        &self.fixture.0
    }

    fn record(&self, tag: &str) -> Result<Value> {
        Ok(json!({
            "tag": tag,
            "source_sha": self.source,
            "assets": {"among-amd64.snap": storage::sha256(fs::File::open(&self.artifact)?)?},
        }))
    }

    fn nightly_record(&self) -> Result<Value> {
        let mut record = self.record("nightly-20261013")?;
        record["mode"] = json!("nightly");
        Ok(record)
    }

    fn assets(&self) -> std::collections::BTreeMap<String, PathBuf> {
        std::collections::BTreeMap::from([("among-amd64.snap".to_string(), self.artifact.clone())])
    }

    /// The SHA-256 digest GitHub reports for the checked artifact.
    fn artifact_digest(&self) -> Result<String> {
        Ok(format!(
            "sha256:{}",
            storage::sha256(fs::File::open(&self.artifact)?)?
        ))
    }

    /// Put an existing release in place: draft or published, the body GitHub
    /// holds for it and the assets attached to it. A nightly record lives on
    /// the rolling `nightly` release; any other record on its own tag.
    fn release(&self, draft: bool, body: &Value, assets: Value) -> Result<()> {
        let tag = if body["record"]["mode"] == "nightly" {
            "nightly"
        } else {
            body["record"]["tag"]
                .as_str()
                .ok_or("release body has no tag")?
        };
        fs::write(self.root().join("tag"), tag)?;
        fs::write(self.root().join("draft"), draft.to_string())?;
        fs::write(self.root().join("body"), body.to_string())?;
        fs::write(self.root().join("assets"), assets.to_string())?;
        Ok(())
    }

    fn tag_points_to(&self, source: &str) -> Result<()> {
        fs::write(self.root().join("tag-source"), source)?;
        Ok(())
    }

    fn fail(&self, call: &str) -> Result<()> {
        fs::write(self.root().join(format!("fail-{call}")), b"")?;
        Ok(())
    }

    /// Run one adapter operation with the credential set for its whole
    /// duration, as a release job has it.
    fn drive<T>(&self, operation: impl FnOnce(&mut GitHubRelease) -> Result<T>) -> Result<T> {
        unsafe { std::env::set_var("GH_TOKEN", "test-token") };
        let outcome = GitHubRelease::new("fixture/among", self.root()).and_then(|github| {
            let mut github = github.using_program(&self.root().join("bin/gh"));
            operation(&mut github)
        });
        unsafe { std::env::remove_var("GH_TOKEN") };
        outcome
    }

    /// Every `gh` invocation in order, without the `gh ` prefix.
    fn calls(&self) -> Result<Vec<String>> {
        Ok(log_of(self.root())?
            .lines()
            .filter_map(|line| line.strip_prefix("gh "))
            .map(str::to_string)
            .collect())
    }

    /// The body GitHub holds for the release now.
    fn body(&self) -> Result<Value> {
        storage::parse_json(&fs::read(self.root().join("body"))?)
    }

    fn path(&self, name: &str) -> String {
        self.root().join(name).display().to_string()
    }
}

fn message<T>(outcome: Result<T>) -> Result<String> {
    Ok(outcome
        .err()
        .ok_or("the operation was expected to fail")?
        .to_string())
}

/// Releases, drafts included, are found through the paginated list; the
/// by-tag endpoint would not show a draft.
const RELEASES: &str = "api --paginate repos/fixture/among/releases?per_page=100";
const RELEASE_V010: &str = RELEASES;
const TAG_V010: &str = "api repos/fixture/among/git/ref/tags/v0.1.0";
const RELEASE_NIGHTLY: &str = RELEASES;

/// Publishing a prepared draft turns exactly that release public and then
/// confirms its tag names the checked commit: the Store may only start serving
/// a binary once its corresponding source is public.
#[test]
fn publishing_a_prepared_draft_makes_it_public_and_confirms_its_tag() -> Result<()> {
    let repository = Repository::new()?;
    let record = repository.record("v0.1.0")?;
    repository.release(
        true,
        &json!({"state":"prepared","record":record}),
        json!([]),
    )?;
    repository.tag_points_to(&repository.source)?;
    repository.drive(|github| github.publish(&record))?;
    assert_eq!(
        repository.calls()?,
        [
            RELEASE_V010,
            "release edit v0.1.0 --repo fixture/among --draft=false",
            TAG_V010,
        ]
    );
    assert_eq!(
        fs::read_to_string(repository.root().join("draft"))?,
        "false"
    );
    Ok(())
}

/// A retry after a successful publication must not touch the public release
/// again; it only re-confirms the tag.
#[test]
fn publishing_an_already_public_release_writes_nothing() -> Result<()> {
    let repository = Repository::new()?;
    let record = repository.record("v0.1.0")?;
    repository.release(
        false,
        &json!({"state":"prepared","record":record}),
        json!([]),
    )?;
    repository.tag_points_to(&repository.source)?;
    repository.drive(|github| github.publish(&record))?;
    assert_eq!(repository.calls()?, [RELEASE_V010, TAG_V010]);
    Ok(())
}

/// A public release whose tag names another commit does not carry the checked
/// source, so the Store step after it must be refused.
#[test]
fn a_published_tag_naming_another_commit_refuses_store_promotion() -> Result<()> {
    let repository = Repository::new()?;
    let record = repository.record("v0.1.0")?;
    repository.release(
        false,
        &json!({"state":"prepared","record":record}),
        json!([]),
    )?;
    repository.tag_points_to(&"9".repeat(40))?;
    assert_eq!(
        message(repository.drive(|github| github.publish(&record)))?,
        "published tag does not identify the checked source; Store promotion refused"
    );
    assert_eq!(repository.calls()?, [RELEASE_V010, TAG_V010]);
    Ok(())
}

/// Publishing a release that was never prepared is a failure, not a silent
/// success with nothing published.
#[test]
fn publishing_a_missing_release_fails_without_any_write() -> Result<()> {
    let repository = Repository::new()?;
    let record = repository.record("v0.1.0")?;
    repository.tag_points_to(&repository.source)?;
    assert_eq!(
        message(repository.drive(|github| github.publish(&record)))?,
        "GitHub release to publish is absent"
    );
    assert_eq!(repository.calls()?, [RELEASE_V010]);
    Ok(())
}

/// When GitHub rejects the publication, the failure surfaces without the
/// command's own output and nothing after it runs.
#[test]
fn a_rejected_publication_stops_before_the_tag_check() -> Result<()> {
    let repository = Repository::new()?;
    let record = repository.record("v0.1.0")?;
    repository.release(
        true,
        &json!({"state":"prepared","record":record}),
        json!([]),
    )?;
    repository.tag_points_to(&repository.source)?;
    repository.fail("edit")?;
    assert_eq!(
        message(repository.drive(|github| github.publish(&record)))?,
        "protected destination operation failed; see the protected job log"
    );
    assert_eq!(
        repository.calls()?,
        [
            RELEASE_V010,
            "release edit v0.1.0 --repo fixture/among --draft=false",
        ]
    );
    Ok(())
}

/// Re-running preparation against a release that already carries this record
/// and the checked bytes must upload nothing.
#[test]
fn preparing_again_with_the_same_record_and_bytes_uploads_nothing() -> Result<()> {
    let repository = Repository::new()?;
    let record = repository.record("v0.1.0")?;
    repository.release(
        true,
        &json!({"state":"prepared","record":record}),
        json!([{"name":"among-amd64.snap","digest":repository.artifact_digest()?}]),
    )?;
    repository.tag_points_to(&repository.source)?;
    repository.drive(|github| github.prepare(&record, &repository.assets()))?;
    assert_eq!(repository.calls()?, [RELEASE_V010, RELEASE_V010, TAG_V010]);
    Ok(())
}

/// A release prepared for another record belongs to another candidate; adding
/// these bytes to it would mix two provenances in one release.
#[test]
fn a_release_prepared_for_another_record_is_a_provenance_conflict() -> Result<()> {
    let repository = Repository::new()?;
    let record = repository.record("v0.1.0")?;
    let mut other = record.clone();
    other["source_sha"] = json!("a".repeat(40));
    repository.release(true, &json!({"state":"prepared","record":other}), json!([]))?;
    repository.tag_points_to(&repository.source)?;
    assert_eq!(
        message(repository.drive(|github| github.prepare(&record, &repository.assets())))?,
        "GitHub release provenance conflict"
    );
    assert!(!repository
        .calls()?
        .iter()
        .any(|call| call.starts_with("release")));
    Ok(())
}

/// An attached asset whose recorded digest differs from the checked bytes
/// means the release already ships something else under that name.
#[test]
fn an_attached_asset_with_another_digest_is_a_conflict() -> Result<()> {
    let repository = Repository::new()?;
    let record = repository.record("v0.1.0")?;
    repository.release(
        true,
        &json!({"state":"prepared","record":record}),
        json!([{"name":"among-amd64.snap","digest":format!("sha256:{}", "0".repeat(64))}]),
    )?;
    repository.tag_points_to(&repository.source)?;
    assert_eq!(
        message(repository.drive(|github| github.prepare(&record, &repository.assets())))?,
        "existing GitHub asset digest conflict"
    );
    assert!(!repository
        .calls()?
        .iter()
        .any(|call| call.starts_with("release")));
    Ok(())
}

/// An attached asset with no recorded digest is not evidence of the checked
/// bytes either; it is refused rather than trusted.
#[test]
fn an_attached_asset_without_a_digest_is_a_conflict() -> Result<()> {
    let repository = Repository::new()?;
    let record = repository.record("v0.1.0")?;
    repository.release(
        true,
        &json!({"state":"prepared","record":record}),
        json!([{"name":"among-amd64.snap"}]),
    )?;
    repository.tag_points_to(&repository.source)?;
    assert_eq!(
        message(repository.drive(|github| github.prepare(&record, &repository.assets())))?,
        "existing GitHub asset digest conflict"
    );
    Ok(())
}

/// An attached asset this run never checked would ship unverified bytes.
#[test]
fn an_attached_asset_outside_the_record_is_refused() -> Result<()> {
    let repository = Repository::new()?;
    let record = repository.record("v0.1.0")?;
    repository.release(
        true,
        &json!({"state":"prepared","record":record}),
        json!([{"name":"unchecked.snap","digest":repository.artifact_digest()?}]),
    )?;
    repository.tag_points_to(&repository.source)?;
    assert_eq!(
        message(repository.drive(|github| github.prepare(&record, &repository.assets())))?,
        "unexpected immutable release assets"
    );
    assert!(!repository
        .calls()?
        .iter()
        .any(|call| call.starts_with("release")));
    Ok(())
}

/// A public release cannot gain assets: it would change what users have
/// already downloaded under that tag.
#[test]
fn a_public_release_missing_a_checked_asset_is_not_amended() -> Result<()> {
    let repository = Repository::new()?;
    let record = repository.record("v0.1.0")?;
    repository.release(
        false,
        &json!({"state":"prepared","record":record}),
        json!([]),
    )?;
    repository.tag_points_to(&repository.source)?;
    assert_eq!(
        message(repository.drive(|github| github.prepare(&record, &repository.assets())))?,
        "published immutable release is missing a checked asset"
    );
    assert!(!repository
        .calls()?
        .iter()
        .any(|call| call.starts_with("release")));
    Ok(())
}

/// An existing stable tag naming another commit binds the release to the
/// wrong source.
#[test]
fn an_existing_tag_naming_another_commit_is_refused_before_upload() -> Result<()> {
    let repository = Repository::new()?;
    let record = repository.record("v0.1.0")?;
    repository.release(
        true,
        &json!({"state":"prepared","record":record}),
        json!([]),
    )?;
    repository.tag_points_to(&"9".repeat(40))?;
    assert_eq!(
        message(repository.drive(|github| github.prepare(&record, &repository.assets())))?,
        "remote immutable release tag/source conflict"
    );
    assert!(!repository
        .calls()?
        .iter()
        .any(|call| call.starts_with("release")));
    Ok(())
}

/// The first nightly creates the rolling release as a draft prerelease that
/// never becomes "latest", attaches the artifact, moves the tag and only then
/// publishes it with a "complete" body. Afterwards that body is the success
/// marker a repeated nightly is compared against, and no body file is left in
/// the work directory.
#[test]
fn the_first_roll_creates_uploads_moves_the_tag_and_completes_last() -> Result<()> {
    let repository = Repository::new()?;
    let record = repository.nightly_record()?;
    let marker = repository.drive(|github| {
        github.roll(&record, &repository.assets())?;
        github.marker()
    })?;
    let source = &repository.source;
    assert_eq!(
        repository.calls()?,
        [
            RELEASE_NIGHTLY.to_string(),
            format!(
                "release create nightly --repo fixture/among --target {source} --draft \
                 --prerelease --latest=false --notes-file {}",
                repository.path("release-body-rolling.json")
            ),
            RELEASE_NIGHTLY.to_string(),
            format!(
                "release upload nightly {} --repo fixture/among",
                repository.artifact.display()
            ),
            format!(
                "api --method PATCH repos/fixture/among/git/refs/tags/nightly \
                 -f sha={source} -F force=true"
            ),
            format!(
                "release edit nightly --repo fixture/among --notes-file {} --prerelease \
                 --latest=false --draft=false",
                repository.path("release-body-complete.json")
            ),
            RELEASE_NIGHTLY.to_string(),
        ]
    );
    assert_eq!(
        repository.body()?,
        json!({"state":"complete","record":record})
    );
    assert_eq!(marker, Some(record));
    for name in ["release-body-rolling.json", "release-body-complete.json"] {
        assert!(
            !repository.root().join(name).exists(),
            "{name} was left behind"
        );
    }
    Ok(())
}

/// Rolling onto an existing nightly first rewrites its body to "rolling", so
/// the previous success marker is gone before any asset is removed, then
/// deletes every old asset and uploads the new one.
#[test]
fn rolling_an_existing_nightly_invalidates_the_marker_before_replacing_assets() -> Result<()> {
    let repository = Repository::new()?;
    let record = repository.nightly_record()?;
    let mut previous = record.clone();
    previous["source_sha"] = json!("a".repeat(40));
    repository.release(
        false,
        &json!({"state":"complete","record":previous}),
        json!([{"id":7,"name":"among-amd64.snap"},{"id":8,"name":"among-arm64.snap"}]),
    )?;
    repository.drive(|github| github.roll(&record, &repository.assets()))?;
    let calls = repository.calls()?;
    assert_eq!(
        calls[..6],
        [
            RELEASE_NIGHTLY.to_string(),
            format!(
                "release edit nightly --repo fixture/among --notes-file {}",
                repository.path("release-body-rolling.json")
            ),
            RELEASE_NIGHTLY.to_string(),
            "api --method DELETE repos/fixture/among/releases/assets/7".to_string(),
            "api --method DELETE repos/fixture/among/releases/assets/8".to_string(),
            format!(
                "release upload nightly {} --repo fixture/among",
                repository.artifact.display()
            ),
        ]
    );
    assert_eq!(calls.len(), 8, "{calls:?}");
    assert_eq!(
        repository.body()?,
        json!({"state":"complete","record":record})
    );
    Ok(())
}

/// An upload that fails part-way leaves the rolling body in place: the tag is
/// not moved, the release is not completed, no success marker is readable and
/// a retry reconciles instead of reporting the nightly delivered.
#[test]
fn a_failed_roll_upload_leaves_no_success_marker() -> Result<()> {
    let repository = Repository::new()?;
    let record = repository.nightly_record()?;
    let mut previous = record.clone();
    previous["source_sha"] = json!("a".repeat(40));
    repository.release(
        false,
        &json!({"state":"complete","record":previous}),
        json!([{"id":7,"name":"among-amd64.snap"}]),
    )?;
    repository.fail("upload")?;
    let failure = message(repository.drive(|github| github.roll(&record, &repository.assets())))?;
    assert_eq!(
        failure,
        "protected destination operation failed; see the protected job log"
    );
    let calls = repository.calls()?;
    assert!(
        calls
            .last()
            .is_some_and(|call| call.starts_with("release upload")),
        "{calls:?}"
    );
    assert!(
        !calls.iter().any(|call| call.contains("PATCH")),
        "{calls:?}"
    );
    assert!(
        !calls.iter().any(|call| call.contains("--draft=false")),
        "{calls:?}"
    );
    assert_eq!(
        repository.body()?,
        json!({"state":"rolling","record":record})
    );
    assert_eq!(repository.drive(|github| github.marker())?, None);
    for name in ["release-body-rolling.json", "release-body-complete.json"] {
        assert!(
            !repository.root().join(name).exists(),
            "{name} was left behind"
        );
    }
    Ok(())
}

/// An attached asset the API reports without an id cannot be removed, so the
/// roll stops before uploading anything next to it.
#[test]
fn a_nightly_asset_without_an_id_stops_the_roll_before_upload() -> Result<()> {
    let repository = Repository::new()?;
    let record = repository.nightly_record()?;
    repository.release(
        false,
        &json!({"state":"complete","record":record}),
        json!([{"name":"among-amd64.snap"}]),
    )?;
    assert_eq!(
        message(repository.drive(|github| github.roll(&record, &repository.assets())))?,
        "nightly asset has no id"
    );
    let calls = repository.calls()?;
    assert!(
        !calls
            .iter()
            .any(|call| call.contains("upload") || call.contains("DELETE")),
        "{calls:?}"
    );
    Ok(())
}

/// A first roll interrupted after creating its draft leaves a draft nightly
/// with assets. The retry must find that draft, which only the release list
/// shows, rewrite it and remove its assets, never create a second release
/// for the same tag.
#[test]
fn a_roll_after_an_interrupted_first_roll_reuses_the_draft() -> Result<()> {
    let repository = Repository::new()?;
    let record = repository.nightly_record()?;
    repository.release(
        true,
        &json!({"state":"rolling","record":record}),
        json!([{"id":7,"name":"among-amd64.snap"}]),
    )?;
    repository.drive(|github| github.roll(&record, &repository.assets()))?;
    let calls = repository.calls()?;
    assert!(
        !calls.iter().any(|call| call.starts_with("release create")),
        "{calls:?}"
    );
    assert!(
        calls.contains(&"api --method DELETE repos/fixture/among/releases/assets/7".to_string()),
        "{calls:?}"
    );
    assert_eq!(
        repository.body()?,
        json!({"state":"complete","record":record})
    );
    Ok(())
}

/// Two releases carrying one tag leave no way to know which one users see,
/// so the lookup refuses rather than picking one.
#[test]
fn two_releases_with_one_tag_are_refused() -> Result<()> {
    let fixture = fixture()?;
    install(
        &fixture.0,
        "gh",
        r#"#!/bin/sh
printf '[{"tag_name":"v0.1.0","draft":true}][{"tag_name":"v0.1.0","draft":false}]'
"#,
    )?;
    unsafe { std::env::set_var("GH_TOKEN", "test-token") };
    let observed = GitHubRelease::new("fixture/among", &fixture.0).and_then(|github| {
        github
            .using_program(&fixture.0.join("bin/gh"))
            .record("v0.1.0")
    });
    unsafe { std::env::remove_var("GH_TOKEN") };
    assert_eq!(
        observed.map_err(|error| error.to_string()),
        Err("more than one GitHub release carries this tag".to_string())
    );
    Ok(())
}
