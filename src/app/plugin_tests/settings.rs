use super::*;
use crate::plugin::host::Configuration;
use std::path::Path;

fn widgets(widget: &gtk::Widget) -> Vec<gtk::Widget> {
    let mut result = vec![widget.clone()];
    let mut child = widget.first_child();
    while let Some(widget) = child {
        result.extend(widgets(&widget));
        child = widget.next_sibling();
    }
    result
}

fn focus(window: &impl IsA<gtk::Window>) {
    let ids = xdo(&[
        "search",
        "--onlyvisible",
        "--pid",
        &std::process::id().to_string(),
        "--name",
        &window.as_ref().title().unwrap(),
    ]);
    let id = ids.lines().last().unwrap();
    xdo(&["windowactivate", "--sync", id]);
    xdo(&["windowfocus", "--sync", id]);
    wait_for(|| window.as_ref().is_active(), Duration::from_secs(2));
}

fn button(widget: &impl IsA<gtk::Widget>, label: &str) -> gtk::Button {
    widgets(widget.as_ref())
        .into_iter()
        .filter_map(|widget| widget.downcast::<gtk::Button>().ok())
        .find(|button| button.label().as_deref() == Some(label))
        .unwrap()
}

fn press(button: &gtk::Button) {
    wait_for(|| button.is_mapped(), Duration::from_secs(2));
    assert!(button.grab_focus());
    xdo(&["key", "--clearmodifiers", "space"]);
}

fn field(editor: &adw::PreferencesWindow, label: &str) -> adw::EntryRow {
    widgets(editor.upcast_ref())
        .into_iter()
        .filter_map(|widget| widget.downcast::<adw::EntryRow>().ok())
        .find(|field| field.title() == label)
        .unwrap()
}

fn edit(editor: &adw::PreferencesWindow, label: &str, text: &str) {
    let field = field(editor, label);
    wait_for(|| field.is_mapped(), Duration::from_secs(2));
    assert!(field.grab_focus());
    xdo(&["key", "--clearmodifiers", "ctrl+a"]);
    xdo(&["type", "--clearmodifiers", "--delay", "0", text]);
    wait_for(|| field.text() == text, Duration::from_secs(2));
}

fn open_editor(
    preferences: &adw::PreferencesWindow,
    row: &adw::ActionRow,
) -> adw::PreferencesWindow {
    focus(preferences);
    press(&button(row, "Settings…"));
    wait_for(
        || {
            gtk::Window::list_toplevels().iter().any(|window| {
                window
                    .downcast_ref::<adw::PreferencesWindow>()
                    .is_some_and(|window| window.title().as_deref() == Some("Plugin settings"))
            })
        },
        Duration::from_secs(2),
    );
    let editor = gtk::Window::list_toplevels()
        .into_iter()
        .filter_map(|window| window.downcast::<adw::PreferencesWindow>().ok())
        .find(|window| window.title().as_deref() == Some("Plugin settings"))
        .unwrap();
    wait_for(|| editor.is_mapped(), Duration::from_secs(2));
    focus(&editor);
    editor
}

