use super::*;
use crate::plugin::host::Configuration;
use serde_json::{json, Value};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

const PROVIDER: &str = "github:12/sample";
const PRODUCER: &str = "github:13/sample";
const PHASE: &str = "AMONG_COMBINED_PHASE";
const NAME: &str =
    "app::notification_ui_tests::combined::every_channel_keeps_its_own_policy_failure_and_restart";

fn frames(package: &Path, kind: &str) -> Vec<Value> {
    std::fs::read_to_string(package.join("received.jsonl"))
        .unwrap_or_default()
        .lines()
        // The plugin may still be writing the last line.
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter(|frame| frame["type"] == kind)
        .collect()
}

fn package_of(root: &Path, key: &str) -> PathBuf {
    crate::plugin::inventory::read(root)
        .unwrap()
        .into_iter()
        .find(|entry| entry.key() == key)
        .unwrap()
        .receipt
        .package_path(root)
}

fn install(root: &Path) -> (PathBuf, PathBuf) {
    let mut provider = crate::plugin::inventory::tests::fixture(root);
    let provider_package = provider.package_path(root);
    let binary = std::env::current_exe()
        .unwrap()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("examples/notification-plugin");
    std::fs::copy(&binary, provider_package.join("plugin")).unwrap();
    std::fs::set_permissions(
        provider_package.join("plugin"),
        std::fs::Permissions::from_mode(0o700),
    )
    .unwrap();
    let path = provider_package.join("among-plugin.json");
    let mut manifest: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    manifest["required_features"] = json!(["status", "settings"]);
    manifest["optional_features"] = json!(["notifications.deliver"]);
    manifest["delivery_channels"] =
        json!([{"id":"first","name":"Alerts","description":"Delivered alerts"}]);
    let bytes = manifest.to_string();
    std::fs::write(&path, &bytes).unwrap();
    provider.manifest_sha256 = crate::plugin::inventory::checksum(bytes.as_bytes()).unwrap();

    let mut producer = provider.clone();
    crate::plugin::inventory::tests::release_of(&mut producer).repository_id = 13;
    let producer_package = producer.package_path(root);
    std::fs::create_dir_all(&producer_package).unwrap();
    let mut parent = producer_package.as_path();
    while parent.starts_with(root) {
        std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700)).unwrap();
        if parent == root {
            break;
        }
        parent = parent.parent().unwrap();
    }
    std::fs::copy(
        provider_package.join("plugin"),
        producer_package.join("plugin"),
    )
    .unwrap();
    manifest["optional_features"] = json!(["notifications.produce"]);
    manifest
        .as_object_mut()
        .unwrap()
        .remove("delivery_channels");
    let bytes = manifest.to_string();
    std::fs::write(producer_package.join("among-plugin.json"), &bytes).unwrap();
    producer.manifest_sha256 = crate::plugin::inventory::checksum(bytes.as_bytes()).unwrap();
    std::fs::write(
        root.join("installed.json"),
        json!({"version":1,"installations":[provider,producer]}).to_string(),
    )
    .unwrap();
    (provider_package, producer_package)
}

fn enable(ctx: &Window, prefs: &adw::PreferencesWindow, identity: &str, features: Value) {
    let review = json!({"owner":"example","repository":"sample","features":features});
    ctx.state
        .borrow_mut()
        .plugins
        .review_access(identity, review.clone())
        .unwrap();
    let page = descendants(prefs.upcast_ref())
        .into_iter()
        .filter_map(|widget| widget.downcast::<adw::PreferencesPage>().ok())
        .find(|page| page.name().as_deref() == Some("plugins"))
        .unwrap();
    prefs.set_visible_page(&page);
    let row = descendants(page.upcast_ref())
        .into_iter()
        .filter_map(|widget| widget.downcast::<adw::ActionRow>().ok())
        .find(|row| row.subtitle().is_some_and(|text| text.contains(identity)))
        .unwrap();
    let switch = descendants(row.upcast_ref())
        .into_iter()
        .find_map(|widget| widget.downcast::<gtk::Switch>().ok())
        .unwrap();
    assert!(!switch.is_active() && switch.grab_focus());
    xdo(&["key", "--clearmodifiers", "space"]);
    wait(|| switch.is_active(), "native managed enable");
    wait(
        || ctx.state.borrow().plugins.configuration(identity) == Configuration::Applied,
        "managed admission",
    );
    ctx.state
        .borrow_mut()
        .settings
        .managed_review_for_installation(identity, review, None)
        .unwrap();
}

