//! real system-status output through the supervised host, not a graphical or
//! authenticated installation claim; the shared fixture receipt stays synthetic.

use super::*;

const RESOURCE: &str = "github:1431055733/system-status";
const LABELS: [&str; 3] = ["CPU", "MEM", "DISK"];
const TIPS: [&str; 3] = [
    "Aggregate CPU busy time; idle and I/O wait excluded",
    "Used memory based on MemAvailable",
    "Root filesystem /; df-style used space excluding reserved availability",
];

fn fixture() -> Fixture {
    let fixture = Fixture::with_binary(
        "system-status",
        &["status"],
        &[],
        "AMONG_MANAGED_SYSTEM_BINARY",
        "AMONG_MANAGED_SYSTEM_SHA256",
    );
    assert_eq!(fixture.key, RESOURCE);
    fixture
}

fn publish(fixture: &mut Fixture, line: &str, initial: bool) -> [Option<u8>; 3] {
    fixture.host.handle_managed_line(0, line);
    let results = fixture.host.drain_results();
    assert_eq!(results.len(), 1);
    let result = &results[0];
    assert_eq!(result.plugin, RESOURCE);
    assert_eq!(result.scope, "app_header");
    assert_eq!(result.refresh, Refresh::IntervalMs(1000));
    assert_eq!(result.fragments.len(), 3);
    let values = std::array::from_fn(|index| {
        let fragment = &result.fragments[index];
        assert_eq!(fragment.priority, 10 + index as i32);
        assert_eq!(fragment.tooltip.as_deref(), Some(TIPS[index]));
        let text = fragment.text.as_deref().unwrap();
        if initial && index == 0 {
            assert_eq!(text, "CPU N/A");
            assert!(fragment.meter.is_none());
            return None;
        }
        let value = text
            .strip_prefix(&format!("{} ", LABELS[index]))
            .unwrap()
            .strip_suffix('%')
            .unwrap()
            .parse::<u8>()
            .unwrap();
        assert!(value <= 100);
        assert_eq!(fragment.meter, Some(f64::from(value) / 100.0));
        Some(value)
    });
    println!(
        "host_resource_header initial={initial} percent={values:?} meter_text_equal=true refresh_ms=1000 binary_sha256={}",
        std::env::var("AMONG_MANAGED_SYSTEM_SHA256").unwrap()
    );
    values
}

#[test]
fn real_resources_require_review_and_publish_measured_header_meters() {
    let mut fixture = fixture();
    fixture.host.enable(RESOURCE);
    assert!(matches!(
        fixture.host.listing()[0].1,
        PluginState::Unavailable { .. }
    ));
    assert_eq!(fixture.host.supervised_count(), 0);
    assert!(fixture.host.running_generation(RESOURCE).is_none());
    fixture.host.disable(RESOURCE);
    fixture.enable(&["status"]);
    assert_eq!(
        fixture.host.plugins[0].runtime.as_ref().unwrap().grants,
        BTreeSet::from(["status".to_string()])
    );
    assert_eq!(
        fixture.host.configuration(RESOURCE),
        Configuration::NotApplied
    );
    for scope in ["pane_footer:1", "workspace_summary:0"] {
        assert!(fixture.host.world.scope(scope).is_some());
        assert!(fixture
            .host
            .request_status(RESOURCE, scope, &Context::default())
            .is_none());
    }
    let generation = fixture.host.running_generation(RESOURCE).unwrap();
    let (_, initial) = fixture.capture("app_header");
    let first = publish(&mut fixture, &initial, true);
    assert!(first[1].is_some() && first[2].is_some());
    std::thread::sleep(Duration::from_millis(1100));
    let (_, measured) = fixture.capture("app_header");
    let values = publish(&mut fixture, &measured, false);
    assert!(values.iter().all(Option::is_some));
    assert_eq!(fixture.host.running_generation(RESOURCE), Some(generation));
    let (_, footer) = fixture.capture("app_footer");
    fixture.host.handle_managed_line(0, &footer);
    let results = fixture.host.drain_results();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].plugin, RESOURCE);
    assert_eq!(results[0].scope, "app_footer");
    assert_eq!(results[0].refresh, Refresh::IntervalMs(1000));
    assert!(results[0].fragments.is_empty());
    fixture.stop();
}

#[test]
fn real_resources_retired_reply_cannot_publish_into_reenabled_header() {
    let mut fixture = fixture();
    fixture.enable(&["status"]);
    let (_, old) = fixture.capture("app_header");
    let generation = fixture.host.running_generation(RESOURCE).unwrap();
    fixture.stop();
    fixture.host.enable(RESOURCE);
    fixture.wait(|host| matches!(host.listing()[0].1, PluginState::Running));
    assert_ne!(
        fixture.host.running_generation(RESOURCE).unwrap(),
        generation
    );
    let (current_id, current) = fixture.capture("app_header");
    fixture.host.handle_managed_line(0, &old);
    assert!(fixture.host.results.is_empty());
    assert!(fixture.host.plugins[0].inflight.contains_key(&current_id));
    // the genuine retired frame has an old request ID too. isolate generation
    // authority with a current-credit response before accepting the real frame.
    let mut stale: Value = serde_json::from_str(&current).unwrap();
    stale["generation"] = json!(generation);
    fixture.host.handle_managed_line(0, &stale.to_string());
    assert!(fixture.host.results.is_empty());
    assert!(fixture.host.plugins[0].inflight.contains_key(&current_id));
    publish(&mut fixture, &current, true);
    fixture.host.handle_managed_line(0, &current);
    assert!(fixture.host.results.is_empty());
    fixture.stop();
}