fn transcript(package: &Path) -> Vec<serde_json::Value> {
    std::fs::read_to_string(package.join("transcript.jsonl"))
        .unwrap_or_default()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

fn unavailable_form_contract(malicious: bool) {
    use std::os::unix::fs::PermissionsExt;
    let profile = crate::test_isolation::check_root().join(format!(
        "unavailable-settings-{}",
        glib::uuid_string_random()
    ));
    std::env::set_var("XDG_DATA_HOME", profile.join("data"));
    std::env::set_var("XDG_CONFIG_HOME", profile.join("config"));
    let root = crate::plugin::inventory::root().unwrap();
    let mut receipt = crate::plugin::inventory::tests::fixture(&root);
    let package = receipt.package_path(&root);
    let mut manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(package.join("among-plugin.json")).unwrap()).unwrap();
    manifest["settings"]["fields"] = if malicious {
        serde_json::json!([{"key":"label","label":"Label","kind":"string","validator":"https://invalid.example/never-evaluate"}])
    } else {
        serde_json::json!([{"key":"token","label":"Required secret","kind":"secret","required":true,"default":"synthetic-secret-default"}])
    };
    let bytes = manifest.to_string();
    std::fs::write(package.join("among-plugin.json"), &bytes).unwrap();
    receipt.manifest_sha256 = crate::plugin::inventory::checksum(bytes.as_bytes()).unwrap();
    std::fs::write(
        root.join("installed.json"),
        serde_json::json!({"version":1,"installations":[receipt]}).to_string(),
    )
    .unwrap();
    let config = crate::settings::directory().unwrap();
    std::fs::create_dir_all(&config).unwrap();
    std::fs::set_permissions(&config, std::fs::Permissions::from_mode(0o700)).unwrap();
    let path = config.join("managed-plugins.json");
    let original =
        br#"{"version":1,"plugins":{"github:12/sample":{"enabled":false,"future":"keep"}}}"#;
    std::fs::write(&path, original).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    let app = application("com.github.among-terminal.unavailable-settings-test");
    let ctx = build_with_writer(&app, None, crate::session::Writer::new).unwrap();
    wait_for(|| ctx.state.borrow().session_ready, Duration::from_secs(4));
    open_preferences(&ctx);
    let preferences = gtk::Window::list_toplevels()
        .into_iter()
        .find_map(|window| window.downcast::<adw::PreferencesWindow>().ok())
        .unwrap();
    let row = widgets(preferences.upcast_ref())
        .into_iter()
        .filter_map(|widget| widget.downcast::<adw::ActionRow>().ok())
        .find(|row| row.title() == "Sample" || row.title() == "github:12/sample")
        .unwrap();
    let page = row
        .ancestor(adw::PreferencesPage::static_type())
        .unwrap()
        .downcast::<adw::PreferencesPage>()
        .unwrap();
    preferences.set_visible_page(&page);
    wait_for(|| row.is_mapped(), Duration::from_secs(2));
    if malicious {
        assert!(!widgets(row.upcast_ref()).iter().any(|widget| widget
            .downcast_ref::<gtk::Button>()
            .is_some_and(|button| button.label().as_deref() == Some("Settings…"))));
        assert!(row
            .subtitle()
            .is_some_and(|text| text.contains("invalid settings schema")));
    } else {
        assert!(row
            .subtitle()
            .is_some_and(|text| text.contains("required settings")));
        let editor = open_editor(&preferences, &row);
        assert!(!button(&editor, "Save").is_sensitive());
        assert!(!widgets(editor.upcast_ref())
            .iter()
            .any(|widget| widget.is::<adw::EntryRow>() || widget.is::<gtk::PasswordEntry>()));
        assert!(has_label(
            editor.upcast_ref(),
            "Unavailable; no plain-text substitute is supported"
        ));
        assert!(!has_label(editor.upcast_ref(), "synthetic-secret-default"));
        xdo(&["key", "--clearmodifiers", "alt+F4"]);
        wait_for(|| !editor.is_visible(), Duration::from_secs(2));
    }
    assert_eq!(std::fs::read(&path).unwrap(), original);
    assert_eq!(ctx.state.borrow().plugins.supervised_count(), 0);
    assert!(!root.join("executed").exists());
    println!("PASS native unavailable form (malicious schema: {malicious}): no operational secret/validator editor, no default or preference writes, and no process start");
    preferences.destroy();
    ctx.window.destroy();
}

