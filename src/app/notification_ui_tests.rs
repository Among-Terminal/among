use super::bell_tests::{activate, pump_for, real_bell, terminal, wait, xdo};
use super::*;
use std::path::PathBuf;

mod installation;

fn descendants(widget: &gtk::Widget) -> Vec<gtk::Widget> {
    let mut result = vec![widget.clone()];
    let mut child = widget.first_child();
    while let Some(widget) = child {
        result.extend(descendants(&widget));
        child = widget.next_sibling();
    }
    result
}

fn named<T: IsA<gtk::Widget> + glib::object::IsClass>(
    widget: &impl IsA<gtk::Widget>,
    name: &str,
) -> T {
    descendants(widget.as_ref())
        .into_iter()
        .find(|widget| widget.widget_name() == name)
        .unwrap_or_else(|| panic!("missing control {name}"))
        .downcast::<T>()
        .ok()
        .unwrap()
}

fn preferences(ctx: &Window) -> adw::PreferencesWindow {
    open_preferences(ctx);
    pump_for(Duration::from_millis(200));
    let window = gtk::Window::list_toplevels()
        .into_iter()
        .find_map(|widget| widget.downcast::<adw::PreferencesWindow>().ok())
        .unwrap();
    activate(&window);
    assert_eq!(window.visible_page_name().as_deref(), Some("general"));
    window
}

fn key_button(button: &gtk::Button) {
    assert!(button.grab_focus());
    xdo(&["key", "--clearmodifiers", "Return"]);
    pump_for(Duration::from_millis(150));
}

fn toggle(row: &adw::SwitchRow) {
    // SwitchRow is the keyboard control; libadwaita deliberately makes its
    // internal switch non-focusable to avoid a duplicate tab stop.
    let before = row.is_active();
    assert!(row.grab_focus());
    xdo(&["key", "--clearmodifiers", "space"]);
    pump_for(Duration::from_millis(250));
    assert_ne!(
        row.is_active(),
        before,
        "Space did not operate the named SwitchRow"
    );
}

fn type_entry(row: &adw::EntryRow, text: &str) {
    assert!(row.grab_focus());
    xdo(&["key", "--clearmodifiers", "ctrl+a"]);
    xdo(&["type", "--clearmodifiers", "--delay", "1", text]);
    pump_for(Duration::from_millis(50));
    assert_eq!(row.text(), text);
}

fn app() -> (adw::Application, Window) {
    let config = PathBuf::from(std::env::var_os("XDG_CONFIG_HOME").unwrap());
    crate::test_isolation::assert_below_check_root(&config);
    assert!(
        !gtk::is_initialized(),
        "notification UI tests require a fresh process before GTK initialization"
    );
    // the owned-display runner disables a11y; GTK caches that choice at init.
    std::env::set_var("GTK_A11Y", "test");
    let app = adw::Application::builder()
        .application_id("com.github.among-terminal.notification-ui-test")
        .flags(gio::ApplicationFlags::NON_UNIQUE)
        .build();
    app.register(None::<&gio::Cancellable>).unwrap();
    let ctx = build_with_writer(&app, None, crate::session::Writer::new).unwrap();
    pump_for(Duration::from_millis(350));
    activate(&ctx.window);
    if let Some(dialog) = ctx.window.visible_dialog() {
        let dialog = dialog.downcast::<adw::AlertDialog>().unwrap();
        assert_eq!(
            dialog.heading().as_deref(),
            Some("Restore the previous session?")
        );
        let restore = descendants(dialog.upcast_ref())
            .into_iter()
            .filter_map(|widget| widget.downcast::<gtk::Button>().ok())
            .find(|button| button.label().as_deref() == Some("Restore"))
            .unwrap();
        key_button(&restore);
        wait(
            || ctx.state.borrow().session_ready,
            "explicit native session restoration before provider admission",
        );
    }
    (app, ctx)
}

fn accessible_label(widget: &impl IsA<gtk::Accessible>, expected: &str) {
    accessible_property(widget, gtk::ffi::GTK_ACCESSIBLE_PROPERTY_LABEL, expected);
}

fn accessible_property(
    widget: &impl IsA<gtk::Accessible>,
    property: gtk::ffi::GtkAccessibleProperty,
    expected: &str,
) {
    use glib::translate::*;
    assert_eq!(std::env::var("GTK_A11Y").as_deref(), Ok("test"));
    let accessible = widget.as_ref().to_glib_none().0;
    let context = unsafe { gtk::ffi::gtk_accessible_get_at_context(accessible) };
    assert!(
        !context.is_null(),
        "accessible property comparison requires a live AT context"
    );
    unsafe {
        glib::gobject_ffi::g_object_unref(context.cast());
    }
    // check_property can return null for a missing context, not only success.
    assert_ne!(
        unsafe { gtk::ffi::gtk_test_accessible_has_property(accessible, property) },
        0,
        "expected accessible property is absent"
    );
    let text = std::ffi::CString::new(expected).unwrap();
    let error = unsafe {
        gtk::ffi::gtk_test_accessible_check_property(accessible, property, text.as_ptr())
    };
    if !error.is_null() {
        let problem = unsafe { std::ffi::CStr::from_ptr(error) }
            .to_string_lossy()
            .into_owned();
        unsafe {
            glib::ffi::g_free(error.cast());
        }
        panic!("accessible property mismatch: {problem}");
    }
}

#[test]
#[ignore = "requires an owned X11 display/WM and private XDG roots; sets GTK_A11Y=test before initialization"]
fn accessibility_checks_reject_missing_and_incorrect_properties() {
    let (app, ctx) = app();
    let button = gtk::Button::with_label("Accessibility guard");
    let window = gtk::Window::builder()
        .application(&app)
        .transient_for(&ctx.window)
        .child(&button)
        .build();
    window.present();
    wait(|| button.is_mapped(), "accessibility guard mapping");
    assert!(button.is_mapped());
    button.update_property(&[gtk::accessible::Property::Description("guarded value")]);
    let check = |expected| {
        accessible_property(
            &button,
            gtk::ffi::GTK_ACCESSIBLE_PROPERTY_DESCRIPTION,
            expected,
        );
    };
    check("guarded value");
    assert!(
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| check("wrong value"))).is_err()
    );
    button.reset_property(gtk::AccessibleProperty::Description);
    assert!(
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| check("guarded value"))).is_err()
    );
    window.destroy();
    ctx.window.destroy();
    println!("PASS live accessibility comparison accepts the expected property and rejects mismatched or absent values");
}

