//! The real GitHub and Store adapters, driven against fake external commands.
//!
//! These are process-level fakes: `gh`, `snapcraft` and `curl` are real
//! executable files that the adapter actually spawns. Nothing is mocked inside
//! the process, so the argument vectors, the environment and the exit codes
//! under test are the ones a release job would produce.
//!
//! This is the coverage that the retained Python `test-release-adapters.py`
//! used to provide and that no Rust test did: the adapters were previously
//! exercised only through hand-written fakes of the `Destinations` trait, which
//! cannot catch a wrong argument, a missing credential or a swallowed error.

use among_tools::{
    promotion::Destinations,
    promotion_destinations::{GitHubRelease, SnapStore},
    storage, Result,
};
use serde_json::json;
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
        log.contains("repos/fixture/among/releases/tags/v0.1.0"),
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
cat "$(dirname "$0")/../channel-map.json"
"#,
    )?;
    fs::set_permissions(
        fixture.0.join("bin/curl"),
        fs::Permissions::from_mode(0o755),
    )?;
    let credentials = fixture.0.join("credentials.json");
    fs::write(&credentials, r#"{"macaroon":"discharge-token"}"#)?;
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
        !log.contains("discharge-token"),
        "the credential appeared in the child's arguments: {log}"
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
    let credentials = fixture.0.join("credentials.json");
    fs::write(&credentials, r#"{"macaroon":"discharge-token"}"#)?;
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