fn always(prefs: &adw::PreferencesWindow, identity: &str) {
    let condition: adw::ComboRow = named(prefs, &format!("notification-{identity}-condition"));
    assert!(condition.grab_focus());
    xdo(&["key", "--clearmodifiers", "Return"]);
    pump_for(Duration::from_millis(100));
    xdo(&["key", "--clearmodifiers", "Home", "Return"]);
    pump_for(Duration::from_millis(200));
    assert_eq!(condition.selected(), 0, "{identity} keyboard condition");
}

fn restored() {
    let root = crate::plugin::inventory::root().unwrap();
    let package = package_of(&root, PROVIDER);
    let before = frames(&package, "deliver").len();
    let general = crate::settings::directory().unwrap().join("general.json");
    let bytes = std::fs::read(&general).unwrap();
    let (_app, ctx) = app();
    wait(
        || ctx.state.borrow().plugins.configuration(PROVIDER) == Configuration::Applied,
        "fresh process retained the provider",
    );
    let backend = ctx.state.borrow().bells.clone().unwrap();
    let prefs = preferences(&ctx);
    let row = |identity: &str| {
        named::<adw::SwitchRow>(&prefs, &format!("notification-{identity}-enabled")).is_active()
    };
    assert!(!row("visual"), "visual policy lost in restart");
    assert!(row("audio"), "audio policy lost in restart");
    assert!(row("command"), "command policy lost in restart");
    assert!(
        !row(&format!("provider:{PROVIDER}:first")),
        "provider channel was turned off before restart"
    );
    pump_for(Duration::from_millis(500));
    assert_eq!(
        std::fs::read(&general).unwrap(),
        bytes,
        "view saved General"
    );
    assert_eq!(
        frames(&package, "deliver").len(),
        before,
        "fresh process replayed a delivery"
    );
    assert_eq!(
        backend.event_count(),
        0,
        "fresh process replayed an old event"
    );
    prefs.destroy();
    ctx.window.destroy();
    println!("PASS fresh process restores every non-default channel policy, saves nothing and replays no delivery or event");
}