#[test]
#[ignore = "requires owned X11/WM/private XDG, prebuilt notification-plugin and command fixtures; typed-core acceptance remains separate"]
fn ordinary_app_provider_controls_and_declarative_actions_use_one_live_core() {
    use serde_json::{json, Value};
    use std::os::unix::fs::PermissionsExt;
    if std::env::var("AMONG_PROVIDER_UI_PHASE").as_deref() == Ok("restore") {
        let root = crate::plugin::inventory::root().unwrap();
        let package = crate::plugin::inventory::read(&root)
            .unwrap()
            .into_iter()
            .find(|entry| entry.key() == "github:12/sample")
            .unwrap()
            .receipt
            .package_path(&root);
        let deliveries = |text: &str| {
            text.lines()
                .filter(|line| serde_json::from_str::<Value>(line).unwrap()["type"] == "deliver")
                .count()
        };
        let before = std::fs::read_to_string(package.join("received.jsonl")).unwrap();
        let general = crate::settings::directory().unwrap().join("general.json");
        let bytes = std::fs::read(&general).unwrap();
        let (_app, ctx) = app();
        eprintln!(
            "fresh provider startup inventory: {:?}",
            ctx.state.borrow().plugins.overview()
        );
        wait(
            || {
                ctx.state.borrow().plugins.configuration("github:12/sample")
                    == crate::plugin::host::Configuration::Applied
            },
            "fresh process retained explicit plugin enablement",
        );
        let executed = std::fs::read(package.join("executed")).unwrap();
        let prefs = preferences(&ctx);
        assert!(named::<adw::SwitchRow>(
            &prefs,
            "notification-provider:github:12/sample:first-enabled"
        )
        .is_active());
        for name in [
            "notification-provider:github:12/sample:second-enabled",
            "notification-provider:github:13/sample:first-enabled",
            "notification-provider:github:13/sample:second-enabled",
        ] {
            assert!(!named::<adw::SwitchRow>(&prefs, name).is_active());
        }
        pump_for(Duration::from_millis(400));
        assert_eq!(
            std::fs::read(general).unwrap(),
            bytes,
            "fresh view saved defaults or replay state"
        );
        assert_eq!(
            std::fs::read(package.join("executed")).unwrap(),
            executed,
            "view restarted the managed process"
        );
        assert_eq!(
            deliveries(&std::fs::read_to_string(package.join("received.jsonl")).unwrap()),
            deliveries(&before),
            "fresh process replayed an old delivery"
        );
        prefs.destroy();
        ctx.window.destroy();
        println!("PASS fresh ordinary app restores independent real provider choices; General view neither restarts admitted providers nor writes defaults/replays delivery");
        return;
    }
    let config = crate::settings::directory().unwrap();
    crate::test_isolation::assert_below_check_root(&config);
    let root = crate::plugin::inventory::root().unwrap();
    let mut receipt = crate::plugin::inventory::tests::fixture(&root);
    let package = receipt.package_path(&root);
    let binary = std::env::current_exe()
        .unwrap()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("examples/notification-plugin");
    assert!(binary.is_file());
    std::fs::copy(&binary, package.join("plugin")).unwrap();
    std::fs::set_permissions(
        package.join("plugin"),
        std::fs::Permissions::from_mode(0o700),
    )
    .unwrap();
    let mut manifest: Value =
        serde_json::from_slice(&std::fs::read(package.join("among-plugin.json")).unwrap()).unwrap();
    manifest["required_features"] = json!(["status", "settings"]);
    manifest["optional_features"] = json!([
        "notifications.ui",
        "notifications.deliver",
        "context.terminals"
    ]);
    manifest["delivery_channels"] = json!([
        {"id":"first","name":"Same display","description":"First independent channel"},
        {"id":"second","name":"Same display","description":"Second independent channel"}
    ]);
    let bytes = manifest.to_string();
    std::fs::write(package.join("among-plugin.json"), &bytes).unwrap();
    receipt.manifest_sha256 = crate::plugin::inventory::checksum(bytes.as_bytes()).unwrap();
    let mut other = receipt.clone();
    other.repository_id = 13;
    let other_package = other.package_path(&root);
    std::fs::create_dir_all(&other_package).unwrap();
    let mut path = other_package.as_path();
    while path.starts_with(&root) {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
        if path == root {
            break;
        }
        path = path.parent().unwrap();
    }
    for name in ["among-plugin.json", "plugin"] {
        std::fs::copy(package.join(name), other_package.join(name)).unwrap();
    }
    std::fs::write(
        root.join("installed.json"),
        json!({"version":1,"installations":[receipt,other]}).to_string(),
    )
    .unwrap();
    let (_app, ctx) = app();
    split_focused(&ctx);
    pump_for(Duration::from_millis(200));
    let backend = ctx.state.borrow().bells.clone().unwrap();
    let bridge = backend.notification_backend();
    let first = PaneId(1);
    let second = ctx.state.borrow().workspaces.active().focused;
    assert_eq!(
        bridge.borrow().source(first.0),
        backend.with_host(|host| host.source(first.0)).unwrap()
    );
    let prefs = preferences(&ctx);
    assert_eq!(backend.provider_inventory().len(), 4);
    assert!(backend
        .provider_inventory()
        .iter()
        .all(|channel| !channel.policy.enabled && !channel.plugin_enabled));
    assert!(!package.join("executed").exists() && !other_package.join("executed").exists());
    assert!(!config.join("general.json").exists(), "view wrote defaults");
    let enabled: adw::SwitchRow = named(
        &prefs,
        "notification-provider:github:12/sample:first-enabled",
    );
    let condition: adw::ComboRow = named(
        &prefs,
        "notification-provider:github:12/sample:first-condition",
    );
    assert!(condition.grab_focus());
    xdo(&["key", "--clearmodifiers", "Return", "Home", "Return"]);
    pump_for(Duration::from_millis(150));
    assert_eq!(condition.selected(), 0);
    toggle(&enabled);
    assert!(
        !package.join("executed").exists(),
        "channel permission started disabled plugin"
    );
    assert!(backend
        .provider_inventory()
        .iter()
        .filter(|channel| channel.policy.enabled)
        .all(
            |channel| channel.declaration.key.identity == "github:12/sample"
                && channel.declaration.key.channel == "first"
        ));
    // Review is explicit fixture setup, not a claim about the separate native
    // access-review workflow. Enablement below still uses the real manager.
    let features = json!([
        "status",
        "settings",
        "notifications.ui",
        "notifications.deliver",
        "context.terminals"
    ]);
    for identity in ["github:12/sample", "github:13/sample"] {
        ctx.state
            .borrow_mut()
            .plugins
            .review_access(
                identity,
                json!({"owner":"example","repository":"sample","features":features}),
            )
            .unwrap();
    }
    let plugin_page = descendants(prefs.upcast_ref())
        .into_iter()
        .filter_map(|widget| widget.downcast::<adw::PreferencesPage>().ok())
        .find(|page| page.name().as_deref() == Some("plugins"))
        .unwrap();
    prefs.set_visible_page(&plugin_page);
    let switch_for = |identity: &str| {
        let row = descendants(plugin_page.upcast_ref())
            .into_iter()
            .filter_map(|widget| widget.downcast::<adw::ActionRow>().ok())
            .find(|row| row.subtitle().is_some_and(|text| text.contains(identity)))
            .unwrap();
        descendants(row.upcast_ref())
            .into_iter()
            .find_map(|widget| widget.downcast::<gtk::Switch>().ok())
            .unwrap()
    };
    for identity in ["github:12/sample", "github:13/sample"] {
        let switch = switch_for(identity);
        assert!(!switch.is_active() && switch.grab_focus());
        xdo(&["key", "--clearmodifiers", "space"]);
        wait(|| switch.is_active(), "native managed enable");
        wait(
            || {
                ctx.state.borrow().plugins.configuration(identity)
                    == crate::plugin::host::Configuration::Applied
            },
            "ordinary application admission",
        );
        ctx.state
            .borrow_mut()
            .settings
            .managed_review_for_installation(
                identity,
                json!({"owner":"example","repository":"sample","features":features}),
                None,
            )
            .unwrap();
    }
    let frames = |package: &std::path::Path| -> Vec<Value> {
        std::fs::read_to_string(package.join("received.jsonl"))
            .unwrap_or_default()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    };
    let mut serial = 0;
    let mut send = |package: &std::path::Path, message: Value| {
        serial += 1;
        let temporary = package.join("ui-pending.json");
        let destination = package.join(format!("input-{serial}.json"));
        std::fs::write(&temporary, message.to_string()).unwrap();
        std::fs::rename(temporary, &destination).unwrap();
        wait(
            || !destination.exists(),
            "actual managed contribution ingress",
        );
        pump_for(Duration::from_millis(100));
    };
    real_bell(&ctx, &backend, first);
    wait(
        || {
            frames(&package)
                .iter()
                .any(|frame| frame["type"] == "deliver")
        },
        "normal VTE to ordinary provider",
    );
    let deliveries: Vec<_> = frames(&package)
        .into_iter()
        .filter(|frame| frame["type"] == "deliver")
        .collect();
    assert_eq!(deliveries.len(), 1);
    assert_eq!(deliveries[0]["delivery"]["event"]["kind"], "terminal_bell");
    assert_eq!(deliveries[0]["delivery"]["channel"], "first");
    assert!(frames(&other_package)
        .iter()
        .all(|frame| frame["type"] != "deliver"));
    let workspace = ctx.state.borrow().workspaces.active().reference.to_string();
    for (id, slot, target) in [
        ("app", "app_header", json!({"kind":"application"})),
        ("footer", "app_footer", json!({"kind":"application"})),
        (
            "workspace",
            "workspace_header",
            json!({"kind":"workspace","reference":workspace}),
        ),
        (
            "summary",
            "workspace_summary",
            json!({"kind":"workspace","reference":workspace}),
        ),
        (
            "pane",
            "pane_header",
            json!({"kind":"pane","reference":second.0.to_string()}),
        ),
        (
            "pane-footer",
            "pane_footer",
            json!({"kind":"pane","reference":second.0.to_string()}),
        ),
    ] {
        send(
            &package,
            json!({"type":"notification_contribution","session":"$session","generation":"$generation",
            "contribution":{"id":id,"revision":1,"scope":{"slot":slot,"target":target},
                "source_generation":bridge.borrow().source(second.0).unwrap().generation().to_string(),
                "text":format!("Native {id}"),"tooltip":"Plain notification text","style":"warning",
                "icon":"theme:dialog-information-symbolic","actions":[{"id":"ack","label":format!("Acknowledge {id}")}]}}),
        );
    }
    prefs.destroy();
    activate(&ctx.window);
    let buttons = || {
        descendants(ctx.window.upcast_ref())
            .into_iter()
            .filter_map(|widget| widget.downcast::<gtk::Button>().ok())
            .filter(|button| button.widget_name() == "notification-contribution-action")
            .collect::<Vec<_>>()
    };
    wait(
        || buttons().len() == 6,
        "six actual allowed-scope native action controls",
    );
    let marker =
        PathBuf::from(std::env::var_os("HOME").unwrap()).join("notification-action-vte-misroute");
    terminal(&ctx, second).grab_focus();
    xdo(&[
        "type",
        "--clearmodifiers",
        "--delay",
        "0",
        &format!("printf WRONG > '{}'", marker.display()),
    ]);
    let mut retained = None;
    let mut ordered = buttons();
    ordered.sort_by_key(|button| button.label().as_deref() == Some("Acknowledge pane-footer"));
    for button in ordered {
        assert!(gtk::test_accessible_has_role(
            &button,
            gtk::AccessibleRole::Button
        ));
        accessible_label(&button, button.label().unwrap().as_str());
        key_button(&button);
        retained = Some(button);
    }
    wait(
        || {
            frames(&package)
                .iter()
                .filter(|frame| frame["type"] == "notification_action")
                .count()
                == 6
        },
        "opaque native action routing",
    );
    assert!(!marker.exists(), "notification action Enter reached VTE");
    let retained = retained.unwrap();
    refresh_slots(&ctx);
    pump_for(Duration::from_millis(200));
    assert!(
        retained.has_focus(),
        "slot polling replaced focused notification action"
    );
    assert!(buttons().contains(&retained));
    let source = bridge.borrow().source(second.0).unwrap();
    close_focused(&ctx);
    wait(
        || bridge.borrow().source(second.0).is_none(),
        "real pane lifetime revocation",
    );
    assert!(!backend.with_host(|host| host.is_live(source)).unwrap());
    wait(
        || buttons().len() == 4,
        "withdrawn pane-scoped native controls",
    );
    let count = frames(&package)
        .iter()
        .filter(|frame| frame["type"] == "notification_action")
        .count();
    retained.emit_clicked();
    pump_for(Duration::from_millis(200));
    assert_eq!(
        frames(&package)
            .iter()
            .filter(|frame| frame["type"] == "notification_action")
            .count(),
        count,
        "stale opaque action routed after pane close"
    );
    terminal(&ctx, first).feed_child(b"\x15printf '\\033]0;provider-ui-input-alive\\007'\n");
    wait(
        || terminal(&ctx, first).window_title().as_deref() == Some("provider-ui-input-alive"),
        "real terminal survives contribution updates",
    );
    ctx.window.destroy();
    wait(
        || ctx.state.borrow().plugins.supervised_count() == 0 && backend.is_stopped(),
        "parent retirement before fresh relaunch",
    );
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["app::notification_ui_tests::ordinary_app_provider_controls_and_declarative_actions_use_one_live_core", "--exact", "--ignored", "--nocapture", "--test-threads=1"])
        .env("AMONG_PROVIDER_UI_PHASE", "restore").output().unwrap();
    let log = crate::test_isolation::check_root()
        .join(format!("provider-ui-fresh-{}.log", std::process::id()));
    std::fs::write(
        &log,
        [output.stdout.as_slice(), output.stderr.as_slice()].concat(),
    )
    .unwrap();
    assert!(
        output.status.success(),
        "fresh provider phase failed; {}",
        log.display()
    );
    println!("PASS ordinary app single core, four same-name identity/channel rows off by default, channel permission does not start plugins, real manager enable/provider/VTE delivery, all six allowed declarative areas, native accessible opaque keyboard actions with zero VTE misroute, stable focused widgets across slot refresh and live pane withdrawal, real fresh-process policy restoration. Typed core/origin command follow-up and broader acceptance remain open.");
}

