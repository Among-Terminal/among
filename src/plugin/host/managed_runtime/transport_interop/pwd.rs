//! production pane-pwd through inventory, review and the real supervised host.
//! model focus/visibility here is not mapped gtk or authenticated installation.

use super::*;

const PWD: &str = "github:1431055733/pane-pwd";
const GRANTS: &[&str] = &["status", "context.terminals", "context.cwd"];

fn fixture() -> Fixture {
    let fixture = Fixture::with_binary(
        "pane-pwd",
        GRANTS,
        &[],
        "AMONG_MANAGED_PWD_BINARY",
        "AMONG_MANAGED_PWD_SHA256",
    );
    assert_eq!(fixture.key, PWD);
    fixture
}

fn publish(fixture: &mut Fixture, line: &str, scope: &str, path: Option<&str>) {
    fixture.host.handle_managed_line(0, line);
    let results = fixture.host.drain_results();
    assert_eq!(results.len(), 1);
    let result = &results[0];
    assert_eq!(result.plugin, PWD);
    assert_eq!(result.scope, scope);
    assert_eq!(result.refresh, Refresh::IntervalMs(1000));
    if let Some(path) = path {
        assert_eq!(result.fragments.len(), 1);
        let fragment = &result.fragments[0];
        assert_eq!(
            fragment.text.as_deref(),
            Some(path.chars().take(480).collect::<String>().as_str())
        );
        assert_eq!(fragment.tooltip.as_deref(), Some(path));
        assert_eq!(fragment.priority, 10);
        assert!(fragment.meter.is_none());
    } else {
        assert!(
            result.fragments.is_empty(),
            "unknown is not a process/sibling cwd fallback"
        );
    }
}

#[test]
fn real_pwd_requires_all_grants_and_keeps_focused_hidden_unicode_paths_separate() {
    let mut fixture = fixture();
    for grants in [
        &["status"][..],
        &["status", "context.terminals"][..],
        &["status", "context.cwd"][..],
    ] {
        assert!(fixture
            .host
            .review_access(
                PWD,
                json!({"owner":"Among-Terminal","repository":"among-plugins","features":grants})
            )
            .is_err());
        assert_eq!(fixture.host.supervised_count(), 0);
    }
    fixture.host.enable(PWD);
    assert!(matches!(
        fixture.host.listing()[0].1,
        PluginState::Unavailable { .. }
    ));
    assert_eq!(fixture.host.supervised_count(), 0);
    fixture.host.disable(PWD);
    fixture.enable(GRANTS);
    assert_eq!(
        fixture.host.plugins[0].runtime.as_ref().unwrap().grants,
        GRANTS
            .iter()
            .map(|value| value.to_string())
            .collect::<BTreeSet<_>>()
    );
    let generation = fixture.host.running_generation(PWD).unwrap();
    let long = format!("/данные/{}", "日本語🙂".repeat(180));
    assert!(long.len() < 4096 && long.chars().count() > 480);
    let mut world = context();
    world.terminals[0].cwd = Some(long.clone());
    fixture.host.update_context(vec![10, 20], world.clone());
    let (_, focused) = fixture.capture("pane_footer:1");
    publish(&mut fixture, &focused, "pane_footer:1", Some(&long));
    let (_, hidden) = fixture.capture("pane_footer:2");
    publish(
        &mut fixture,
        &hidden,
        "pane_footer:2",
        Some("/sibling-private"),
    );
    world.active_workspace = 1;
    world.focused_pane = Some(2);
    fixture.host.update_context(vec![10, 20], world);
    let (_, now_hidden) = fixture.capture("pane_footer:1");
    publish(&mut fixture, &now_hidden, "pane_footer:1", Some(&long));
    for scope in [
        "app_header",
        "app_footer",
        "workspace_summary:0",
        "pane_header:1",
    ] {
        let (_, line) = fixture.capture(scope);
        publish(&mut fixture, &line, scope, None);
    }
    assert_eq!(fixture.host.running_generation(PWD), Some(generation));
    println!("host_pwd focused_hidden_separate=true unicode_chars={} displayed_chars=480 full_tooltip=true focus_switch_no_retarget=true unrelated_empty=true", long.chars().count());
    fixture.stop();
}

#[test]
fn real_pwd_unknown_empty_and_host_unavailable_cwd_never_fall_back() {
    let mut fixture = fixture();
    fixture.enable(GRANTS);
    for (label, path, unavailable) in [
        ("unknown", None, false),
        ("empty", Some(String::new()), false),
        ("byte_budget", Some("x".repeat(4097)), true),
        ("encoded_budget", Some("\u{1}".repeat(2000)), true),
    ] {
        let mut world = context();
        world.terminals[0].cwd = path;
        fixture.host.update_context(vec![10, 20], world);
        let target = fixture.host.world.scope("pane_footer:1").unwrap();
        let projected = fixture
            .host
            .world
            .context(
                &target,
                &fixture.host.plugins[0].runtime.as_ref().unwrap().grants,
            )
            .unwrap();
        assert_eq!(projected["terminals"].as_array().unwrap().len(), 1);
        assert_eq!(
            projected["terminals"][0]["unavailable"] == json!(["cwd"]),
            unavailable
        );
        let (_, line) = fixture.capture("pane_footer:1");
        publish(&mut fixture, &line, "pane_footer:1", None);
        assert!(!line.contains("sibling-private"));
        println!("host_pwd cwd_case={label} empty_without_fallback=true unavailable={unavailable}");
    }
    assert!(fixture
        .host
        .request_status(PWD, "pane_footer:999")
        .is_none());
    fixture.stop();
}

#[test]
fn real_pwd_closed_and_retired_replies_cannot_overwrite_live_panes() {
    let mut fixture = fixture();
    fixture.enable(GRANTS);
    let (id, closed_reply) = fixture.capture("pane_footer:1");
    let mut world = context();
    world.terminals.remove(0);
    world.focused_pane = Some(2);
    fixture.host.update_context(vec![10, 20], world);
    assert!(fixture.host.request_status(PWD, "pane_footer:1").is_none());
    assert!(fixture.host.plugins[0].inflight.contains_key(&id));
    fixture.host.handle_managed_line(0, &closed_reply);
    assert!(!fixture.host.plugins[0].inflight.contains_key(&id));
    assert!(fixture.host.drain_results().is_empty());
    let (_, retired) = fixture.capture("pane_footer:2");
    let generation = fixture.host.running_generation(PWD).unwrap();
    fixture.stop();
    fixture.host.enable(PWD);
    fixture.wait(|host| matches!(host.listing()[0].1, PluginState::Running));
    assert_ne!(fixture.host.running_generation(PWD).unwrap(), generation);
    let (current_id, current) = fixture.capture("pane_footer:2");
    fixture.host.handle_managed_line(0, &retired);
    assert!(fixture.host.results.is_empty());
    assert!(fixture.host.plugins[0].inflight.contains_key(&current_id));
    let mut isolated: Value = serde_json::from_str(&current).unwrap();
    isolated["generation"] = json!(generation);
    fixture.host.handle_managed_line(0, &isolated.to_string());
    assert!(fixture.host.results.is_empty());
    assert!(fixture.host.plugins[0].inflight.contains_key(&current_id));
    publish(
        &mut fixture,
        &current,
        "pane_footer:2",
        Some("/sibling-private"),
    );
    fixture.host.handle_managed_line(0, &current);
    assert!(fixture.host.drain_results().is_empty());
    println!("host_pwd closed_credit_reaped=true retired_and_isolated_generation_refused=true current_pane_publishes=true");
    fixture.stop();
}
