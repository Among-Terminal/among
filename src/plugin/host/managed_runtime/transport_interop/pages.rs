//! the external scalar reducer is a labelled sdk fixture, not another plugin.
//! real child enumerate frames enter the production host page sender unchanged.

use super::*;

const PAGES: &str = "github:1431055733/sdk-pages-fixture";
const GRANTS: &[&str] = &["status", "context.terminals", "context.cwd"];

fn fixture() -> Fixture {
    let mut fixture = Fixture::with_binary(
        "sdk-pages-fixture",
        GRANTS,
        &[],
        "AMONG_MANAGED_PAGES_FIXTURE",
        "AMONG_MANAGED_PAGES_SHA256",
    );
    assert_eq!(fixture.key, PAGES);
    fixture.enable(GRANTS);
    fixture
}

fn frame(fixture: &Fixture) -> (String, Value) {
    let io = fixture.host.plugins[0].io.as_ref().unwrap();
    let line = io
        .process
        .frames
        .recv_timeout(Duration::from_secs(3))
        .expect("real sdk child stdout frame");
    let value: Value = serde_json::from_str(&line).unwrap();
    assert_eq!(value["session"], fixture.host.world.session);
    assert_eq!(value["generation"], io.generation);
    (line, value)
}

fn request(fixture: &mut Fixture, scope: &str) -> (u64, managed::Scope) {
    let target = fixture.host.world.scope(scope).unwrap();
    let id = fixture.host.request_status(PAGES, scope).unwrap();
    assert!(fixture.host.plugins[0].inflight.contains_key(&id));
    (id, target)
}

fn enumeration(
    fixture: &Fixture,
    target: &managed::Scope,
    expected_id: u64,
    cursor: u64,
) -> String {
    let (line, value) = frame(fixture);
    assert_eq!(value["type"], "enumerate");
    assert_eq!(value["request_id"], expected_id);
    assert_eq!(value["scope"], serde_json::to_value(target).unwrap());
    assert_eq!(value["cursor"], cursor.to_string());
    line
}

fn count(fixture: &mut Fixture, scope: &str, id: u64, expected: usize) {
    let (line, value) = frame(fixture);
    assert_eq!(value["type"], "status");
    assert_eq!(value["request_id"], id);
    assert_eq!(value["fragments"][0]["text"], expected.to_string());
    fixture.accept(&line, scope, &expected.to_string());
    assert!(fixture.host.plugins[0].inflight.is_empty());
    assert!(fixture.host.plugins[0]
        .runtime
        .as_ref()
        .unwrap()
        .enumeration
        .is_none());
}

fn terminals(
    count: u64,
    workspace: usize,
    start: u64,
    cwd: &str,
) -> Vec<crate::plugin::TerminalInfo> {
    (start..start + count)
        .map(|pane| crate::plugin::TerminalInfo {
            pane,
            workspace,
            cwd: Some(cwd.into()),
            title: Some("not granted".into()),
            running: true,
        })
        .collect()
}

#[test]
fn real_sdk_application_count_paging_interleaves_a_live_pane_through_the_host() {
    let mut fixture = fixture();
    let mut world = context();
    world.terminals = terminals(33, 0, 1, "/данные/日本語");
    fixture.host.update_context(vec![10, 20], world);
    let (id, target) = request(&mut fixture, "app_header");
    assert_eq!(id, 1);
    let initial = fixture
        .host
        .world
        .context(
            &target,
            &fixture.host.plugins[0].runtime.as_ref().unwrap().grants,
        )
        .unwrap();
    assert_eq!(initial["terminals"].as_array().unwrap().len(), 32);
    let enumeration = enumeration(&fixture, &target, 1, 32);
    assert!(fixture.host.results.is_empty());
    assert_eq!(fixture.host.plugins[0].next_request_id, 2);
    let (pane_id, pane) = fixture.capture("pane_footer:33");
    assert_eq!(pane_id, 2);
    fixture.accept(&pane, "pane_footer:33", "/данные/日本語");
    assert!(fixture.host.plugins[0].inflight.contains_key(&id));
    fixture.host.handle_managed_line(0, &enumeration);
    count(&mut fixture, "app_header", id, 33);
    assert_eq!(fixture.host.plugins[0].next_request_id, 3);
    println!("host_sdk application_pages=32+1 reduced_count=33 real_enumeration=true interleaved_pane=true separate_status_ids=true");
    fixture.stop();
}