#[test]
#[ignore = "requires owned X11/WM/private XDG and the prebuilt notification-plugin; access review only, not independent backend acceptance"]
fn native_role_review_is_explicit_reachable_and_restored_without_starting_code() {
    use crate::plugin::managed::NotificationRole;
    use serde_json::{json, Value};
    use std::os::unix::fs::PermissionsExt;
    let key = "github:12/sample";
    let config = crate::settings::directory()
        .unwrap()
        .join("managed-plugins.json");
    let root = crate::plugin::inventory::root().unwrap();
    if std::env::var("AMONG_ROLE_UI_PHASE").as_deref() == Ok("restore") {
        let before = std::fs::read(&config).unwrap();
        let (_app, ctx) = app();
        let prefs = preferences(&ctx);
        assert_eq!(
            ctx.state
                .borrow()
                .plugins
                .reviewed_notification_role(key)
                .unwrap(),
            Some(NotificationRole::Producer)
        );
        assert_eq!(
            ctx.state.borrow().plugins.admitted_notification_role(key),
            None
        );
        let review: gtk::Button = named(&prefs, "notification-review-role:github:12/sample");
        key_button(&review);
        let dialog = prefs
            .visible_dialog()
            .unwrap()
            .downcast::<adw::AlertDialog>()
            .unwrap();
        assert_eq!(
            named::<adw::ComboRow>(&dialog, "notification-role-choice").selected(),
            2
        );
        xdo(&["key", "--clearmodifiers", "Escape"]);
        pump_for(Duration::from_millis(200));
        assert_eq!(
            std::fs::read(&config).unwrap(),
            before,
            "fresh role inspection wrote settings"
        );
        assert_eq!(
            ctx.state.borrow().plugins.supervised_count(),
            0,
            "fresh saved review enabled code"
        );
        prefs.destroy();
        ctx.window.destroy();
        println!("PASS fresh ordinary process restores explicit producer review separately from admitted none without starting/restarting code or writing defaults");
        return;
    }
    crate::test_isolation::assert_below_check_root(&root);
    let mut receipt = crate::plugin::inventory::tests::fixture(&root);
    let package = receipt.package_path(&root);
    let binary = std::env::current_exe()
        .unwrap()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("examples/notification-plugin");
    assert!(binary.is_file());
    std::fs::copy(binary, package.join("plugin")).unwrap();
    std::fs::set_permissions(
        package.join("plugin"),
        std::fs::Permissions::from_mode(0o700),
    )
    .unwrap();
    let mut manifest: Value =
        serde_json::from_slice(&std::fs::read(package.join("among-plugin.json")).unwrap()).unwrap();
    manifest["required_features"] = json!(["status", "settings"]);
    manifest["optional_features"] = json!([
        "notifications.ui",
        "notifications.produce",
        "notifications.deliver"
    ]);
    manifest["delivery_channels"] =
        json!([{"id":"first","name":"Synthetic","description":"Review does not enable delivery"}]);
    let bytes = manifest.to_string();
    std::fs::write(package.join("among-plugin.json"), &bytes).unwrap();
    receipt.manifest_sha256 = crate::plugin::inventory::checksum(bytes.as_bytes()).unwrap();
    let mut conflicting = receipt.clone();
    conflicting.repository_id = 13;
    let other = conflicting.package_path(&root);
    std::fs::create_dir_all(&other).unwrap();
    let mut path = other.as_path();
    while path.starts_with(&root) {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
        if path == root {
            break;
        }
        path = path.parent().unwrap();
    }
    std::fs::copy(package.join("plugin"), other.join("plugin")).unwrap();
    manifest["required_features"] = json!([
        "status",
        "settings",
        "notifications.produce",
        "notifications.deliver"
    ]);
    manifest["optional_features"] = json!(["notifications.ui"]);
    let bytes = manifest.to_string();
    std::fs::write(other.join("among-plugin.json"), &bytes).unwrap();
    std::fs::set_permissions(
        other.join("among-plugin.json"),
        std::fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    conflicting.manifest_sha256 = crate::plugin::inventory::checksum(bytes.as_bytes()).unwrap();
    std::fs::write(
        root.join("installed.json"),
        json!({"version":1,"installations":[receipt,conflicting]}).to_string(),
    )
    .unwrap();
    std::fs::create_dir_all(config.parent().unwrap()).unwrap();
    std::fs::set_permissions(
        config.parent().unwrap(),
        std::fs::Permissions::from_mode(0o700),
    )
    .unwrap();
    let inspected = crate::plugin::inventory::read(&root).unwrap();
    assert_eq!(inspected.len(), 2);
    for entry in &inspected {
        assert!(
            entry.inspected.is_ok(),
            "role fixture {} is not inspected: {:?}",
            entry.key(),
            entry.inspected
        );
    }
    let original = json!({"version":1,"future":"opaque","plugins":{
        "github:12/sample":{"enabled":false,"settings":{"version":1,"values":{"future":"retained"}},"custom":42},
        "github:13/sample":{"enabled":false,"custom":"other identity"}
    }});
    std::fs::write(&config, original.to_string()).unwrap();
    std::fs::set_permissions(&config, std::fs::Permissions::from_mode(0o600)).unwrap();
    let (_app, ctx) = app();
    let prefs = preferences(&ctx);
    let open = |identity: &str| {
        eprintln!(
            "role review startup inventory: {:?}",
            ctx.state.borrow().plugins.overview()
        );
        let review: gtk::Button = named(&prefs, &format!("notification-review-role:{identity}"));
        accessible_label(
            &review,
            &format!("Review notification role and access for {identity}"),
        );
        key_button(&review);
        wait(
            || prefs.visible_dialog().is_some(),
            "native access review from General",
        );
        prefs
            .visible_dialog()
            .unwrap()
            .downcast::<adw::AlertDialog>()
            .unwrap()
    };
    let response = |dialog: &adw::AlertDialog, label: &str| {
        descendants(dialog.upcast_ref())
            .into_iter()
            .filter_map(|widget| widget.downcast::<gtk::Button>().ok())
            .find(|button| button.label().as_deref() == Some(label))
            .unwrap()
    };
    let before = std::fs::read(&config).unwrap();
    let dialog = open("github:13/sample");
    assert!(dialog.body().contains("cannot both be required"));
    assert!(!other.join("executed").exists());
    xdo(&["key", "--clearmodifiers", "Escape"]);
    pump_for(Duration::from_millis(200));
    assert_eq!(std::fs::read(&config).unwrap(), before);
    let dialog = open(key);
    let picker: adw::ComboRow = named(&dialog, "notification-role-choice");
    accessible_label(
        picker.upcast_ref::<gtk::Widget>(),
        "Notification role for this process",
    );
    assert_eq!(picker.selected(), 0);
    assert!(
        !response(&dialog, "Save reviewed access").is_sensitive(),
        "ambiguous optional role accepted without a choice"
    );
    xdo(&["key", "--clearmodifiers", "Escape"]);
    pump_for(Duration::from_millis(200));
    assert_eq!(
        std::fs::read(&config).unwrap(),
        before,
        "Cancel wrote consent"
    );
    for (index, expected) in [
        (2, Some(NotificationRole::Producer)),
        (3, Some(NotificationRole::Delivery)),
        (1, None),
        (3, Some(NotificationRole::Delivery)),
    ] {
        let dialog = open(key);
        let picker: adw::ComboRow = named(&dialog, "notification-role-choice");
        assert!(picker.grab_focus());
        xdo(&["key", "--clearmodifiers", "Return", "Home"]);
        for _ in 0..index {
            xdo(&["key", "--clearmodifiers", "Down"]);
        }
        xdo(&["key", "--clearmodifiers", "Return"]);
        pump_for(Duration::from_millis(150));
        assert_eq!(picker.selected(), index);
        let ui = descendants(dialog.upcast_ref())
            .into_iter()
            .filter_map(|widget| widget.downcast::<gtk::CheckButton>().ok())
            .find(|control| control.label().as_deref() == Some("notifications.ui"))
            .unwrap();
        if !ui.is_active() {
            assert!(ui.grab_focus());
            xdo(&["key", "--clearmodifiers", "space"]);
            pump_for(Duration::from_millis(100));
        }
        assert!(
            ui.is_active(),
            "UI must stay independent of the exclusive role"
        );
        key_button(&response(&dialog, "Save reviewed access"));
        wait(
            || prefs.visible_dialog().is_none(),
            "native reviewed role save",
        );
        assert_eq!(
            ctx.state
                .borrow()
                .plugins
                .reviewed_notification_role(key)
                .unwrap(),
            expected
        );
        assert_eq!(
            ctx.state.borrow().plugins.admitted_notification_role(key),
            None
        );
        assert!(
            !package.join("executed").exists(),
            "saving role started code"
        );
        let saved: Value = serde_json::from_slice(&std::fs::read(&config).unwrap()).unwrap();
        assert_eq!(saved["plugins"][key]["enabled"], false);
        assert_eq!(saved["future"], original["future"]);
        assert_eq!(saved["plugins"][key]["custom"], 42);
        assert_eq!(
            saved["plugins"][key]["settings"],
            original["plugins"][key]["settings"]
        );
        assert_eq!(
            saved["plugins"]["github:13/sample"],
            original["plugins"]["github:13/sample"]
        );
        assert!(saved["plugins"][key]["review"]["features"]
            .as_array()
            .unwrap()
            .contains(&json!("notifications.ui")));
    }
    // Enable only through the existing native Plugins manager after the separate
    // role save; its ordinary admission must reflect the reviewed role.
    let page = descendants(prefs.upcast_ref())
        .into_iter()
        .filter_map(|widget| widget.downcast::<adw::PreferencesPage>().ok())
        .find(|page| page.name().as_deref() == Some("plugins"))
        .unwrap();
    prefs.set_visible_page(&page);
    let row = descendants(page.upcast_ref())
        .into_iter()
        .filter_map(|widget| widget.downcast::<adw::ActionRow>().ok())
        .find(|row| row.subtitle().is_some_and(|text| text.contains(key)))
        .unwrap();
    let switch = descendants(row.upcast_ref())
        .into_iter()
        .find_map(|widget| widget.downcast::<gtk::Switch>().ok())
        .unwrap();
    assert!(switch.grab_focus());
    xdo(&["key", "--clearmodifiers", "space"]);
    wait(
        || {
            ctx.state.borrow().plugins.admitted_notification_role(key)
                == Some(NotificationRole::Delivery)
        },
        "real reviewed delivery admission",
    );
    assert!(ctx
        .state
        .borrow()
        .bells
        .as_ref()
        .unwrap()
        .provider_inventory()
        .iter()
        .all(|channel| !channel.policy.enabled));
    prefs.set_visible_page_name("general");
    wait(
        || {
            named::<adw::ActionRow>(&prefs, "notification-role-status:github:12/sample")
                .subtitle()
                .is_some_and(|text| text.contains("reviewed delivery · admitted delivery"))
        },
        "native reviewed/admitted delivery display",
    );
    let dialog = open(key);
    assert!(dialog.body().contains("Stop this plugin"));
    key_button(&response(&dialog, "Stop and disable plugin"));
    wait(
        || ctx.state.borrow().plugins.supervised_count() == 0,
        "explicit role-review stop retires the generation",
    );
    let executed = std::fs::read(package.join("executed")).unwrap();
    let dialog = open(key);
    let picker: adw::ComboRow = named(&dialog, "notification-role-choice");
    assert_eq!(
        picker.selected(),
        3,
        "existing valid consent must be reachable for re-review"
    );
    assert!(picker.grab_focus());
    xdo(&[
        "key",
        "--clearmodifiers",
        "Return",
        "Home",
        "Down",
        "Down",
        "Return",
    ]);
    pump_for(Duration::from_millis(150));
    key_button(&response(&dialog, "Save reviewed access"));
    wait(
        || prefs.visible_dialog().is_none(),
        "save changed role while stopped",
    );
    assert_eq!(
        ctx.state
            .borrow()
            .plugins
            .reviewed_notification_role(key)
            .unwrap(),
        Some(NotificationRole::Producer)
    );
    wait(
        || {
            named::<adw::ActionRow>(&prefs, "notification-role-status:github:12/sample")
                .subtitle()
                .is_some_and(|text| text.contains("reviewed producer · admitted none"))
        },
        "native saved versus stopped admitted role display",
    );
    assert_eq!(
        ctx.state.borrow().plugins.admitted_notification_role(key),
        None
    );
    pump_for(Duration::from_millis(300));
    assert_eq!(
        std::fs::read(package.join("executed")).unwrap(),
        executed,
        "changed role restarted code"
    );
    prefs.destroy();
    ctx.window.destroy();
    wait(
        || ctx.state.borrow().plugins.supervised_count() == 0,
        "role-test parent retirement",
    );
    let child = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["app::notification_ui_tests::native_role_review_is_explicit_reachable_and_restored_without_starting_code", "--exact", "--ignored", "--nocapture", "--test-threads=1"])
        .env("AMONG_ROLE_UI_PHASE", "restore").output().unwrap();
    let log = crate::test_isolation::check_root()
        .join(format!("role-ui-fresh-{}.log", std::process::id()));
    std::fs::write(
        &log,
        [child.stdout.as_slice(), child.stderr.as_slice()].concat(),
    )
    .unwrap();
    assert!(
        child.status.success(),
        "fresh role process failed: {}",
        log.display()
    );
    println!("PASS General role review is keyboard named/reachable with existing consent; required-both refuses, optional producer/delivery/neither is explicit, UI remains independent, reviewed/admitted roles differ honestly, no channel enablement/start/restart from save, explicit stop precedes role change, unknown settings/other identity survive and fresh process restores the stopped review");
}