#[test]
#[ignore = "requires an isolated graphical session, built fake-plugin and real keyboard input"]
fn typed_forms_optional_absence_validation_restart_and_fresh_process_are_real() {
    const PHASE: &str = "AMONG_TYPED_SETTINGS_TEST_PHASE";
    let phase = std::env::var(PHASE).ok();
    if phase.is_none() {
        for phase in [
            "edit-and-restart",
            "fresh-process",
            "edit-live",
            "required-secret",
            "malicious-schema",
        ] {
            let output=std::process::Command::new(std::env::current_exe().unwrap())
                .args(["app::plugin_tests::settings::typed_forms_optional_absence_validation_restart_and_fresh_process_are_real","--exact","--ignored","--test-threads=1","--nocapture"])
                .env(PHASE,phase).output().unwrap();
            assert!(
                output.status.success(),
                "phase {phase}: {} {}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            print!("{}", String::from_utf8_lossy(&output.stdout));
        }
        return;
    }
    if matches!(
        phase.as_deref(),
        Some("required-secret" | "malicious-schema")
    ) {
        unavailable_form_contract(phase.as_deref() == Some("malicious-schema"));
        return;
    }
    const KEY: &str = "github:12/sample";
    if phase.as_deref() == Some("edit-live") {
        let profile = crate::test_isolation::check_root().join(format!(
            "live-session-settings-{}",
            glib::uuid_string_random()
        ));
        std::env::set_var("XDG_DATA_HOME", profile.join("data"));
        std::env::set_var("XDG_CONFIG_HOME", profile.join("config"));
    }
    let root = crate::plugin::inventory::root().unwrap();
    crate::test_isolation::assert_below_check_root(&root);
    let config = crate::settings::directory()
        .unwrap()
        .join("managed-plugins.json");
    let package = if matches!(phase.as_deref(), Some("edit-and-restart" | "edit-live")) {
        use std::os::unix::fs::PermissionsExt;
        let mut receipt = crate::plugin::inventory::tests::fixture(&root);
        let package = receipt.package_path(&root);
        std::fs::copy(fixture("managed-echo").remove(0), package.join("plugin")).unwrap();
        std::fs::set_permissions(
            package.join("plugin"),
            std::fs::Permissions::from_mode(0o700),
        )
        .unwrap();
        std::fs::write(package.join("among-fake-mode"), b"managed-echo").unwrap();
        let mut manifest: serde_json::Value =
            serde_json::from_slice(&std::fs::read(package.join("among-plugin.json")).unwrap())
                .unwrap();
        manifest["required_features"] = serde_json::json!(["status", "settings"]);
        let apply = if phase.as_deref() == Some("edit-live") {
            "live"
        } else {
            "restart"
        };
        manifest["settings"] = serde_json::json!({"version":1,"apply":apply,"fields":[
            {"key":"toggle","label":"Toggle","kind":"boolean","default":false},
            {"key":"count","label":"Count","kind":"integer","min":1,"max":5,"default":1},
            {"key":"ratio","label":"Ratio","kind":"number","min":0,"max":1,"default":0.2},
            {"key":"label","label":"Label","kind":"string","max_length":12,"default":"default"},
            {"key":"choice","label":"Choice","kind":"enum","default":"first","choices":[{"value":"first","label":"First"},{"value":"second","label":"Second"}]},
            {"key":"unused","label":"Unset number","kind":"number","required":false},
            {"key":"unused_flag","label":"Unset flag","kind":"boolean","required":false},
            {"key":"unused_choice","label":"Unset choice","kind":"enum","required":false,"choices":[{"value":"first","label":"First"}]},
            {"key":"token","label":"Unsupported secret","kind":"secret","required":false,"default":"synthetic-secret-default"}]});
        let bytes = manifest.to_string();
        std::fs::write(package.join("among-plugin.json"), &bytes).unwrap();
        receipt.manifest_sha256 = crate::plugin::inventory::checksum(bytes.as_bytes()).unwrap();
        std::fs::write(
            root.join("installed.json"),
            serde_json::json!({"version":1,"installations":[receipt]}).to_string(),
        )
        .unwrap();
        std::fs::create_dir_all(config.parent().unwrap()).unwrap();
        std::fs::set_permissions(
            config.parent().unwrap(),
            std::fs::Permissions::from_mode(0o700),
        )
        .unwrap();
        std::fs::write(&config,serde_json::json!({"version":1,"plugins":{(KEY):{"enabled":false,"future":"keep","settings":{"version":1,"values":{"future":"opaque"}}}}}).to_string()).unwrap();
        std::fs::set_permissions(&config, std::fs::Permissions::from_mode(0o600)).unwrap();
        package
    } else {
        crate::plugin::inventory::read(&root).unwrap()[0]
            .receipt
            .package_path(&root)
    };
    let before = std::fs::read(&config).unwrap();
    let app = application("com.github.among-terminal.typed-settings-test");
    let ctx = build_with_writer(&app, None, crate::session::Writer::new).unwrap();
    wait_for(
        || ctx.state.borrow().session_ready || ctx.window.visible_dialog().is_some(),
        Duration::from_secs(4),
    );
    if !ctx.state.borrow().session_ready {
        let dialog = ctx.window.visible_dialog().unwrap();
        let fresh = button(&dialog, "Start new");
        wait_for(|| fresh.is_mapped(), Duration::from_secs(2));
        focus(&ctx.window);
        press(&fresh);
        wait_for(|| ctx.state.borrow().session_ready, Duration::from_secs(4));
    }
    let terminal = ctx
        .state
        .borrow()
        .pane(PaneId(1))
        .unwrap()
        .terminal()
        .clone();
    open_preferences(&ctx);
    let preferences = gtk::Window::list_toplevels()
        .into_iter()
        .find_map(|window| window.downcast::<adw::PreferencesWindow>().ok())
        .unwrap();
    let row = widgets(preferences.upcast_ref())
        .into_iter()
        .filter_map(|widget| widget.downcast::<adw::ActionRow>().ok())
        .find(|row| row.title() == "Sample")
        .unwrap();
    let page = row
        .ancestor(adw::PreferencesPage::static_type())
        .unwrap()
        .downcast::<adw::PreferencesPage>()
        .unwrap();
    preferences.set_visible_page(&page);
    wait_for(|| row.is_mapped(), Duration::from_secs(2));
    let editor = open_editor(&preferences, &row);
    assert_eq!(
        std::fs::read(&config).unwrap(),
        before,
        "opening a form never materializes defaults"
    );
    assert!(!widgets(editor.upcast_ref()).iter().any(|widget| widget
        .downcast_ref::<adw::EntryRow>()
        .is_some_and(|field| field.title() == "Unsupported secret")));
    assert!(has_label(
        editor.upcast_ref(),
        "Unavailable; no plain-text substitute is supported"
    ));
    let toggle = widgets(editor.upcast_ref())
        .into_iter()
        .find_map(|widget| {
            widget
                .downcast::<adw::SwitchRow>()
                .ok()
                .filter(|row| row.title() == "Toggle")
        })
        .unwrap();
    let choice = widgets(editor.upcast_ref())
        .into_iter()
        .find_map(|widget| {
            widget
                .downcast::<adw::ComboRow>()
                .ok()
                .filter(|row| row.title() == "Choice")
        })
        .unwrap();
    if phase.as_deref() == Some("fresh-process") {
        wait_for(
            || {
                ctx.state.borrow().plugins.configuration(KEY) == Configuration::Applied
                    && has_label(ctx.app_header.upcast_ref(), "restart edit")
            },
            Duration::from_secs(6),
        );
        assert!(toggle.is_active());
        assert_eq!(field(&editor, "Count").text(), "4");
        assert_eq!(field(&editor, "Ratio").text(), "0.75");
        assert_eq!(choice.selected(), 1);
        assert_eq!(field(&editor, "Label").text(), "restart edit");
        assert_eq!(field(&editor, "Unset number").text(), "");
        assert_eq!(std::fs::read(&config).unwrap(), before);
        println!("PASS fresh process restores all accepted field kinds and explicit grants, leaves optional absence/secret data unavailable and applies the saved restart revision without inspection writes");
        editor.destroy();
        preferences.destroy();
        ctx.window.destroy();
        wait_for(
            || ctx.state.borrow().plugins.is_stopped(),
            Duration::from_secs(2),
        );
        return;
    }
    assert_eq!(ctx.state.borrow().plugins.supervised_count(), 0);
    assert!(!package.join("executed").exists());
    assert!(toggle.grab_focus());
    xdo(&["key", "--clearmodifiers", "space"]);
    wait_for(|| toggle.is_active(), Duration::from_secs(2));
    edit(&editor, "Count", "6");
    edit(&editor, "Ratio", "0.75");
    edit(&editor, "Label", "edited");
    assert!(choice.grab_focus());
    xdo(&["key", "--clearmodifiers", "space"]);
    wait_for(
        || {
            widgets(choice.upcast_ref())
                .iter()
                .any(|widget| widget.is::<gtk::Popover>() && widget.is_mapped())
        },
        Duration::from_secs(2),
    );
    xdo(&["key", "--clearmodifiers", "End"]);
    xdo(&["key", "--clearmodifiers", "Return"]);
    wait_for(|| choice.selected() == 1, Duration::from_secs(2));
    press(&button(&editor, "Save"));
    wait_for(
        || {
            has_label(
                editor.upcast_ref(),
                "Setting count: value is outside the declared type or bounds",
            )
        },
        Duration::from_secs(2),
    );
    assert_eq!(std::fs::read(&config).unwrap(), before);
    assert!(!package.join("executed").exists());
    edit(&editor, "Count", "4");
    edit(&editor, "Ratio", "NaN");
    press(&button(&editor, "Save"));
    wait_for(
        || {
            has_label(
                editor.upcast_ref(),
                "Setting ratio: enter an accepted value of its declared type",
            )
        },
        Duration::from_secs(2),
    );
    assert_eq!(std::fs::read(&config).unwrap(), before);
    edit(&editor, "Ratio", "0.75");
    edit(&editor, "Label", "synthetic-value-too-long");
    press(&button(&editor, "Save"));
    wait_for(
        || {
            has_label(
                editor.upcast_ref(),
                "Setting label: value is outside the declared type or bounds",
            )
        },
        Duration::from_secs(2),
    );
    assert_eq!(std::fs::read(&config).unwrap(), before);
    edit(&editor, "Label", "edited");
    press(&button(&editor, "Save"));
    wait_for(
        || has_label(editor.upcast_ref(), "Saved locally"),
        Duration::from_secs(2),
    );
    let saved: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&config).unwrap()).unwrap();
    assert_eq!(
        saved["plugins"][KEY]["settings"]["values"],
        serde_json::json!({"toggle":true,"count":4,"ratio":0.75,"label":"edited","choice":"second","future":"opaque"})
    );
    assert_eq!(saved["plugins"][KEY]["future"], "keep");
    assert_eq!(saved["plugins"][KEY]["enabled"], false);
    assert_eq!(
        ctx.state.borrow().plugins.configuration(KEY),
        Configuration::NotApplied
    );
    let durable_before = std::fs::read(&config).unwrap();
    let backup = config.with_extension("previous");
    std::fs::rename(&config, &backup).unwrap();
    std::fs::create_dir(&config).unwrap();
    edit(&editor, "Label", "session edit");
    press(&button(&editor, "Save"));
    wait_for(
        || {
            has_label(
                editor.upcast_ref(),
                "Accepted for this session; not saved locally",
            )
        },
        Duration::from_secs(2),
    );
    assert_eq!(std::fs::read(&backup).unwrap(), durable_before);
    assert_eq!(
        ctx.state
            .borrow()
            .plugins
            .managed_details(KEY)
            .unwrap()
            .1
            .unwrap()["values"]["label"],
        "session edit"
    );
    assert!(ctx
        .state
        .borrow()
        .settings
        .status()
        .contains("session-only"));
    assert!(!ctx
        .state
        .borrow()
        .settings
        .status()
        .contains("session edit"));
    editor.destroy();
    let editor = open_editor(&preferences, &row);
    assert_eq!(field(&editor, "Label").text(), "session edit");
    std::fs::remove_dir(&config).unwrap();
    std::fs::rename(&backup, &config).unwrap();
    edit(&editor, "Ratio", "0.5");
    press(&button(&editor, "Save"));
    wait_for(
        || has_label(editor.upcast_ref(), "Saved locally"),
        Duration::from_secs(2),
    );
    let recovered: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&config).unwrap()).unwrap();
    assert_eq!(
        recovered["plugins"][KEY]["settings"]["values"]["label"],
        "session edit"
    );
    assert_eq!(
        recovered["plugins"][KEY]["settings"]["values"]["future"],
        "opaque"
    );
    assert!(ctx.state.borrow().settings.status().is_empty());
    ctx.state
        .borrow_mut()
        .settings
        .fail_next_managed_confirmation();
    edit(&editor, "Ratio", "0.75");
    press(&button(&editor, "Save"));
    wait_for(
        || {
            has_label(
                editor.upcast_ref(),
                "Written locally; durability not confirmed",
            )
        },
        Duration::from_secs(2),
    );
    let uncertain: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&config).unwrap()).unwrap();
    assert_eq!(
        uncertain["plugins"][KEY]["settings"]["values"]["ratio"],
        0.75
    );
    assert!(ctx
        .state
        .borrow()
        .settings
        .status()
        .contains("Durability uncertain"));
    assert_eq!(
        ctx.state.borrow().plugins.configuration(KEY),
        Configuration::NotApplied
    );
    press(&button(&editor, "Save"));
    wait_for(
        || has_label(editor.upcast_ref(), "Saved locally"),
        Duration::from_secs(2),
    );
    assert!(ctx.state.borrow().settings.status().is_empty());
    assert!(!package.join("executed").exists());
    println!("PASS actual storage refusal retains accepted session values across form reopening; repair plus Save preserves unknown data; injected post-rename confirmation failure shows uncertainty while actual file bytes are already changed, and explicit retry clears it without running disabled code");
    let before_review = std::fs::read(&config).unwrap();
    std::fs::rename(&config, &backup).unwrap();
    std::fs::create_dir(&config).unwrap();
    editor.destroy();
    focus(&preferences);
    let switch = widgets(row.upcast_ref())
        .into_iter()
        .find_map(|widget| widget.downcast::<gtk::Switch>().ok())
        .unwrap();
    assert!(switch.grab_focus());
    wait_for(|| switch.has_focus(), Duration::from_secs(2));
    xdo(&["key", "--clearmodifiers", "space"]);
    wait_for(
        || preferences.visible_dialog().is_some(),
        Duration::from_secs(2),
    );
    let review = preferences.visible_dialog().unwrap();
    press(&button(&review, "Accept and enable"));
    wait_for(
        || {
            ctx.state.borrow().plugins.configuration(KEY) == Configuration::Applied
                && has_label(ctx.app_header.upcast_ref(), "session edit")
        },
        Duration::from_secs(6),
    );
    assert!(ctx
        .state
        .borrow()
        .settings
        .status()
        .contains("session-only"));
    assert_eq!(std::fs::read(&backup).unwrap(), before_review);
    assert!(ctx.state.borrow().plugins.desired_enabled(KEY));
    std::fs::remove_dir(&config).unwrap();
    std::fs::rename(&backup, &config).unwrap();
    println!("PASS valid explicit first-enable consent starts the real managed generation for the session despite refused persistence; no grant is fabricated or silently saved");
    let running = ctx.state.borrow().plugins.running_generation(KEY).unwrap();
    let process = std::fs::read(package.join("executed")).unwrap();
    let applied = transcript(&package)
        .into_iter()
        .find(|frame| frame["type"] == "configure")
        .unwrap();
    assert_eq!(
        applied["values"],
        serde_json::json!({"toggle":true,"count":4,"ratio":0.75,"label":"session edit","choice":"second"})
    );
    if phase.as_deref() == Some("edit-live") {
        let editor = open_editor(&preferences, &row);
        let durable = std::fs::read(&config).unwrap();
        std::fs::rename(&config, &backup).unwrap();
        std::fs::create_dir(&config).unwrap();
        edit(&editor, "Label", "live session");
        press(&button(&editor, "Save"));
        wait_for(
            || {
                has_label(
                    editor.upcast_ref(),
                    "Accepted for this session; not saved locally",
                ) && ctx.state.borrow().plugins.configuration(KEY) == Configuration::Applied
                    && has_label(ctx.app_header.upcast_ref(), "live session")
            },
            Duration::from_secs(3),
        );
        assert_eq!(std::fs::read(&backup).unwrap(), durable);
        assert_eq!(
            ctx.state
                .borrow()
                .plugins
                .running_generation(KEY)
                .as_deref(),
            Some(running.as_str())
        );
        assert_eq!(std::fs::read(package.join("executed")).unwrap(), process);
        assert!(transcript(&package).iter().any(
            |frame| frame["type"] == "configure" && frame["values"]["label"] == "live session"
        ));
        editor.destroy();
        let editor = open_editor(&preferences, &row);
        assert_eq!(field(&editor, "Label").text(), "live session");
        std::fs::remove_dir(&config).unwrap();
        std::fs::rename(&backup, &config).unwrap();
        press(&button(&editor, "Save"));
        wait_for(
            || {
                has_label(editor.upcast_ref(), "Saved locally")
                    && ctx.state.borrow().plugins.configuration(KEY) == Configuration::Applied
            },
            Duration::from_secs(3),
        );
        let saved: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&config).unwrap()).unwrap();
        assert_eq!(
            saved["plugins"][KEY]["settings"]["values"]["label"],
            "live session"
        );
        assert_eq!(saved["plugins"][KEY]["enabled"], true);
        assert_eq!(saved["plugins"][KEY]["review"]["owner"], "example");
        assert!(ctx.state.borrow().settings.status().is_empty());
        assert_eq!(
            ctx.state
                .borrow()
                .plugins
                .running_generation(KEY)
                .as_deref(),
            Some(running.as_str())
        );
        assert_eq!(
            ctx.state.borrow().pane(PaneId(1)).unwrap().terminal(),
            &terminal
        );
        println!("PASS a session-only live Save changes actual configure traffic and rendered header without a process restart; reopening retains it and repaired saving persists pending grants, enabled state and values together");
        editor.destroy();
        preferences.destroy();
        ctx.window.destroy();
        wait_for(
            || ctx.state.borrow().plugins.is_stopped(),
            Duration::from_secs(2),
        );
        return;
    }
    let editor = open_editor(&preferences, &row);
    edit(&editor, "Label", "restart edit");
    press(&button(&editor, "Save"));
    wait_for(
        || ctx.state.borrow().plugins.configuration(KEY) == Configuration::RestartRequired,
        Duration::from_secs(2),
    );
    let retried: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&config).unwrap()).unwrap();
    assert_eq!(retried["plugins"][KEY]["enabled"], true);
    assert_eq!(retried["plugins"][KEY]["review"]["owner"], "example");
    assert_eq!(retried["plugins"][KEY]["future"], "keep");
    assert!(ctx.state.borrow().settings.status().is_empty());
    assert!(has_label(ctx.app_header.upcast_ref(), "session edit"));
    assert_eq!(std::fs::read(package.join("executed")).unwrap(), process);
    let restart = button(&editor, "Restart plugin…");
    wait_for(|| restart.is_mapped(), Duration::from_secs(2));
    press(&restart);
    wait_for(|| editor.visible_dialog().is_some(), Duration::from_secs(2));
    let dialog = editor.visible_dialog().unwrap();
    wait_for(
        || button(&dialog, "Cancel").is_mapped(),
        Duration::from_secs(2),
    );
    xdo(&["key", "--clearmodifiers", "Escape"]);
    wait_for(|| editor.visible_dialog().is_none(), Duration::from_secs(2));
    assert_eq!(
        ctx.state
            .borrow()
            .plugins
            .running_generation(KEY)
            .as_deref(),
        Some(running.as_str())
    );
    press(&restart);
    wait_for(|| editor.visible_dialog().is_some(), Duration::from_secs(2));
    let dialog = editor.visible_dialog().unwrap();
    // another explicit lifecycle operation can retire the generation while
    // this dialog remains open. its late approval must not restart the new one.
    assert!(ctx
        .state
        .borrow_mut()
        .plugins
        .restart_managed(KEY, &running));
    wait_for(
        || ctx.state.borrow().plugins.configuration(KEY) == Configuration::Applied,
        Duration::from_secs(6),
    );
    let replacement = ctx.state.borrow().plugins.running_generation(KEY).unwrap();
    assert_ne!(replacement, running);
    let replacement_process = std::fs::read(package.join("executed")).unwrap();
    let stored = ctx
        .state
        .borrow()
        .plugins
        .managed_details(KEY)
        .unwrap()
        .1
        .unwrap();
    ctx.state
        .borrow_mut()
        .plugins
        .stored_settings_changed(KEY, stored);
    assert_eq!(
        ctx.state.borrow().plugins.configuration(KEY),
        Configuration::RestartRequired
    );
    press(&button(&dialog, "Restart plugin"));
    wait_for(|| editor.visible_dialog().is_none(), Duration::from_secs(2));
    assert_eq!(
        ctx.state
            .borrow()
            .plugins
            .running_generation(KEY)
            .as_deref(),
        Some(replacement.as_str())
    );
    assert_eq!(
        std::fs::read(package.join("executed")).unwrap(),
        replacement_process
    );
    assert_eq!(
        ctx.state.borrow().plugins.configuration(KEY),
        Configuration::RestartRequired
    );
    println!("PASS an actual old restart dialog cannot restart a replacement process even when its new settings also require restart");
    press(&restart);
    wait_for(|| editor.visible_dialog().is_some(), Duration::from_secs(2));
    let dialog = editor.visible_dialog().unwrap();
    press(&button(&dialog, "Restart plugin"));
    wait_for(
        || {
            ctx.state.borrow().plugins.configuration(KEY) == Configuration::Applied
                && has_label(ctx.app_header.upcast_ref(), "restart edit")
        },
        Duration::from_secs(6),
    );
    assert_ne!(
        ctx.state
            .borrow()
            .plugins
            .running_generation(KEY)
            .as_deref(),
        Some(running.as_str())
    );
    assert_ne!(std::fs::read(package.join("executed")).unwrap(), process);
    assert_eq!(
        ctx.state.borrow().pane(PaneId(1)).unwrap().terminal(),
        &terminal
    );
    println!("PASS actual bool/integer/number/string/enum controls reject an invalid Save atomically, preserve unknown/unset/secret fields, save while disabled, send accepted typed values to the real child after explicit enable, and require Cancel/explicit confirmation before a generation-safe plugin-only restart");
    editor.destroy();
    preferences.destroy();
    ctx.window.destroy();
    wait_for(
        || ctx.state.borrow().plugins.is_stopped(),
        Duration::from_secs(2),
    );
}