#[test]
fn real_sdk_hidden_workspace_byte_paging_uses_all_pages_without_sibling_records() {
    let mut fixture = fixture();
    let path = format!("/{}", "界".repeat(1333));
    assert_eq!(path.len(), 4000);
    let mut world = context();
    world.terminals[1].workspace = 0;
    world.terminals.extend(terminals(17, 1, 100, &path));
    fixture.host.update_context(vec![10, 20], world);
    let (id, target) = request(&mut fixture, "workspace_summary:1");
    assert_eq!(target.target, managed::Target::Workspace { reference: 20 });
    let grants = fixture.host.plugins[0]
        .runtime
        .as_ref()
        .unwrap()
        .grants
        .clone();
    let mut projected = fixture.host.world.context(&target, &grants).unwrap();
    let mut sizes = Vec::new();
    let mut total = 0;
    let mut enumeration_id = 1;
    loop {
        let rows = projected["terminals"].as_array().unwrap();
        assert!(!rows.is_empty() && rows.len() < 32);
        assert!(serde_json::to_vec(rows).unwrap().len() <= 28 * 1024);
        assert!(rows.iter().all(|row| row["workspace"] == "20"
            && row["cwd"] == path
            && row.get("title").is_none()
            && row.get("running").is_none()));
        sizes.push(rows.len());
        total += rows.len();
        let Some(cursor) = projected["next_cursor"].as_str() else {
            break;
        };
        let cursor = managed::parse_reference(cursor).unwrap();
        let line = enumeration(&fixture, &target, enumeration_id, cursor);
        assert!(fixture.host.results.is_empty());
        projected = fixture
            .host
            .world
            .page(&target, &grants, Some(cursor))
            .unwrap();
        fixture.host.handle_managed_line(0, &line);
        enumeration_id += 1;
    }
    assert_eq!(sizes, [7, 7, 3]);
    assert_eq!(total, 17);
    count(&mut fixture, "workspace_summary:1", id, 17);
    println!("host_sdk hidden_workspace_pages=7+7+3 byte_ceiling=28672 reduced_count=17 sibling_records_excluded=true real_enumeration=true");
    fixture.stop();
}

#[test]
fn real_sdk_closed_workspace_continuation_fails_without_publishing_empty_success() {
    let mut fixture = fixture();
    let mut world = context();
    world.terminals.extend(terminals(33, 1, 100, "/hidden"));
    fixture.host.update_context(vec![10, 20], world);
    let started = Instant::now();
    let (id, target) = request(&mut fixture, "workspace_summary:1");
    let line = enumeration(&fixture, &target, 1, 130);
    let mut closed = context();
    closed.workspace_count = 1;
    closed.terminals.truncate(1);
    fixture.host.update_context(vec![10], closed);
    fixture.host.handle_managed_line(0, &line);
    assert!(fixture.host.plugins[0]
        .runtime
        .as_ref()
        .unwrap()
        .enumeration
        .is_none());
    assert!(fixture.host.plugins[0].inflight.contains_key(&id));
    let deadline = started + Duration::from_secs(6);
    loop {
        fixture.host.poll();
        assert!(
            fixture.host.results.is_empty(),
            "closure cannot publish empty-success data"
        );
        let plugin = &fixture.host.plugins[0];
        if plugin.after_stop.is_some() || matches!(plugin.state, PluginState::Faulted { .. }) {
            assert!(
                plugin.restart_at.is_some(),
                "ordinary host crash/backoff policy remains active"
            );
            assert_eq!(plugin.restart_attempts, 0);
            break;
        }
        assert!(
            Instant::now() < deadline,
            "closed continuation must terminate"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(started.elapsed() >= Duration::from_millis(3500));
    assert!(
        started.elapsed() < Duration::from_millis(5000),
        "client budget, not the five-second host deadline"
    );
    assert!(fixture.host.desired_enabled(PAGES));
    println!("host_sdk closed_workspace_fault_elapsed_ms={} no_empty_success=true host_backoff_retained=true", started.elapsed().as_millis());
    fixture.stop();
}

#[test]
fn real_sdk_retired_enumeration_cannot_attach_to_a_new_host_generation() {
    let mut fixture = fixture();
    let mut world = context();
    world.terminals = terminals(33, 0, 1, "/current");
    fixture.host.update_context(vec![10, 20], world);
    let (_, target) = request(&mut fixture, "app_header");
    let retired = enumeration(&fixture, &target, 1, 32);
    let generation = fixture.host.running_generation(PAGES).unwrap();
    fixture.stop();
    fixture.host.enable(PAGES);
    fixture.wait(|host| matches!(host.listing()[0].1, PluginState::Running));
    assert_ne!(fixture.host.running_generation(PAGES).unwrap(), generation);
    let (id, target) = request(&mut fixture, "app_header");
    let current = enumeration(&fixture, &target, 1, 32);
    fixture.host.handle_managed_line(0, &retired);
    assert!(fixture.host.results.is_empty());
    assert!(fixture.host.plugins[0].inflight.contains_key(&id));
    assert!(fixture.host.plugins[0]
        .runtime
        .as_ref()
        .unwrap()
        .enumeration
        .is_none());
    fixture.host.handle_managed_line(0, &current);
    count(&mut fixture, "app_header", id, 33);
    println!("host_sdk retired_enumeration_refused=true current_generation_count=33 real_enumeration=true");
    fixture.stop();
}