#[test]
#[ignore = "requires an owned X11 display/WM with GTK_A11Y=test and private XDG roots"]
fn visual_attention_is_live_static_accessible_and_geometry_neutral() {
    let (_app, ctx) = app();
    let backend = ctx.state.borrow().bells.clone().unwrap();
    let first = ctx.state.borrow().workspaces.active().focused;
    split_focused(&ctx);
    pump_for(Duration::from_millis(200));
    let second = ctx.state.borrow().workspaces.active().focused;
    let term = terminal(&ctx, first);
    term.update_property(&[gtk::accessible::Property::Description(
        "Native terminal description retained",
    )]);
    let header = term.parent().unwrap().first_child().unwrap();
    assert!(gtk::test_accessible_has_role(
        &header,
        gtk::AccessibleRole::Group
    ));
    let stripe = find_with_class(&header, "workspace-stripe").unwrap();
    let badge = find_with_class(&header, "notification-unread").unwrap();
    let geometry = (
        term.width(),
        term.height(),
        term.row_count(),
        term.column_count(),
    );
    let colour = stripe.css_classes();
    gtk::Settings::default()
        .unwrap()
        .set_gtk_enable_animations(true);
    real_bell(&ctx, &backend, first);
    wait(
        || stripe.has_css_class("notification-pulse"),
        "header stripe pulse",
    );
    assert!(badge.is_visible());
    accessible_label(&header, &format!("Terminal {}, unread bell", first.0));
    accessible_property(
        &term,
        gtk::ffi::GTK_ACCESSIBLE_PROPERTY_DESCRIPTION,
        "Native terminal description retained",
    );
    let row = ctx.workspace_list.row_at_index(0).unwrap();
    accessible_label(&row, "project, 2 terminals, 1 unread bells");
    assert_eq!(
        geometry,
        (
            term.width(),
            term.height(),
            term.row_count(),
            term.column_count()
        )
    );
    gtk::Settings::default()
        .unwrap()
        .set_gtk_enable_animations(false);
    wait(
        || !stripe.has_css_class("notification-pulse"),
        "dynamic reduced motion",
    );
    assert!(badge.is_visible() && backend.attention(first.0).unread);
    for class in colour.iter() {
        assert!(stripe.has_css_class(class));
    }
    ctx.state.borrow_mut().sidebar_collapsed = true;
    rebuild_sidebar(&ctx, true);
    present_attention(&ctx);
    let row = ctx.workspace_list.row_at_index(0).unwrap();
    assert!(find_with_class(row.upcast_ref(), "notification-unread")
        .unwrap()
        .is_visible());
    accessible_label(&row, "project, 2 terminals, 1 unread bells");
    ctx.state.borrow_mut().sidebar_collapsed = false;
    rebuild_sidebar(&ctx, true);
    let row = ctx.workspace_list.row_at_index(0).unwrap();
    let content = row.child().unwrap().downcast::<gtk::Box>().unwrap();
    ctx.state.borrow_mut().renaming = Some(0);
    Rename::begin(&ctx, 0, &content, "project", 0).unwrap();
    pump_for(Duration::from_millis(100));
    real_bell(&ctx, &backend, first);
    assert_eq!(
        ctx.workspace_list.row_at_index(0).unwrap(),
        row,
        "bell replaced the row during rename"
    );
    let entry = descendants(&content.upcast())
        .into_iter()
        .find_map(|widget| widget.downcast::<gtk::Entry>().ok())
        .unwrap();
    assert!(
        gtk::prelude::RootExt::focus(&ctx.window).is_some_and(|focus| focus
            == entry.clone().upcast::<gtk::Widget>()
            || focus.is_ancestor(&entry))
    );
    xdo(&["key", "--clearmodifiers", "ctrl+a"]);
    xdo(&["type", "--clearmodifiers", "renamed"]);
    xdo(&["key", "--clearmodifiers", "Return"]);
    wait(
        || ctx.state.borrow().workspaces.active().name == "renamed",
        "real rename keyboard input",
    );
    assert!(backend.attention(first.0).unread);
    toggle_zoom(&ctx);
    pump_for(Duration::from_millis(100));
    assert!(!term.is_mapped());
    real_bell(&ctx, &backend, first);
    let restore = find_with_class(ctx.pane_area.upcast_ref(), "pane-restore")
        .unwrap()
        .downcast::<gtk::Button>()
        .unwrap();
    key_button(&restore);
    wait(|| term.is_mapped(), "Restore hidden unread sibling");
    assert!(backend.attention(first.0).unread);
    assert_eq!(
        backend.focus().viewed.map(|source| source.pane()),
        Some(second.0)
    );
    let prefs = preferences(&ctx);
    real_bell(&ctx, &backend, second);
    rebuild(&ctx);
    assert!(backend.attention(first.0).unread && backend.attention(second.0).unread);
    prefs.destroy();
    activate(&ctx.window);
    terminal(&ctx, first).grab_focus();
    wait(
        || !backend.attention(first.0).unread,
        "actual origin focus acknowledgement",
    );
    xdo(&[
        "type",
        "--clearmodifiers",
        "printf '\\033]0;visual-input-alive\\007'",
    ]);
    xdo(&["key", "--clearmodifiers", "Return"]);
    wait(
        || term.window_title().as_deref() == Some("visual-input-alive"),
        "real source keyboard input",
    );
    ctx.window.destroy();
    println!("PASS actual header/sidebar pulse and static !, GTK accessible Group/ListItem properties, dynamic reduced motion, unchanged hue and terminal cell/allocation geometry, expanded/collapsed rows, uninterrupted rename, hidden sibling/Restore, Preferences reconstruction, per-source actual focus and real keyboard input");
}

