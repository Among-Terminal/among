//! real clock-managed and legacy clock binaries from the exact companion source;
//! package receipts here remain synthetic and do not certify release installation.

use super::*;
use crate::plugin::host::environment;
use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

const CLOCK: &str = "github:1431055733/clock";

fn fixture() -> Fixture {
    let fixture = Fixture::with_binary(
        "clock",
        &["status"],
        &[],
        "AMONG_MANAGED_CLOCK_BINARY",
        "AMONG_MANAGED_CLOCK_SHA256",
    );
    assert_eq!(fixture.key, CLOCK);
    fixture
}

fn seconds(text: &str) -> u64 {
    assert_eq!(text.len(), 8);
    assert_eq!(&text[2..3], ":");
    assert_eq!(&text[5..6], ":");
    let fields = text
        .split(':')
        .map(|field| field.parse::<u64>().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(fields.len(), 3);
    assert!(fields[0] < 24 && fields[1] < 60 && fields[2] < 60);
    fields[0] * 3600 + fields[1] * 60 + fields[2]
}

fn epoch() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

fn legacy_time() -> u64 {
    let executable = PathBuf::from(
        std::env::var_os("AMONG_LEGACY_CLOCK_BINARY").expect("explicit pinned legacy reference"),
    );
    let metadata = std::fs::symlink_metadata(&executable).unwrap();
    assert!(executable.is_absolute() && metadata.is_file() && metadata.mode() & 0o111 != 0);
    assert!(metadata.len() <= 32 * 1024 * 1024);
    let checksum = inventory::checksum(&std::fs::read(&executable).unwrap()).unwrap();
    assert_eq!(
        checksum,
        std::env::var("AMONG_LEGACY_CLOCK_SHA256").unwrap()
    );
    let mut command = Command::new(executable);
    environment::configure(&mut command).unwrap();
    let output = environment::capture_for_test(
        command,
        b"{\"type\":\"status\",\"request_id\":91,\"scope\":\"app_footer\",\"context\":{}}\n",
    );
    let row: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(row["type"], "status");
    assert_eq!(row["request_id"], 91);
    assert_eq!(row["refresh_ms"], 1000);
    assert_eq!(row["fragments"].as_array().unwrap().len(), 1);
    seconds(row["fragments"][0]["text"].as_str().unwrap())
}

#[test]
fn real_clock_reports_application_footer_in_inherited_timezone() {
    let case = std::env::var("AMONG_CLOCK_CASE").expect("explicit isolated timezone case");
    let timezone = std::env::var_os("TZ");
    let offset = match case.as_str() {
        "utc" => {
            assert_eq!(timezone.as_deref(), Some(OsStr::new("UTC")));
            Some(0)
        }
        "tokyo" => {
            assert_eq!(timezone.as_deref(), Some(OsStr::new("Asia/Tokyo")));
            Some(9 * 3600)
        }
        "unset" => {
            assert!(timezone.is_none());
            None
        }
        "empty" => {
            assert_eq!(timezone.as_deref(), Some(OsStr::new("")));
            None
        }
        _ => panic!("unsupported timezone case"),
    };
    let mut fixture = fixture();
    fixture.enable(&["status"]);
    assert_eq!(
        fixture.host.plugins[0].runtime.as_ref().unwrap().grants,
        BTreeSet::from(["status".to_string()])
    );
    assert_eq!(fixture.host.configuration(CLOCK), Configuration::NotApplied);
    for scope in ["pane_footer:1", "workspace_summary:0"] {
        assert!(fixture.host.world.scope(scope).is_some());
        assert!(fixture
            .host
            .request_status(CLOCK, scope, &Context::default())
            .is_none());
    }
    let generation = fixture.host.running_generation(CLOCK).unwrap();
    let mut observed = Vec::new();
    for iteration in 0..2 {
        let before = epoch();
        let legacy_before = offset.is_none().then(legacy_time);
        let id = fixture
            .host
            .request_status(CLOCK, "app_footer", &Context::default())
            .unwrap();
        assert!(fixture.host.plugins[0].inflight.contains_key(&id));
        fixture.wait(|host| !host.results.is_empty());
        assert!(!fixture.host.plugins[0].inflight.contains_key(&id));
        let results = fixture.host.drain_results();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].plugin, CLOCK);
        assert_eq!(results[0].scope, "app_footer");
        assert_eq!(results[0].refresh, Refresh::IntervalMs(1000));
        assert_eq!(results[0].fragments.len(), 1);
        let fragment = &results[0].fragments[0];
        assert_eq!(fragment.tooltip.as_deref(), Some("Local time"));
        assert_eq!(fragment.priority, 5);
        let actual = seconds(fragment.text.as_deref().unwrap());
        let legacy_after = offset.is_none().then(legacy_time);
        let after = epoch();
        assert!(
            after >= before && after - before <= 12,
            "reference-time fence changed"
        );
        if let Some(offset) = offset {
            assert!((before..=after).any(|second| (second + offset) % 86400 == actual));
        } else {
            let start = legacy_before.unwrap();
            let span = (legacy_after.unwrap() + 86400 - start) % 86400;
            assert!(span <= 12);
            assert!((actual + 86400 - start) % 86400 <= span);
        }
        observed.push(actual);
        if iteration == 0 {
            std::thread::sleep(Duration::from_millis(1100));
        }
    }
    assert_ne!(
        observed[0], observed[1],
        "actual host clock did not advance"
    );
    assert_eq!(fixture.host.running_generation(CLOCK), Some(generation));
    fixture
        .host
        .request_status(CLOCK, "app_header", &Context::default())
        .unwrap();
    fixture.wait(|host| !host.results.is_empty());
    let results = fixture.host.drain_results();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].scope, "app_header");
    assert!(results[0].fragments.is_empty());
    assert_eq!(results[0].refresh, Refresh::IntervalMs(1000));
    println!(
        "clock_case={case} clock_sha256={} legacy_clock_sha256={} footer_time_observed=true",
        std::env::var("AMONG_MANAGED_CLOCK_SHA256").unwrap(),
        std::env::var("AMONG_LEGACY_CLOCK_SHA256").unwrap()
    );
    fixture.stop();
}