/// Runs visual, audio, command and provider channels in one application with
/// non-default policies, because each is otherwise proven alone and an
/// interaction between them would pass every separate test. The test binary
/// cannot host the decoder helper, so its audio channel makes no sound here;
/// `check-general-audio` judges real output from the executable.
#[test]
#[ignore = "requires owned X11/WM/private XDG and the prebuilt notification-plugin"]
fn every_channel_keeps_its_own_policy_failure_and_restart() {
    if std::env::var_os(PHASE).is_some() {
        restored();
        return;
    }
    let root = crate::plugin::inventory::root().unwrap();
    let (provider, producer) = install(&root);
    let (_app, ctx) = app();
    split_focused(&ctx);
    pump_for(Duration::from_millis(200));
    let backend = ctx.state.borrow().bells.clone().unwrap();
    let first = PaneId(1);
    let second = ctx.state.borrow().workspaces.active().focused;
    let prefs = preferences(&ctx);
    let visual: adw::SwitchRow = named(&prefs, "notification-visual-enabled");
    toggle(&visual);
    assert!(!visual.is_active());
    always(&prefs, "audio");
    let audio: adw::SwitchRow = named(&prefs, "notification-audio-enabled");
    toggle(&audio);
    assert!(audio.is_active());
    let program: adw::EntryRow = named(&prefs, "notification-command-program");
    type_entry(&program, "/usr/bin/false");
    key_button(&named(&prefs, "notification-command-save"));
    always(&prefs, "command");
    let command: adw::SwitchRow = named(&prefs, "notification-command-enabled");
    toggle(&command);
    assert!(command.is_active());

    enable(
        &ctx,
        &prefs,
        PROVIDER,
        json!(["status", "settings", "notifications.deliver"]),
    );
    enable(
        &ctx,
        &prefs,
        PRODUCER,
        json!(["status", "settings", "notifications.produce"]),
    );
    let general = descendants(prefs.upcast_ref())
        .into_iter()
        .filter_map(|widget| widget.downcast::<adw::PreferencesPage>().ok())
        .find(|page| page.name().as_deref() == Some("notifications"))
        .unwrap();
    prefs.set_visible_page(&general);
    pump_for(Duration::from_millis(200));
    let channel = format!("provider:{PROVIDER}:first");
    always(&prefs, &channel);
    let delivery: adw::SwitchRow = named(&prefs, &format!("notification-{channel}-enabled"));
    toggle(&delivery);
    assert!(delivery.is_active());
    let completions = || {
        backend
            .with_host(|host| host.command_completions().count())
            .unwrap()
    };

    // A bell reaches the provider and the failing command, and leaves no mark.
    real_bell(&ctx, &backend, first);
    wait(
        || frames(&provider, "deliver").len() == 1,
        "bell to the provider beside a failing command and audio channel",
    );
    wait(|| completions() == 1, "failing command completed");
    assert!(backend.command_status().contains("Exited(1)"));
    let source = backend
        .with_host(|host| host.source(first.0))
        .unwrap()
        .unwrap();
    assert!(
        !backend
            .with_host(|host| host.attention(source, std::time::Instant::now()).unread)
            .unwrap(),
        "disabled visual channel marked the terminal"
    );

    // A targetless event from the other plugin takes the same route.
    let pending = producer.join("pending.json");
    std::fs::write(
        &pending,
        json!({"type":"notification_event","session":"$session","generation":"$generation",
            "event":{"sequence":1,"target":{"kind":"application"},"source_generation":null,
            "message":"targetless"}})
        .to_string(),
    )
    .unwrap();
    std::fs::rename(&pending, producer.join("input-1.json")).unwrap();
    wait(
        || frames(&provider, "deliver").len() == 2,
        "targetless event to the provider",
    );
    let targetless = frames(&provider, "deliver").remove(1);
    assert!(targetless["delivery"]["event"]["source"].is_null());
    wait(|| completions() == 2, "targetless event to the command");

    // A flood stays bounded for every channel and leaves the sibling terminal
    // responsive.
    terminal(&ctx, first).feed_child(b"for i in $(seq 300); do printf '\\007'; done\n");
    pump_for(Duration::from_millis(1500));
    terminal(&ctx, second).feed_child(b"printf '\\033]0;combined-alive\\007'\n");
    wait(
        || terminal(&ctx, second).window_title().as_deref() == Some("combined-alive"),
        "sibling terminal during the flood",
    );
    let flooded = frames(&provider, "deliver").len() - 2;
    assert!(
        (1..=3).contains(&flooded),
        "{flooded} deliveries from 300 bells"
    );
    assert!(completions() - 2 <= 3, "command attempts from 300 bells");

    // Turning one channel off leaves the others running.
    toggle(&delivery);
    pump_for(Duration::from_millis(2200));
    let (delivered, ran) = (frames(&provider, "deliver").len(), completions());
    real_bell(&ctx, &backend, first);
    wait(
        || completions() == ran + 1,
        "command after provider disable",
    );
    pump_for(Duration::from_millis(300));
    assert_eq!(frames(&provider, "deliver").len(), delivered);

    prefs.destroy();
    ctx.window.destroy();
    wait(
        || ctx.state.borrow().plugins.supervised_count() == 0 && backend.is_stopped(),
        "retirement before the fresh process",
    );
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            NAME,
            "--exact",
            "--ignored",
            "--test-threads=1",
            "--nocapture",
        ])
        .env(PHASE, "restore")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "fresh phase failed: {}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    println!("PASS visual, audio, command and provider channels ran together under non-default keyboard-set policies: a failing command and a soundless audio channel left provider delivery intact, a disabled visual channel left no mark, a targetless event reached provider and command, 300 flooded bells stayed bounded with a live sibling terminal, one disabled channel left the others running, and a fresh process restored policies with no replay");
}