#[test]
#[ignore = "requires owned X11/private XDG plus prebuilt Rust command fixture; ordinary small trees only, not exceptional-path or packaged acceptance"]
fn general_controls_drive_production_commands_and_application_retirement() {
    let (app, ctx) = app();
    let backend = ctx.state.borrow().bells.clone().unwrap();
    let first = ctx.state.borrow().workspaces.active().focused;
    split_focused(&ctx);
    pump_for(Duration::from_millis(150));
    let target = std::env::current_exe()
        .unwrap()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_owned();
    let fixture = target.join("notification-command-fixture");
    assert!(fixture.is_file());
    let root = crate::test_isolation::check_root()
        .join(format!("production-command-ui-{}", std::process::id()));
    std::fs::create_dir(&root).unwrap();
    let data = PathBuf::from(std::env::var_os("XDG_DATA_HOME").unwrap())
        .join("among/notification-command-home");
    let general = crate::settings::directory().unwrap().join("general.json");
    let prefs = preferences(&ctx);
    assert!(
        !general.exists() && !data.exists(),
        "opening Preferences wrote defaults or prepared command cwd"
    );
    let program: adw::EntryRow = named(&prefs, "notification-command-program");
    let arguments: adw::EntryRow = named(&prefs, "notification-command-arguments");
    let save: gtk::Button = named(&prefs, "notification-command-save");
    let enabled: adw::SwitchRow = named(&prefs, "notification-command-enabled");
    assert!(!enabled.is_active() && !enabled.is_sensitive());
    type_entry(&program, fixture.to_str().unwrap());
    type_entry(
        &arguments,
        &serde_json::json!(["record", root.to_str().unwrap(), "; $HOME $(false)"]).to_string(),
    );
    key_button(&save);
    assert!(enabled.is_sensitive() && !enabled.is_active());
    assert!(
        !data.exists(),
        "saving configuration performed eager directory work"
    );
    let condition: adw::ComboRow = named(&prefs, "notification-command-condition");
    assert!(condition.grab_focus());
    xdo(&["key", "--clearmodifiers", "Return"]);
    pump_for(Duration::from_millis(100));
    xdo(&["key", "--clearmodifiers", "Home", "Return"]);
    pump_for(Duration::from_millis(200));
    assert_eq!(condition.selected(), 0, "keyboard condition selection");
    toggle(&enabled);
    assert!(enabled.is_active());
    assert!(
        !data.exists(),
        "enable before an event performed eager directory work"
    );
    real_bell(&ctx, &backend, first);
    wait(
        || {
            std::fs::read_dir(&root).unwrap().any(|entry| {
                entry
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .starts_with("eof-")
            })
        },
        "normal VTE -> saved General command -> runtime -> actual EOF",
    );
    wait(
        || backend.command_status().contains("Exited(0)"),
        "normal production poll completion",
    );
    assert!(data.is_dir());
    let first_channel = backend
        .with_host(|host| {
            host.command_completions()
                .next_back()
                .unwrap()
                .job
                .channel()
        })
        .unwrap();
    assert_eq!(
        backend
            .preferences()
            .configuration
            .as_ref()
            .unwrap()
            .arguments()[2],
        "; $HOME $(false)"
    );
    assert!(
        crate::settings::load(&general, crate::settings::Document::General).unwrap()
            ["notifications"]["command"]["enabled"]
            .as_bool()
            .unwrap()
    );
    let tree = root.join("tree");
    std::fs::create_dir(&tree).unwrap();
    type_entry(
        &arguments,
        &serde_json::json!(["tree", tree.to_str().unwrap()]).to_string(),
    );
    key_button(&save);
    real_bell(&ctx, &backend, first);
    wait(
        || {
            std::fs::read_dir(&tree)
                .unwrap()
                .filter(|entry| {
                    entry
                        .as_ref()
                        .unwrap()
                        .file_name()
                        .to_string_lossy()
                        .starts_with("child-")
                })
                .count()
                == 2
        },
        "production owned stubborn command tree",
    );
    prefs.destroy();
    let ticks = Rc::new(Cell::new(0));
    let ticking = ticks.clone();
    let timer = glib::timeout_add_local(Duration::from_millis(10), move || {
        ticking.set(ticking.get() + 1);
        glib::ControlFlow::Continue
    });
    let started = std::time::Instant::now();
    ctx.window.destroy();
    assert!(
        started.elapsed() < Duration::from_millis(50),
        "window retirement blocked GTK"
    );
    // real GApplication run must remain alive on the removal handler's hold,
    // without a window, until the actual supervisor/descendant cleanup finishes.
    let _ = app.run_with_args(&["among-notification-retirement-check"]);
    timer.remove();
    assert!(started.elapsed() >= Duration::from_millis(200));
    assert!(
        ticks.get() >= 10,
        "main loop stopped progressing during retirement"
    );
    assert_eq!(among::notifications::commands::active_count(), 0);
    assert!(backend.is_stopped());
    let final_channel = backend
        .with_host(|host| {
            host.command_completions()
                .next_back()
                .unwrap()
                .job
                .channel()
        })
        .unwrap();
    assert_eq!(
        first_channel, final_channel,
        "configuration replacement changed channel identity"
    );
    println!("PASS real labelled General keyboard program/literal-argv/condition/enable controls drive the normal VTE backend registration, lazy off-loop directory preparation, shared-tick execution/completion, stable ownership and nonblocking GApplication hold through real small-tree reaping; PID selection has separate repair/re-review evidence, not exceptional-path or packaged acceptance");
}

