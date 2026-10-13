use among_tools::{nightly, storage, Result};
use serde_json::{json, Value};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

struct Fixture(PathBuf);
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn record() -> Value {
    let source = "a".repeat(40);
    json!({"mode":"nightly","source_sha":source,"tag":format!("nightly-{source}"),"version":format!("n20261007-{}",&source[..12])})
}
#[test]
fn missing_unchanged_and_partial_next_day_retries_preserve_original_artifacts() -> Result<()> {
    let source = "a".repeat(40);
    let now = "2026-10-08T03:17:00+00:00";
    let mut reads = Vec::new();
    let selected = nightly::select(&source, now, |tag| {
        reads.push(tag.to_string());
        Ok(None)
    })?;
    assert_eq!(selected["skip"], false);
    assert_eq!(selected["resume"], false);
    assert_eq!(selected["version"], format!("n20261008-{}", &source[..12]));
    assert_eq!(
        reads,
        vec!["nightly".to_string(), format!("nightly-{source}")]
    );
    reads.clear();
    let unchanged = nightly::select(&source, now, |tag| {
        reads.push(tag.to_string());
        Ok(Some((record(), true)))
    })?;
    assert_eq!(unchanged, json!({"source_sha":source,"skip":true}));
    assert_eq!(reads, vec!["nightly"]);
    let resumed = nightly::select(&source, now, |tag| Ok(Some((record(), tag != "nightly"))))?;
    assert_eq!(resumed["resume"], true);
    assert_eq!(resumed["version"], record()["version"]);
    for (key, value) in [
        ("mode", json!("stable")),
        ("source_sha", json!("b".repeat(40))),
        ("tag", json!("nightly-other")),
        ("version", json!(format!("n20261007-{}", "b".repeat(12)))),
    ] {
        let mut changed = record();
        changed[key] = value;
        assert!(nightly::selection(&source, None, now, Some(&changed)).is_err());
    }
    Ok(())
}

#[test]
fn endpoint_failure_is_not_missing_and_never_exposes_diagnostic_credentials() -> Result<()> {
    assert!(nightly::release_response(false, b"", "HTTP 404 fixture")
        .is_ok_and(|result| result.is_none()));
    let error = nightly::release_response(false, b"synthetic-secret", "HTTP 403 synthetic-secret")
        .err()
        .ok_or("endpoint failure was accepted")?;
    assert!(!error.to_string().contains("synthetic-secret"));
    assert!(error.to_string().contains("not missing state"));
    for (state, draft, complete) in [
        ("complete", false, true),
        ("complete", true, false),
        ("prepared", false, false),
        ("rolling", false, false),
    ] {
        let response = json!({"draft":draft,"body":serde_json::to_string(&json!({"state":state,"record":record()}))?});
        assert_eq!(
            nightly::release_response(true, &serde_json::to_vec(&response)?, "")?
                .ok_or("valid release absent")?
                .1,
            complete
        );
    }
    let mut changed = record();
    changed["source_sha"] = "short".into();
    let response = json!({"draft":false,"body":serde_json::to_string(&json!({"state":"complete","record":changed}))?});
    assert!(nightly::release_response(true, &serde_json::to_vec(&response)?, "").is_err());
    assert!(nightly::selection("short", None, "2026-10-08", None).is_err());
    Ok(())
}