#[test]
fn invalid_host_timezone_refuses_clock_before_spawn() {
    let case = std::env::var("AMONG_CLOCK_CASE").expect("explicit isolated refusal case");
    let timezone = std::env::var_os("TZ").expect("refusal requires a timezone value");
    match case.as_str() {
        "too_long" => assert_eq!(timezone.as_bytes(), vec![b'x'; 1025]),
        "control" => assert_eq!(timezone.as_bytes(), b"UTC\x1frefused"),
        _ => panic!("unsupported refusal case"),
    }
    let mut fixture = fixture();
    fixture.review(&["status"]);
    fixture.host.enable(CLOCK);
    assert!(matches!(
        &fixture.host.listing()[0].1,
        PluginState::Unavailable { reason }
            if reason == "managed TZ exceeds 1024 bytes or contains ASCII control bytes"
    ));
    assert!(fixture.host.desired_enabled(CLOCK));
    assert_eq!(fixture.host.supervised_count(), 0);
    assert!(fixture.host.plugins[0].io.is_none());
    assert!(fixture.host.plugins[0].observed.is_none());
    assert!(fixture.host.running_generation(CLOCK).is_none());
    assert!(fixture
        .host
        .request_status(CLOCK, "app_footer", &Context::default())
        .is_none());
    fixture.host.poll();
    assert_eq!(fixture.host.supervised_count(), 0);
    assert!(fixture.host.results.is_empty());
    println!("clock_case={case} refused_before_spawn=true identity={CLOCK}");
    fixture.stop();
}