#[test]
#[ignore = "requires owned X11/private XDG roots; spawns fresh test processes on that same owned display"]
fn general_settings_persist_across_real_fresh_processes_and_write_failure() {
    use among::notifications::preferences::Preferences;
    let phase = std::env::var("AMONG_GENERAL_TEST_PHASE").ok();
    if phase.is_none() {
        use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
        let root = crate::test_isolation::check_root()
            .join(format!("general-restart-{}", std::process::id()));
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&root)
            .unwrap();
        for dir in [
            "home",
            "config",
            "config/among",
            "data",
            "data/among",
            "cache",
        ] {
            std::fs::DirBuilder::new()
                .mode(0o700)
                .create(root.join(dir))
                .unwrap();
        }
        let config = root.join("config/among");
        let original = serde_json::json!({"version":1,"future":{"keep":42},"notifications":{
            "visual":{"enabled":true,"future":7},"audio":{"enabled":false,"source":"builtin","future":"retained"},
            "providers":{"origin-one/same":{"channels":{"delivery":{"enabled":false,"future":7}}},
                "origin-two/same":{"channels":{"delivery":{"enabled":false}}}}}});
        std::fs::write(config.join("general.json"), original.to_string()).unwrap();
        let legacy = br#"{"plugins":[{"name":"missing","enabled":false,"future":7}],"keep":42}"#;
        let managed = br#"{"version":1,"plugins":{"github:1/sample":{"enabled":false,"installation":"retained","settings":{"future":7}}},"future":42}"#;
        std::fs::write(config.join("plugins.json"), legacy).unwrap();
        std::fs::write(config.join("managed-plugins.json"), managed).unwrap();
        for name in ["general.json", "plugins.json", "managed-plugins.json"] {
            std::fs::set_permissions(config.join(name), std::fs::Permissions::from_mode(0o600))
                .unwrap();
        }
        for (index, phase) in ["edit", "restore", "failure", "restore"].iter().enumerate() {
            let session = root.join("data/among/session.json");
            if session.exists() {
                std::fs::remove_file(session).unwrap();
            }
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["app::notification_ui_tests::general_settings_persist_across_real_fresh_processes_and_write_failure", "--exact", "--ignored", "--nocapture", "--test-threads=1"])
                .env("AMONG_GENERAL_TEST_PHASE", phase).env("HOME", root.join("home"))
                .env("XDG_CONFIG_HOME", root.join("config")).env("XDG_DATA_HOME", root.join("data"))
                .env("XDG_CACHE_HOME", root.join("cache")).output().unwrap();
            std::fs::write(
                root.join(format!("{index}-{phase}.stdout.log")),
                &output.stdout,
            )
            .unwrap();
            std::fs::write(
                root.join(format!("{index}-{phase}.stderr.log")),
                &output.stderr,
            )
            .unwrap();
            assert!(
                output.status.success(),
                "phase {phase}: {}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            assert_eq!(std::fs::read(config.join("plugins.json")).unwrap(), legacy);
            assert_eq!(
                std::fs::read(config.join("managed-plugins.json")).unwrap(),
                managed
            );
            assert!(!root.join("data/among/notification-command-home").exists());
        }
        let final_document = crate::settings::load(
            &config.join("general.json"),
            crate::settings::Document::General,
        )
        .unwrap();
        assert_eq!(final_document["future"], original["future"]);
        assert_eq!(
            final_document["notifications"]["audio"],
            original["notifications"]["audio"]
        );
        assert_eq!(final_document["notifications"]["visual"]["future"], 7);
        assert_eq!(
            final_document["notifications"]["providers"]["origin-one/same"]["channels"]["delivery"]
                ["future"],
            7
        );
        println!("PASS actual General controls survive four fresh processes; unknown General/audio/provider data and legacy/managed plugin bytes preserved, unavailable choices identity-scoped, refused write remains session-only, inspection/restart creates no command cwd and replays no event; {}", root.display());
        return;
    }
    let (_app, ctx) = app();
    let backend = ctx.state.borrow().bells.clone().unwrap();
    assert_eq!(backend.event_count(), 0);
    let prefs = preferences(&ctx);
    let path = crate::settings::directory().unwrap().join("general.json");
    let original = std::fs::read(&path).unwrap();
    let visual: adw::SwitchRow = named(&prefs, "notification-visual-enabled");
    let a: adw::SwitchRow = named(
        &prefs,
        "notification-provider:origin-one/same:delivery-enabled",
    );
    let b: adw::SwitchRow = named(
        &prefs,
        "notification-provider:origin-two/same:delivery-enabled",
    );
    assert!(a.subtitle().unwrap().contains("Unavailable"));
    assert!(!b.is_active());
    match phase.as_deref().unwrap() {
        "edit" => {
            assert!(visual.is_active());
            toggle(&visual);
            toggle(&a);
            assert!(!backend.preferences().visual.enabled);
            assert!(a.is_active() && !b.is_active());
        }
        "restore" => {
            assert!(!visual.is_active());
            assert!(a.is_active() && !b.is_active());
            assert_eq!(
                std::fs::read(&path).unwrap(),
                original,
                "inspection saved defaults or normalized unknown settings"
            );
            assert_eq!(
                Preferences::parse(&serde_json::from_slice(&original).unwrap()).unwrap(),
                backend.preferences()
            );
        }
        "failure" => {
            let retained = path.with_file_name("general-retained.json");
            std::fs::rename(&path, &retained).unwrap();
            std::fs::create_dir(&path).unwrap();
            toggle(&visual);
            assert!(visual.is_active() && backend.preferences().visual.enabled);
            let result: adw::ActionRow = named(&prefs, "notification-save-status");
            assert!(result.subtitle().unwrap().contains("not saved locally"));
            assert_eq!(std::fs::read(&retained).unwrap(), original);
            std::fs::remove_dir(&path).unwrap();
            std::fs::rename(retained, &path).unwrap();
        }
        other => panic!("unknown phase {other}"),
    }
    prefs.destroy();
    ctx.window.destroy();
    pump_for(Duration::from_millis(100));
}

