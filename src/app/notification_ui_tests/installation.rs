use super::*;
use crate::plugin::host::{Configuration, PluginState};
use crate::plugin::install::source::tests::{install_at, package_route, Server};
use crate::plugin::install::{mutation, test_notification_package};
use crate::plugin::inventory::{self, Receipt};
use serde_json::{json, Value};
use std::os::fd::AsRawFd;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

const KEY: &str = "github:12/sample";

fn frames(package: &Path) -> Vec<Value> {
    std::fs::read_to_string(package.join("received.jsonl"))
        .unwrap_or_default()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

fn send(package: &Path, value: Value) {
    let serial =
        std::fs::read_to_string(package.join("next-command")).unwrap_or_else(|_| "1".into());
    let temporary = package.join("next-input.json");
    let path = package.join(format!("input-{}.json", serial.trim()));
    std::fs::write(&temporary, value.to_string()).unwrap();
    std::fs::set_permissions(&temporary, std::fs::Permissions::from_mode(0o600)).unwrap();
    std::fs::rename(temporary, &path).unwrap();
    wait(|| !path.exists(), "real managed stdout ingress");
    pump_for(Duration::from_millis(100));
}

fn provider_switch(prefs: &adw::PreferencesWindow) -> gtk::Switch {
    prefs.set_visible_page_name("plugins");
    pump_for(Duration::from_millis(150));
    let row = descendants(prefs.upcast_ref())
        .into_iter()
        .filter_map(|widget| widget.downcast::<adw::ActionRow>().ok())
        .find(|row| row.subtitle().is_some_and(|text| text.contains(KEY)))
        .unwrap();
    descendants(row.upcast_ref())
        .into_iter()
        .find_map(|widget| widget.downcast::<gtk::Switch>().ok())
        .unwrap()
}

fn native_toggle(prefs: &adw::PreferencesWindow, enabled: bool) {
    activate(prefs);
    let switch = provider_switch(prefs);
    assert_ne!(switch.is_active(), enabled);
    assert!(switch.grab_focus());
    xdo(&["key", "--clearmodifiers", "space"]);
    wait(
        || switch.is_active() == enabled,
        "native lifecycle enable/disable",
    );
}

fn action(ctx: &Window, package: &Path) -> gtk::Button {
    wait(
        || ctx.state.borrow().plugins.configuration(KEY) == Configuration::Applied,
        "actual settings acknowledgement before publishing fresh content",
    );
    send(
        package,
        json!({"type":"notification_contribution","session":"$session","generation":"$generation",
        "contribution":{"id":"lifecycle","revision":1,"scope":{"slot":"app_header","target":{"kind":"application"}},"text":"Lifecycle authority","actions":[{"id":"ack","label":"Lifecycle acknowledge"}]}}),
    );
    wait(
        || {
            descendants(ctx.window.upcast_ref()).iter().any(|widget| {
                widget.downcast_ref::<gtk::Button>().is_some_and(|button| {
                    button.label().as_deref() == Some("Lifecycle acknowledge")
                })
            })
        },
        "live declarative lifecycle action",
    );
    descendants(ctx.window.upcast_ref())
        .into_iter()
        .filter_map(|widget| widget.downcast::<gtk::Button>().ok())
        .find(|button| button.label().as_deref() == Some("Lifecycle acknowledge"))
        .unwrap()
}

fn active_delivery(ctx: &Window, package: &Path, pane: PaneId) -> Value {
    let backend = ctx.state.borrow().bells.clone().unwrap();
    let before = frames(package)
        .iter()
        .filter(|frame| frame["type"] == "deliver")
        .count();
    std::fs::write(package.join("delivery-mode"), "late").unwrap();
    std::fs::set_permissions(
        package.join("delivery-mode"),
        std::fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    real_bell(ctx, &backend, pane);
    wait(
        || {
            frames(package)
                .iter()
                .filter(|frame| frame["type"] == "deliver")
                .count()
                > before
        },
        "active provider delivery before lifecycle mutation",
    );
    assert_eq!(backend.notification_backend().borrow().active_count(), 1);
    frames(package)
        .into_iter()
        .rev()
        .find(|frame| frame["type"] == "deliver")
        .unwrap()
}

fn pending_update(
    ctx: &Window,
    root: &Path,
    version: &str,
) -> (
    Server,
    Arc<AtomicBool>,
    crate::plugin::install::source::InstallJob,
) {
    let release = Arc::new(AtomicBool::new(false));
    let (metadata, archive) = test_notification_package(version);
    let server = Server::new({
        let release = release.clone();
        move |request| {
            if request.contains("/github-production-release-asset/12/") {
                let deadline = std::time::Instant::now() + Duration::from_secs(15);
                while !release.load(Ordering::Acquire) {
                    assert!(
                        std::time::Instant::now() < deadline,
                        "held HTTPS fixture not released"
                    );
                    std::thread::sleep(Duration::from_millis(1));
                }
            }
            package_route(metadata.clone(), archive.clone(), request)
        }
    });
    let (entry, stored) = ctx.state.borrow().plugins.managed_details(KEY).unwrap();
    let selected = server
        .inspect(true)
        .unwrap()
        .select("sample", version)
        .unwrap()
        .for_update(&entry, stored.as_ref())
        .unwrap();
    let job = server.install(selected, root.to_path_buf()).unwrap();
    wait(
        || {
            let requested = server
                .requests()
                .iter()
                .any(|request| request.contains("/github-production-release-asset/12/"));
            if !requested {
                if let Some(outcome) = job.poll() {
                    panic!(
                        "update finished before held HTTPS body: {outcome:?}; requests={:?}",
                        server.requests()
                    );
                }
            }
            requested
        },
        "real pending HTTPS body",
    );
    assert!(job.poll().is_none());
    (server, release, job)
}

fn finish_update(job: &crate::plugin::install::source::InstallJob) -> Receipt {
    let result = RefCell::new(None);
    wait(
        || {
            if result.borrow().is_some() {
                return true;
            }
            if let Some(outcome) = job.poll() {
                result.replace(Some(outcome));
                true
            } else {
                false
            }
        },
        "actual package commit",
    );
    result.into_inner().unwrap().unwrap().receipt
}

#[test]
#[ignore = "requires owned native X11/WM/private XDG, real HTTPS installation and prebuilt Rust plugin/command fixtures"]
fn combined_provider_installation_lifecycle_rejects_stale_work_without_stopping_siblings() {
    let root = inventory::root().unwrap();
    let config = crate::settings::directory().unwrap();
    crate::test_isolation::assert_below_check_root(&root);
    let (metadata, archive) = test_notification_package("1.0");
    let server =
        Server::new(move |request| package_route(metadata.clone(), archive.clone(), request));
    let first = install_at(
        &server,
        server
            .inspect(true)
            .unwrap()
            .select("sample", "1.0")
            .unwrap(),
        &root,
    )
    .unwrap()
    .receipt;
    std::fs::create_dir_all(&config).unwrap();
    std::fs::set_permissions(&config, std::fs::Permissions::from_mode(0o700)).unwrap();
    let features = vec![
        "status".into(),
        "settings".into(),
        "notifications.deliver".into(),
        "notifications.ui".into(),
        "context.terminals".into(),
    ];
    let saved = json!({"version":1,"future":"preserve","plugins":{KEY:{"enabled":true,"installation":first.installation,"review":first.review(features),"settings":{"version":1,"values":{"label":"retained","opaque":"keep"}},"future":42}}});
    let document = config.join("managed-plugins.json");
    std::fs::write(&document, saved.to_string()).unwrap();
    std::fs::set_permissions(&document, std::fs::Permissions::from_mode(0o600)).unwrap();
    let (_app, ctx) = app();
    wait(
        || ctx.state.borrow().plugins.configuration(KEY) == Configuration::Applied,
        "initial ordinary provider admission",
    );
    let backend = ctx.state.borrow().bells.clone().unwrap();
    let bridge = backend.notification_backend();
    let pane = ctx.state.borrow().workspaces.active().focused;
    let pty = terminal(&ctx, pane).pty().unwrap().fd().as_raw_fd();
    split_focused(&ctx);
    let sibling_pane = ctx.state.borrow().workspaces.active().focused;
    ctx.state
        .borrow_mut()
        .plugins
        .register(crate::plugin::host::PluginSpec {
            name: "continuity".into(),
            command: vec![
                std::env::current_exe()
                    .unwrap()
                    .parent()
                    .unwrap()
                    .parent()
                    .unwrap()
                    .join("examples/fake-plugin")
                    .to_string_lossy()
                    .into_owned(),
                "echo".into(),
            ],
            enabled: true,
        });
    wait(
        || {
            ctx.state
                .borrow()
                .plugins
                .listing()
                .iter()
                .any(|(name, state)| name == "continuity" && state == &PluginState::Running)
        },
        "sibling real process",
    );
    let provider_pid: u32 = std::fs::read_to_string(first.package_path(&root).join("executed"))
        .unwrap()
        .parse()
        .unwrap();
    let sibling_pid = ctx
        .state
        .borrow()
        .plugins
        .supervised_pids()
        .into_iter()
        .find(|pid| *pid != provider_pid)
        .unwrap();
    let continuity = |phase: &str| {
        assert!(ctx
            .state
            .borrow()
            .plugins
            .supervised_pids()
            .contains(&sibling_pid));
        assert!(Path::new(&format!("/proc/{sibling_pid}")).exists());
        assert_eq!(terminal(&ctx, pane).pty().unwrap().fd().as_raw_fd(), pty);
        activate(&ctx.window);
        assert!(terminal(&ctx, sibling_pane).grab_focus());
        xdo(&[
            "type",
            "--clearmodifiers",
            "--delay",
            "0",
            &format!("printf '\\033]0;combined-{phase}\\007'"),
        ]);
        xdo(&["key", "--clearmodifiers", "Return"]);
        wait(
            || {
                terminal(&ctx, sibling_pane).window_title().as_deref()
                    == Some(&format!("combined-{phase}"))
            },
            "actual terminal keyboard continuity",
        );
    };
    let prefs = preferences(&ctx);
    let condition: adw::ComboRow = named(
        &prefs,
        "notification-provider:github:12/sample:first-condition",
    );
    condition.set_selected(0);
    toggle(&named::<adw::SwitchRow>(
        &prefs,
        "notification-provider:github:12/sample:first-enabled",
    ));
    let command_root = crate::test_isolation::check_root()
        .join(format!("combined-command-{}", std::process::id()));
    std::fs::create_dir(&command_root).unwrap();
    let fixture = std::env::current_exe()
        .unwrap()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("notification-command-fixture");
    type_entry(
        &named(&prefs, "notification-command-program"),
        fixture.to_str().unwrap(),
    );
    type_entry(
        &named(&prefs, "notification-command-arguments"),
        &json!(["hang", command_root]).to_string(),
    );
    key_button(&named(&prefs, "notification-command-save"));
    named::<adw::ComboRow>(&prefs, "notification-command-condition").set_selected(0);
    toggle(&named::<adw::SwitchRow>(
        &prefs,
        "notification-command-enabled",
    ));
    prefs.destroy();
    let old_action = action(&ctx, &first.package_path(&root));
    let old_delivery = active_delivery(&ctx, &first.package_path(&root), pane);
    wait(
        || {
            std::fs::read_dir(&command_root).unwrap().any(|entry| {
                entry
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .starts_with("started-")
            })
        },
        "configured real command while provider and HTTPS work are active",
    );
    let (_server, release, job) = pending_update(&ctx, &root, "2.0");
    continuity("pending-update");
    release.store(true, Ordering::Release);
    let second = finish_update(&job);
    installation_observer(&ctx)();
    wait(
        || {
            ctx.state.borrow().plugins.configuration(KEY) == Configuration::Applied
                && ctx
                    .state
                    .borrow()
                    .plugins
                    .managed_details(KEY)
                    .unwrap()
                    .0
                    .receipt
                    == second
        },
        "same-token update to ordinary admitted provider",
    );
    assert_eq!(first.installation, second.installation);
    assert!(!Path::new(&format!("/proc/{provider_pid}")).exists());
    assert_eq!(bridge.borrow().active_count(), 0);
    old_action.emit_clicked();
    assert!(bridge
        .borrow()
        .contributions(std::time::Instant::now())
        .is_empty());
    let withdrawn = bridge
        .borrow()
        .inventory()
        .into_iter()
        .find(|channel| channel.declaration.key.channel == "second")
        .unwrap();
    assert!(!withdrawn.installed);
    assert!(bridge
        .borrow()
        .inventory()
        .iter()
        .any(|channel| channel.declaration.key.channel == "first" && channel.policy.enabled));
    continuity("updated");
    eprintln!("OBSERVED update: old process reaped, provider job/action withdrawn, static channel removed, policy and sibling input retained");
    let accepted = json!({"version":1,"values":{"label":"session-only","opaque":"keep"}});
    ctx.state
        .borrow_mut()
        .plugins
        .stored_settings_changed(KEY, accepted.clone());
    let second_action = action(&ctx, &second.package_path(&root));
    let _delivery = active_delivery(&ctx, &second.package_path(&root), pane);
    let (_server2, release2, job2) = pending_update(&ctx, &root, "3.0");
    let prefs = preferences(&ctx);
    native_toggle(&prefs, false);
    release2.store(true, Ordering::Release);
    let third = finish_update(&job2);
    installation_observer(&ctx)();
    wait(
        || ctx.state.borrow().plugins.supervised_stopped(KEY),
        "later disable wins pending update",
    );
    assert!(!ctx.state.borrow().plugins.desired_enabled(KEY));
    assert!(!third.package_path(&root).join("executed").exists());
    assert_eq!(
        ctx.state.borrow().plugins.managed_details(KEY).unwrap().1,
        Some(accepted.clone())
    );
    second_action.emit_clicked();
    assert_eq!(bridge.borrow().active_count(), 0);
    eprintln!(
        "OBSERVED later native disable beats pending update; accepted session-only cache retained"
    );
    native_toggle(&prefs, true);
    wait(
        || ctx.state.borrow().plugins.configuration(KEY) == Configuration::Applied,
        "explicit reenable updated provider",
    );
    prefs.destroy();
    let third_action = action(&ctx, &third.package_path(&root));
    let third_delivery = active_delivery(&ctx, &third.package_path(&root), pane);
    let (_server3, release3, job3) = pending_update(&ctx, &root, "4.0");
    activate(&ctx.window);
    confirm_package_action(&ctx, third.clone(), mutation::Action::Uninstall, None);
    wait(
        || ctx.window.visible_dialog().is_some(),
        "real uninstall consent",
    );
    let dialog = ctx
        .window
        .visible_dialog()
        .unwrap()
        .downcast::<adw::AlertDialog>()
        .unwrap();
    assert_eq!(dialog.default_response().as_deref(), Some("cancel"));
    let uninstall = descendants(dialog.upcast_ref())
        .into_iter()
        .filter_map(|widget| widget.downcast::<gtk::Button>().ok())
        .find(|button| button.label().as_deref() == Some("Uninstall"))
        .unwrap();
    let response = Rc::new(RefCell::new(None));
    dialog.connect_response(None, {
        let response = response.clone();
        move |_, value| {
            response.replace(Some(value.to_string()));
        }
    });
    wait(
        || uninstall.is_mapped(),
        "actual uninstall response button mapping",
    );
    pump_for(Duration::from_millis(250));
    key_button(&uninstall);
    wait(
        || response.borrow().is_some(),
        "actual native uninstall response",
    );
    assert_eq!(response.borrow().as_deref(), Some("apply"));
    release3.store(true, Ordering::Release);
    wait(
        || inventory::read(&root).unwrap().is_empty(),
        "authoritative removal commits after actual supervised stop",
    );
    wait(
        || ctx.state.borrow().plugins.managed_details(KEY).is_none(),
        "current authoritative host observes uninstall",
    );
    let result = RefCell::new(None);
    wait(
        || {
            if result.borrow().is_some() {
                return true;
            }
            if let Some(value) = job3.poll() {
                result.replace(Some(value));
                true
            } else {
                false
            }
        },
        "superseded HTTPS job completion",
    );
    assert!(result.into_inner().unwrap().is_err());
    assert!(!bridge.borrow().registration_current(KEY));
    assert!(bridge
        .borrow()
        .inventory()
        .iter()
        .all(|channel| !channel.installed));
    third_action.emit_clicked();
    assert!(bridge
        .borrow()
        .contributions(std::time::Instant::now())
        .is_empty());
    if let Some(dialog) = ctx.window.visible_dialog() {
        dialog.close();
    }
    continuity("uninstalled");
    eprintln!("OBSERVED native uninstall: real supervised stop/store removal, transfer superseded, backend static uninstall and stale action rejection");
    let fresh = install_at(
        &server,
        server
            .inspect(true)
            .unwrap()
            .select("sample", "1.0")
            .unwrap(),
        &root,
    )
    .unwrap()
    .receipt;
    installation_observer(&ctx)();
    assert_ne!(fresh.installation, first.installation);
    assert!(!ctx.state.borrow().plugins.desired_enabled(KEY));
    assert!(!fresh.package_path(&root).join("executed").exists());
    assert_eq!(
        ctx.state
            .borrow()
            .plugins
            .managed_details(KEY)
            .unwrap()
            .1
            .unwrap()["values"]["opaque"],
        "keep"
    );
    let retained: Value = serde_json::from_slice(&std::fs::read(&document).unwrap()).unwrap();
    assert_eq!(retained["plugins"][KEY]["enabled"], false);
    assert_eq!(retained["plugins"][KEY]["future"], 42);
    let prefs = preferences(&ctx);
    native_toggle(&prefs, true);
    wait(
        || ctx.state.borrow().plugins.configuration(KEY) == Configuration::Applied,
        "actual fresh-token explicit ON positive",
    );
    let current: Value = serde_json::from_slice(&std::fs::read(&document).unwrap()).unwrap();
    assert_eq!(
        current["plugins"][KEY]["installation"],
        json!(fresh.installation)
    );
    prefs.destroy();
    for previous in [old_delivery, third_delivery] {
        send(
            &fresh.package_path(&root),
            json!({"type":"delivery_result","session":previous["session"],"generation":previous["generation"],"job_id":previous["delivery"]["job_id"],"outcome":"accepted"}),
        );
    }
    assert_eq!(bridge.borrow().active_count(), 0);
    assert!(bridge
        .borrow()
        .rejections()
        .contains(&among::plugin_notifications::Rejection::Unauthorized));
    let fresh_action = action(&ctx, &fresh.package_path(&root));
    activate(&ctx.window);
    key_button(&fresh_action);
    wait(
        || {
            frames(&fresh.package_path(&root))
                .iter()
                .any(|frame| frame["type"] == "notification_action")
        },
        "fresh action positive after explicit enable",
    );
    continuity("fresh-enabled");
    eprintln!("OBSERVED fresh token OFF/settings retained, explicit correct-token native ON, old replies rejected and fresh action delivered");
    let producer_root = crate::test_isolation::check_root()
        .join(format!("combined-producer-{}", std::process::id()));
    let mut producer = inventory::tests::fixture(&producer_root);
    let original_package = producer.package_path(&producer_root);
    producer.repository_id = 13;
    let producer_package = producer.package_path(&producer_root);
    std::fs::create_dir_all(producer_package.parent().unwrap()).unwrap();
    for directory in [
        producer_package.parent().unwrap(),
        producer_package.parent().unwrap().parent().unwrap(),
    ] {
        std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    std::fs::rename(original_package, &producer_package).unwrap();
    let binary = std::env::current_exe()
        .unwrap()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("examples/notification-plugin");
    std::fs::copy(binary, producer_package.join("plugin")).unwrap();
    std::fs::set_permissions(
        producer_package.join("plugin"),
        std::fs::Permissions::from_mode(0o700),
    )
    .unwrap();
    let mut manifest: Value =
        serde_json::from_slice(&std::fs::read(producer_package.join("among-plugin.json")).unwrap())
            .unwrap();
    manifest["required_features"] = json!(["status", "settings"]);
    manifest["optional_features"] = json!(["notifications.produce", "context.terminals"]);
    let bytes = manifest.to_string();
    std::fs::write(producer_package.join("among-plugin.json"), &bytes).unwrap();
    producer.manifest_sha256 = inventory::checksum(bytes.as_bytes()).unwrap();
    std::fs::write(
        producer_root.join("installed.json"),
        json!({"version":1,"installations":[producer]}).to_string(),
    )
    .unwrap();
    let producer_entry = inventory::read(&producer_root).unwrap().remove(0);
    ctx.state.borrow_mut().plugins.register_managed(producer_entry, Some(&json!({"enabled":true,"review":producer.review(vec!["status".into(),"settings".into(),"notifications.produce".into(),"context.terminals".into()])})));
    wait(
        || ctx.state.borrow().plugins.configuration("github:13/sample") == Configuration::Applied,
        "ordinary separate producer admission",
    );
    let prefs = preferences(&ctx);
    type_entry(
        &named(&prefs, "notification-command-arguments"),
        &json!(["record", command_root]).to_string(),
    );
    key_button(&named(&prefs, "notification-command-save"));
    prefs.destroy();
    wait(
        || among::notifications::commands::active_count() == 0,
        "previous configured command retirement before producer positive",
    );
    let source = bridge.borrow().source(pane.0).unwrap();
    send(
        &producer_package,
        json!({"type":"notification_event","session":"$session","generation":"$generation","event":{"sequence":1,"target":{"kind":"pane","reference":pane.0.to_string()},"source_generation":source.generation().to_string(),"message":"actual combined producer"}}),
    );
    wait(
        || {
            std::fs::read_dir(&command_root).unwrap().any(|entry| {
                entry
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .starts_with("payload-")
            })
        },
        "managed producer reaches actual configured command stdin",
    );
    let payload = std::fs::read_dir(&command_root)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| {
            path.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("payload-")
        })
        .unwrap();
    let payload: Value = serde_json::from_slice(&std::fs::read(payload).unwrap()).unwrap();
    assert_eq!(payload["kind"], "managed-notification");
    let prefs = preferences(&ctx);
    type_entry(
        &named(&prefs, "notification-command-arguments"),
        &json!(["hang", command_root]).to_string(),
    );
    key_button(&named(&prefs, "notification-command-save"));
    prefs.destroy();
    pump_for(among::notifications::COOLDOWN + Duration::from_millis(100));
    send(
        &producer_package,
        json!({"type":"notification_event","session":"$session","generation":"$generation","event":{"sequence":2,"target":{"kind":"pane","reference":pane.0.to_string()},"source_generation":source.generation().to_string(),"message":"revoke owned command"}}),
    );
    wait(
        || among::notifications::commands::active_count() > 0,
        "managed producer active command",
    );
    ctx.state.borrow_mut().plugins.disable("github:13/sample");
    wait(
        || {
            among::notifications::commands::active_count() == 0
                && ctx
                    .state
                    .borrow()
                    .plugins
                    .supervised_stopped("github:13/sample")
        },
        "actual origin disable retires managed command authority and process",
    );
    pump_for(among::notifications::COOLDOWN + Duration::from_millis(100));
    real_bell(&ctx, &backend, pane);
    wait(
        || among::notifications::commands::active_count() > 0,
        "active configured command before source withdrawal",
    );
    close_pane(&ctx, pane);
    wait(
        || among::notifications::commands::active_count() == 0,
        "source lifetime cancels and reaps configured command",
    );
    assert!(ctx
        .state
        .borrow()
        .plugins
        .supervised_pids()
        .contains(&sibling_pid));
    assert!(ctx.state.borrow().plugins.running_generation(KEY).is_some());
    ctx.window.destroy();
    wait(
        || ctx.state.borrow().plugins.is_stopped() && backend.is_stopped(),
        "normal window retirement preserves process/command shutdown",
    );
    println!("PASS combined ordinary provider/action/jobs + held HTTPS update, disable, real native uninstall, superseded transfer, fresh-token OFF/settings retention, actual explicit ON and stale reply/action rejection; sibling PID/PTY/keyboard continuity and configured-command source/window reaping observed. No audio off-thread, exceptional process, independent-review or platform approval implied.");
}