#[test]
fn actual_cli_resolves_git_once_reads_only_and_preserves_existing_output() -> Result<()> {
    let f = Fixture(storage::private_directory(
        &std::env::temp_dir(),
        "among-nightly-fixture",
    )?);
    let git = |args: &[&str]| -> Result<String> {
        let result = std::process::Command::new("/usr/bin/git")
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .arg("-C")
            .arg(&f.0)
            .args(args)
            .output()?;
        if !result.status.success() {
            return Err("fixture Git command failed".into());
        }
        Ok(String::from_utf8(result.stdout)?.trim().into())
    };
    git(&["init", "-q", "-b", "main"])?;
    git(&["config", "user.name", "Synthetic Fixture"])?;
    git(&["config", "user.email", "fixture@example.invalid"])?;
    fs::write(f.0.join("source"), b"synthetic")?;
    git(&["add", "source"])?;
    git(&[
        "-c",
        "commit.gpgSign=false",
        "commit",
        "-qm",
        "synthetic source",
    ])?;
    let source = git(&["rev-parse", "HEAD"])?;
    git(&["update-ref", "refs/remotes/origin/main", &source])?;
    fs::create_dir(f.0.join("bin"))?;
    let gh = f.0.join("bin/gh");
    fs::write(&gh,format!("#!/bin/sh\n[ \"$1\" = api ] || exit 8\nprintf '%s\\n' \"$2\" >> '{}'\nprintf 'HTTP 404 fixture\\n' >&2\nexit 1\n",f.0.join("reads").display()))?;
    fs::set_permissions(&gh, fs::Permissions::from_mode(0o755))?;
    let output = f.0.join("selection.json");
    let run = || {
        std::process::Command::new(env!("CARGO_BIN_EXE_among-tools"))
            .env_clear()
            .env("PATH", f.0.join("bin"))
            .env("GH_TOKEN", "synthetic-secret")
            .env("AMONG_CHECK_ROOT", f.0.join("checks"))
            .arg("select-nightly")
            .arg(&f.0)
            .arg("fixture/among")
            .arg(&output)
            .args(["--now", "2026-10-08T03:17:00Z"])
            .output()
    };
    let result = run()?;
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert_eq!(result.stdout, b"build\n");
    let bytes = fs::read(&output)?;
    let selected = among_tools::storage::parse_json(&bytes)?;
    assert_eq!(selected["source_sha"], source);
    assert_eq!(selected["version"], format!("n20261008-{}", &source[..12]));
    assert_eq!(
        fs::read_to_string(f.0.join("reads"))?
            .lines()
            .collect::<Vec<_>>(),
        vec![
            "repos/fixture/among/releases/tags/nightly".to_string(),
            format!("repos/fixture/among/releases/tags/nightly-{source}")
        ]
    );
    assert!(!run()?.status.success());
    assert_eq!(fs::read(&output)?, bytes);
    fs::write(
        &gh,
        "#!/bin/sh\nprintf 'HTTP 403 synthetic-secret\\n' >&2\nexit 1\n",
    )?;
    let failed = run()?;
    assert!(!failed.status.success());
    assert!(!String::from_utf8_lossy(&failed.stderr).contains("synthetic-secret"));
    assert_eq!(fs::read(output)?, bytes);
    Ok(())
}

/// The nightly version carries the UTC date of the run. A timestamp without a
/// timezone names no instant, so it is refused rather than read as UTC; one
/// with an offset is converted, which can move the date across a day, a month
/// or a year.
#[test]
fn nightly_date_is_the_utc_date_of_a_timestamp_with_an_explicit_timezone() -> Result<()> {
    let source = "a".repeat(40);
    for (now, date) in [
        ("2026-10-08T03:17:00Z", "20261008"),
        ("2026-10-08T03:17:00+00:00", "20261008"),
        ("2026-10-08T23:30:00-02:00", "20261009"),
        ("2026-12-31T23:30:00.250-01:00", "20270101"),
        ("2026-03-01T00:30+02:00", "20260228"),
        ("2028-03-01T01:00:00+05:30", "20280229"),
    ] {
        let selected = nightly::selection(&source, None, now, None)?;
        assert_eq!(
            selected["version"],
            format!("n{date}-{}", &source[..12]),
            "{now}"
        );
    }
    for now in [
        "2026-10-08",
        "2026-10-08T03:17:00",
        "2026-02-30T00:00:00Z",
        "2026-10-08T24:00:00Z",
        "2026-10-08T03:17:00+25:00",
        "2026-10-08T03:17:00 UTC",
    ] {
        assert!(
            nightly::selection(&source, None, now, None).is_err(),
            "accepted {now}"
        );
    }
    Ok(())
}