fn require_private_audio() {
    assert!(
        std::path::Path::new("/.dockerenv").exists() && !std::path::Path::new("/dev/snd").exists(),
        "audio UI acceptance requires an owned device-free container"
    );
    let socket = format!(
        "unix:{}/pulse.sock",
        std::env::var("XDG_RUNTIME_DIR").unwrap()
    );
    assert_eq!(std::env::var("PULSE_SERVER").unwrap(), socket);
    let output = |arguments: &[&str]| {
        let result = std::process::Command::new("pactl")
            .args(arguments)
            .output()
            .unwrap();
        assert!(result.status.success());
        String::from_utf8(result.stdout).unwrap()
    };
    let info = output(&["info"]);
    let sinks = output(&["list", "short", "sinks"]);
    let modules = output(&["list", "short", "modules"]);
    assert!(info.contains(&socket) && info.contains("Default Sink: among_audit"));
    assert_eq!(sinks.lines().count(), 1);
    assert!(sinks.contains("among_audit\tmodule-null-sink.c"));
    assert_eq!(modules.lines().count(), 2);
    assert!(
        modules.contains("module-native-protocol-unix") && modules.contains("module-null-sink")
    );
}

#[test]
#[ignore = "requires confirmed private Unix null sink in an owned device-free container, captured waveform, X11/WM and private XDG roots"]
fn general_audio_controls_and_normal_vte_route() {
    require_private_audio();
    let scenario = std::env::var("AMONG_GENERAL_AUDIO_SCENARIO").unwrap();
    let input = PathBuf::from(std::env::var_os("AMONG_GENERAL_AUDIO_WAV").unwrap());
    let (_app, ctx) = app();
    let backend = ctx.state.borrow().bells.clone().unwrap();
    let first = ctx.state.borrow().workspaces.active().focused;
    split_focused(&ctx);
    pump_for(Duration::from_millis(150));
    let prefs = preferences(&ctx);
    let audio: adw::SwitchRow = named(&prefs, "notification-audio-enabled");
    let preview: gtk::Button = named(&prefs, "notification-audio-preview");
    assert!(!audio.is_active());
    let general = crate::settings::directory().unwrap().join("general.json");
    assert!(!general.exists(), "audio inspection saved defaults");
    let events = backend.event_count();
    let unread = backend.attention(first.0).unread;
    match scenario.as_str() {
        "off" => {
            real_bell(&ctx, &backend, first);
            assert!(!audio.is_active());
            assert!(!general.exists());
        }
        "preview-off" => {
            key_button(&preview);
            assert_eq!(backend.event_count(), events);
            assert_eq!(backend.attention(first.0).unread, unread);
            assert!(
                !general.exists(),
                "preview changed policies or persisted defaults"
            );
        }
        "invalid-local"
        | "local-preview"
        | "stale-selection"
        | "event"
        | "background-ineligible"
        | "focus-revoke"
        | "remove"
        | "disable" => {
            let path: adw::EntryRow = named(&prefs, "notification-audio-path");
            let select: gtk::Button = named(&prefs, "notification-audio-select");
            type_entry(&path, input.to_str().unwrap());
            if scenario == "stale-selection" {
                // both real UI actions run before the async read completion;
                // the older callback must not persist over the newer builtin.
                select.emit_clicked();
                let builtin: gtk::Button = named(&prefs, "notification-audio-builtin");
                builtin.emit_clicked();
                pump_for(Duration::from_millis(350));
                assert_eq!(
                    backend.preferences().sound,
                    among::notifications::preferences::AudioSource::Builtin
                );
                assert_eq!(
                    crate::settings::load(&general, crate::settings::Document::General).unwrap()
                        ["notifications"]["audio"]["source"],
                    "builtin"
                );
                key_button(&preview);
            } else {
                key_button(&select);
                let result: adw::ActionRow = named(&prefs, "notification-save-status");
                wait(
                    || {
                        !result
                            .subtitle()
                            .unwrap()
                            .contains("Reading and validating")
                    },
                    "bounded asynchronous General WAV selection",
                );
                if scenario == "invalid-local" {
                    assert!(!general.exists());
                    assert_eq!(
                        backend.preferences().sound,
                        among::notifications::preferences::AudioSource::Builtin
                    );
                    assert!(!result.subtitle().unwrap().contains("Saved locally"));
                } else {
                    assert_eq!(
                        backend.preferences().sound,
                        among::notifications::preferences::AudioSource::Local(
                            input.to_str().unwrap().into()
                        )
                    );
                    assert_eq!(
                        crate::settings::load(&general, crate::settings::Document::General)
                            .unwrap()["notifications"]["audio"]["source"]["local"],
                        input.to_str().unwrap()
                    );
                    if scenario == "local-preview" {
                        // the decoder must retain admitted bytes, not reopen a
                        // pathname that changed after the UI accepted it.
                        std::fs::remove_file(&input).unwrap();
                        std::fs::write(&input, b"changed after accepted snapshot").unwrap();
                        key_button(&preview);
                        assert!(!audio.is_active() && backend.event_count() == events);
                    } else {
                        let condition: adw::ComboRow =
                            named(&prefs, "notification-audio-condition");
                        condition.set_selected(match scenario.as_str() {
                            "focus-revoke" => 1,
                            "background-ineligible" => 2,
                            _ => 0,
                        });
                        toggle(&audio);
                        assert!(audio.is_active());
                        if scenario == "background-ineligible" {
                            real_bell(&ctx, &backend, first);
                            assert!(
                                backend.focus().application_active
                                    && backend.focus().viewed.is_none()
                            );
                            assert!(!backend.audio_status().contains("Playback requested"));
                        } else {
                            prefs.destroy();
                            activate(&ctx.window);
                            real_bell(&ctx, &backend, first);
                            assert!(backend.attention(first.0).unread);
                            pump_for(Duration::from_secs(3));
                            match scenario.as_str() {
                                "focus-revoke" => {
                                    terminal(&ctx, first).grab_focus();
                                }
                                "remove" => close_pane(&ctx, first),
                                "disable" => {
                                    let prefs = preferences(&ctx);
                                    let audio: adw::SwitchRow =
                                        named(&prefs, "notification-audio-enabled");
                                    toggle(&audio);
                                    assert!(!audio.is_active());
                                    prefs.destroy();
                                }
                                _ => {}
                            }
                            pump_for(Duration::from_millis(100));
                            if scenario != "event" {
                                assert!(
                                    backend.audio_status().contains("cancelled"),
                                    "normal adapter did not cancel its own active source: {}",
                                    backend.audio_status()
                                );
                            }
                        }
                    }
                }
            }
        }
        _ => panic!("unknown owned audio scenario"),
    }
    pump_for(Duration::from_secs(4));
    assert!(
        !backend.audio_status().contains("unavailable"),
        "{}",
        backend.audio_status()
    );
    assert!(backend
        .with_host(|host| host.command_completions().next().is_none())
        .unwrap());
    let root = crate::test_isolation::check_root();
    std::fs::write(
        root.join("ui-observations.txt"),
        format!(
            "scenario={scenario} events={} audio_policy={:?} status={}\n",
            backend.event_count(),
            backend.preferences().audio,
            backend.audio_status()
        ),
    )
    .unwrap();
    std::fs::write(
        root.join("maps.txt"),
        std::fs::read("/proc/self/maps").unwrap(),
    )
    .unwrap();
    prefs.destroy();
    ctx.window.destroy();
    pump_for(Duration::from_millis(100));
    assert!(
        backend.preview().is_err(),
        "retired window still admitted explicit playback"
    );
    println!("PASS real General audio scenario {scenario}; requested/decoder status is NOT output proof: independently captured waveform/silence is required");
}
