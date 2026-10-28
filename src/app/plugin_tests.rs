use super::*;
use crate::plugin::host::PluginState;

mod installation;
#[cfg(feature = "managed-transport-interop")]
mod resources;
mod settings;

#[track_caller]
fn wait_for(predicate: impl Fn() -> bool, timeout: Duration) {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        while glib::MainContext::default().pending() {
            glib::MainContext::default().iteration(false);
        }
        if predicate() {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "live plugin check timed out at {}",
            std::panic::Location::caller()
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn xdo(args: &[&str]) -> String {
    let output = std::process::Command::new("xdotool")
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

fn has_label(widget: &gtk::Widget, text: &str) -> bool {
    if widget
        .downcast_ref::<gtk::Label>()
        .is_some_and(|label| label.text() == text)
    {
        return true;
    }
    let mut child = widget.first_child();
    while let Some(widget) = child {
        if has_label(&widget, text) {
            return true;
        }
        child = widget.next_sibling();
    }
    false
}

fn mapped_labels(widget: &gtk::Widget, text: &str) -> usize {
    let mut count = usize::from(
        widget
            .downcast_ref::<gtk::Label>()
            .is_some_and(|label| label.text() == text && label.is_mapped()),
    );
    let mut child = widget.first_child();
    while let Some(widget) = child {
        count += mapped_labels(&widget, text);
        child = widget.next_sibling();
    }
    count
}

fn fixture(mode: &str) -> Vec<String> {
    let executable = std::env::current_exe().unwrap();
    let path = executable
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("examples/fake-plugin");
    assert!(
        path.is_file(),
        "build fake-plugin before filtered graphical tests"
    );
    vec![path.to_string_lossy().into_owned(), mode.into()]
}

fn application(id: &str) -> adw::Application {
    let config = std::env::var_os("XDG_CONFIG_HOME").unwrap();
    crate::test_isolation::assert_below_check_root(std::path::Path::new(&config));
    std::env::set_var("SHELL", "/bin/sh");
    let app = adw::Application::builder()
        .application_id(id)
        .flags(gio::ApplicationFlags::NON_UNIQUE)
        .build();
    app.register(None::<&gio::Cancellable>).unwrap();
    app
}

#[test]
#[ignore = "requires an isolated graphical session, built fake-plugin and real keyboard input"]
fn hostile_plugin_pipes_preserve_terminal_input_and_withdraw_sole_content() {
    let app = application("com.github.among-terminal.plugin-transport-test");
    let ctx = build_with_writer(&app, None, crate::session::Writer::new).unwrap();
    wait_for(|| ctx.state.borrow().session_ready, Duration::from_secs(5));
    ctx.state.borrow_mut().plugins.register(PluginSpec {
        name: "sole".into(),
        command: fixture("one-status"),
        enabled: true,
    });
    wait_for(
        || has_label(ctx.app_header.upcast_ref(), "vanishing"),
        Duration::from_secs(5),
    );
    ctx.state
        .borrow_mut()
        .plugins
        .broadcast_event(EventKind::WorkingDirectoryChanged, 1, None);
    wait_for(
        || !has_label(ctx.app_header.upcast_ref(), "vanishing"),
        Duration::from_secs(2),
    );
    assert!(ctx.state.borrow().store.is_empty(Scope::AppHeader));
    println!(
        "PASS sole faulted contributor is withdrawn from the real header without another result"
    );
    ctx.state.borrow_mut().plugins.disable("sole");

    ctx.state.borrow_mut().plugins.register(PluginSpec {
        name: "disabled".into(),
        command: fixture("echo"),
        enabled: true,
    });
    wait_for(
        || has_label(ctx.app_header.upcast_ref(), "ok"),
        Duration::from_secs(5),
    );
    ctx.state.borrow_mut().plugins.disable("disabled");
    wait_for(
        || !has_label(ctx.app_header.upcast_ref(), "ok"),
        Duration::from_secs(2),
    );
    assert!(ctx.state.borrow().store.is_empty(Scope::AppHeader));
    println!("PASS sole disabled contributor is withdrawn without a replacement plugin");

    let ids = xdo(&[
        "search",
        "--onlyvisible",
        "--pid",
        &std::process::id().to_string(),
        "--name",
        "Among Terminal",
    ]);
    let id = ids.lines().last().unwrap();
    xdo(&["windowactivate", "--sync", id]);
    xdo(&["windowfocus", "--sync", id]);
    xdo(&["key", "--clearmodifiers", "ctrl+shift+t"]);
    wait_for(
        || ctx.state.borrow().workspaces.active_index() == 1,
        Duration::from_secs(2),
    );
    ctx.state.borrow_mut().plugins.register(PluginSpec {
        name: "blocked".into(),
        command: fixture("never-read"),
        enabled: true,
    });
    wait_for(
        || {
            ctx.state
                .borrow()
                .plugins
                .listing()
                .iter()
                .any(|(name, state)| name == "blocked" && state.is_healthy())
        },
        Duration::from_secs(2),
    );
    let started = std::time::Instant::now();
    // two frames exceed even a 64-KiB page-rounded fixture pipe, without
    // racing the separate 16-frame overflow boundary.
    for _ in 0..2 {
        ctx.state.borrow_mut().plugins.broadcast_event(
            EventKind::WorkingDirectoryChanged,
            1,
            Some("x".repeat(60 * 1024)),
        );
    }
    xdo(&["key", "--clearmodifiers", "ctrl+Page_Up"]);
    wait_for(
        || ctx.state.borrow().workspaces.active_index() == 0,
        Duration::from_secs(1),
    );
    xdo(&[
        "type",
        "--clearmodifiers",
        "--delay",
        "0",
        "printf alive > \"$HOME/plugin-alive\"",
    ]);
    xdo(&["key", "--clearmodifiers", "Return"]);
    let output = std::path::PathBuf::from(std::env::var_os("HOME").unwrap()).join("plugin-alive");
    wait_for(|| output.exists(), Duration::from_secs(1));
    assert_eq!(std::fs::read_to_string(output).unwrap(), "alive");
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "typing did not precede the blocked-write deadline"
    );
    wait_for(
        || {
            ctx.state
                .borrow()
                .plugins
                .listing()
                .iter()
                .any(|(name, state)| {
                    if name != "blocked" {
                        return false;
                    }
                    if let PluginState::Stopping { reason, .. } | PluginState::Faulted { reason } =
                        state
                    {
                        assert!(
                            reason.contains("two seconds"),
                            "blocked child stopped for a different reason: {state:?}"
                        );
                    }
                    matches!(state,PluginState::Faulted{reason} if reason.contains("two seconds"))
                })
        },
        Duration::from_secs(4),
    );
    println!("PASS real workspace shortcut and terminal input while stdin is full; writer deadline contained the fixture");
    ctx.state.borrow_mut().plugins.register(PluginSpec {
        name: "shutdown".into(),
        command: fixture("stubborn"),
        enabled: true,
    });
    wait_for(
        || {
            ctx.state
                .borrow()
                .plugins
                .listing()
                .iter()
                .any(|(name, state)| name == "shutdown" && state.is_healthy())
        },
        Duration::from_secs(2),
    );
    let start = std::time::Instant::now();
    ctx.window.destroy();
    assert!(start.elapsed() < Duration::from_millis(100));
    assert!(!ctx.state.borrow().plugins.is_stopped());
    wait_for(
        || ctx.state.borrow().plugins.is_stopped(),
        Duration::from_secs(2),
    );
    println!(
        "PASS last-window destruction stays nonblocking and retains supervision until reaping"
    );
}

#[test]
#[ignore = "requires an isolated graphical session, built fake-plugin and real keyboard input"]
fn manager_search_same_name_origins_and_background_refresh_preserve_real_controls() {
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
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
    }
    let root = crate::plugin::inventory::root().unwrap();
    crate::test_isolation::assert_below_check_root(&root);
    let first = crate::plugin::inventory::tests::fixture(&root);
    let mut second = first.clone();
    second.repository_id = 13;
    second.owner = "another".into();
    let package = second.package_path(&root);
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&package)
        .unwrap();
    for name in ["among-plugin.json", "plugin"] {
        std::fs::copy(first.package_path(&root).join(name), package.join(name)).unwrap();
    }
    std::fs::set_permissions(
        package.join("plugin"),
        std::fs::Permissions::from_mode(0o700),
    )
    .unwrap();
    std::fs::write(
        root.join("installed.json"),
        serde_json::json!({"version":1,"installations":[first,second]}).to_string(),
    )
    .unwrap();
    let config = crate::settings::directory().unwrap();
    std::fs::create_dir_all(&config).unwrap();
    let local=serde_json::json!({"version":1,"plugins":{"github:12/sample":{"enabled":false},"github:13/sample":{"enabled":false,"future":"keep"}}}).to_string();
    std::fs::write(config.join("managed-plugins.json"), &local).unwrap();
    let legacy = r#"{"plugins":[{"name":"Lost","enabled":true}],"future":"keep"}"#;
    std::fs::write(config.join("plugins.json"), legacy).unwrap();
    let app = application("com.github.among-terminal.manager-inventory-test");
    let ctx = build_with_writer(&app, None, crate::session::Writer::new).unwrap();
    wait_for(
        || ctx.state.borrow().session_ready && ctx.state.borrow().plugins.supervised_count() == 0,
        Duration::from_secs(4),
    );
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
    let rows: Vec<_> = widgets(preferences.upcast_ref())
        .into_iter()
        .filter_map(|widget| widget.downcast::<adw::ActionRow>().ok())
        .filter(|row| row.title() == "Sample")
        .collect();
    assert_eq!(rows.len(), 2);
    let selected = rows
        .iter()
        .find(|row| {
            row.subtitle()
                .is_some_and(|text| text.contains("github:13/sample"))
        })
        .unwrap()
        .clone();
    let page = selected
        .ancestor(adw::PreferencesPage::static_type())
        .unwrap()
        .downcast::<adw::PreferencesPage>()
        .unwrap();
    preferences.set_visible_page(&page);
    wait_for(|| selected.is_mapped(), Duration::from_secs(2));
    focus(&preferences);
    let switch = widgets(selected.upcast_ref())
        .into_iter()
        .find_map(|widget| widget.downcast::<gtk::Switch>().ok())
        .unwrap();
    assert!(!switch.is_active());
    assert!(selected
        .subtitle()
        .is_some_and(|text| text.contains("Disabled") && text.contains("another/sample")));
    let details = widgets(selected.upcast_ref())
        .into_iter()
        .filter_map(|widget| widget.downcast::<gtk::Button>().ok())
        .find(|button| button.label().as_deref() == Some("Details…"))
        .unwrap();
    assert!(details.grab_focus());
    xdo(&["key", "--clearmodifiers", "space"]);
    wait_for(
        || {
            gtk::Window::list_toplevels().iter().any(|window| {
                window.downcast_ref::<gtk::Window>().is_some_and(|window| {
                    window.title().as_deref() == Some("Plugin details") && window.is_mapped()
                })
            })
        },
        Duration::from_secs(2),
    );
    let details = gtk::Window::list_toplevels()
        .into_iter()
        .find_map(|window| {
            window
                .downcast::<adw::PreferencesWindow>()
                .ok()
                .filter(|window| window.title().as_deref() == Some("Plugin details"))
        })
        .unwrap();
    assert!(has_label(
        details.upcast_ref(),
        "Declared API 1.2 supported"
    ));
    assert!(has_label(
        details.upcast_ref(),
        "Not observed in this session"
    ));
    assert!(has_label(details.upcast_ref(), "github:13/sample"));
    focus(&details);
    xdo(&["key", "--clearmodifiers", "alt+F4"]);
    wait_for(|| !details.is_visible(), Duration::from_secs(2));
    focus(&preferences);
    let search = widgets(page.upcast_ref())
        .into_iter()
        .find_map(|widget| widget.downcast::<gtk::SearchEntry>().ok())
        .unwrap();
    assert!(search.grab_focus());
    xdo(&["type", "--clearmodifiers", "--delay", "0", "13/sample"]);
    wait_for(
        || rows.iter().filter(|row| row.is_mapped()).count() == 1,
        Duration::from_secs(2),
    );
    assert!(selected.is_mapped());
    assert!(switch.grab_focus());
    let settings = widgets(selected.upcast_ref())
        .into_iter()
        .filter_map(|widget| widget.downcast::<gtk::Button>().ok())
        .find(|button| button.label().as_deref() == Some("Settings…"))
        .unwrap();
    assert!(settings.grab_focus());
    xdo(&["key", "--clearmodifiers", "space"]);
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
    let field = widgets(editor.upcast_ref())
        .into_iter()
        .find_map(|widget| widget.downcast::<adw::EntryRow>().ok())
        .unwrap();
    wait_for(|| field.is_mapped(), Duration::from_secs(2));
    assert!(field.grab_focus());
    xdo(&["key", "--clearmodifiers", "ctrl+a"]);
    xdo(&[
        "type",
        "--clearmodifiers",
        "--delay",
        "0",
        "unfinished edit",
    ]);
    wait_for(|| field.text() == "unfinished edit", Duration::from_secs(2));
    ctx.state.borrow_mut().plugins.register(PluginSpec {
        name: "controlled-crash".into(),
        command: fixture("crashing"),
        enabled: true,
    });
    wait_for(
        || {
            ctx.state
                .borrow()
                .plugins
                .listing()
                .iter()
                .any(|(key, state)| {
                    key == "controlled-crash"
                        && matches!(
                            state,
                            PluginState::Stopping { .. } | PluginState::Faulted { .. }
                        )
                })
        },
        Duration::from_secs(3),
    );
    wait_for(
        || {
            widgets(page.upcast_ref()).iter().any(|widget| {
                widget.downcast_ref::<adw::ActionRow>().is_some_and(|row| {
                    row.title() == "controlled-crash"
                        && row.subtitle().is_some_and(|text| {
                            text.contains("Stopped") || text.contains("Stopping")
                        })
                })
            })
        },
        Duration::from_secs(2),
    );
    assert_eq!(field.text(), "unfinished edit");
    assert!(field
        .root()
        .and_then(|root| root.focus())
        .is_some_and(
            |focus| focus == field.clone().upcast::<gtk::Widget>() || focus.is_ancestor(&field)
        ));
    let retained_switch = widgets(selected.upcast_ref())
        .into_iter()
        .find_map(|widget| widget.downcast::<gtk::Switch>().ok())
        .unwrap();
    assert_eq!(retained_switch, switch);
    xdo(&["key", "--clearmodifiers", "alt+F4"]);
    wait_for(|| !editor.is_visible(), Duration::from_secs(2));
    focus(&preferences);
    assert!(search.grab_focus());
    xdo(&["key", "--clearmodifiers", "ctrl+a"]);
    xdo(&["key", "--clearmodifiers", "BackSpace"]);
    wait_for(
        || rows.iter().filter(|row| row.is_mapped()).count() == 2,
        Duration::from_secs(2),
    );
    let missing = widgets(page.upcast_ref())
        .into_iter()
        .filter_map(|widget| widget.downcast::<adw::ActionRow>().ok())
        .find(|row| row.title() == "Lost")
        .unwrap();
    assert!(missing
        .subtitle()
        .is_some_and(|text| text.contains("Files unavailable") && text.contains("Not installed")));
    assert!(!root.join("executed").exists());
    assert_eq!(
        std::fs::read_to_string(config.join("managed-plugins.json")).unwrap(),
        local
    );
    assert_eq!(
        std::fs::read_to_string(config.join("plugins.json")).unwrap(),
        legacy
    );
    assert_eq!(
        ctx.state.borrow().pane(PaneId(1)).unwrap().terminal(),
        &terminal
    );
    println!("PASS identity-scoped native search distinguishes two Sample origins; missing desired-on legacy entry remains visible; a real background process fault adds/updates its card without replacing the selected switch, stealing editor focus, losing unfinished text or writing inspected settings");
    let missing_switch = widgets(missing.upcast_ref())
        .into_iter()
        .find_map(|widget| widget.downcast::<gtk::Switch>().ok())
        .unwrap();
    assert!(missing_switch.is_active() && missing_switch.is_sensitive());
    assert!(missing_switch.grab_focus());
    xdo(&["key", "--clearmodifiers", "space"]);
    wait_for(
        || !ctx.state.borrow().plugins.desired_enabled("Lost"),
        Duration::from_secs(2),
    );
    let saved: serde_json::Value =
        serde_json::from_slice(&std::fs::read(config.join("plugins.json")).unwrap()).unwrap();
    assert_eq!(saved["plugins"][0]["enabled"], false);
    assert_eq!(saved["future"], "keep");
    assert_eq!(
        std::fs::read_to_string(config.join("managed-plugins.json")).unwrap(),
        local
    );
    println!("PASS real Space disables and durably saves the unavailable desired-on legacy entry without changing either equal-name managed identity or unrelated fields");
    preferences.destroy();
    ctx.window.destroy();
    wait_for(
        || ctx.state.borrow().plugins.is_stopped(),
        Duration::from_secs(2),
    );
}

#[test]
#[ignore = "requires an isolated graphical session, built fake-plugin and real keyboard input"]
fn managed_access_review_runtime_settings_and_fresh_process_grants_use_real_controls() {
    const PHASE: &str = "AMONG_MANAGED_RUNTIME_TEST_PHASE";
    let phase = std::env::var(PHASE).ok();
    if phase.is_none() {
        for phase in ["review-and-edit", "fresh-process"] {
            if phase == "fresh-process" {
                // a supervised stop can interrupt the fixture's diagnostic
                // file mid-frame; that must not hide the next process's hello.
                use std::io::Write;
                let root = crate::plugin::inventory::root().unwrap();
                let package = crate::plugin::inventory::read(&root).unwrap()[0]
                    .receipt
                    .package_path(&root);
                std::fs::OpenOptions::new()
                    .append(true)
                    .open(package.join("transcript.jsonl"))
                    .unwrap()
                    .write_all(br#"{"type":"interrupted"#)
                    .unwrap();
            }
            let output=std::process::Command::new(std::env::current_exe().unwrap())
                .args(["app::plugin_tests::managed_access_review_runtime_settings_and_fresh_process_grants_use_real_controls","--exact","--ignored","--test-threads=1","--nocapture"])
                .env(PHASE,phase).output().unwrap();
            assert!(
                output.status.success(),
                "phase {phase}: {} {}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            print!("{}", String::from_utf8_lossy(&output.stdout));
            println!("PASS managed native phase {phase}: actual fresh process exited successfully");
        }
        return;
    }
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
        let title = format!("^{}$", window.as_ref().title().unwrap());
        let ids = xdo(&[
            "search",
            "--onlyvisible",
            "--pid",
            &std::process::id().to_string(),
            "--name",
            &title,
        ]);
        let id = ids.lines().last().unwrap();
        xdo(&["windowactivate", "--sync", id]);
        xdo(&["windowfocus", "--sync", id]);
        wait_for(|| window.as_ref().is_active(), Duration::from_secs(2));
    }
    fn press(button: &gtk::Button) {
        wait_for(|| button.is_mapped(), Duration::from_secs(2));
        assert!(button.grab_focus());
        xdo(&["key", "--clearmodifiers", "space"]);
        wait_for(
            || !glib::MainContext::default().pending(),
            Duration::from_secs(2),
        );
    }
    fn frames(package: &std::path::Path) -> Vec<serde_json::Value> {
        std::fs::read(package.join("transcript.jsonl"))
            .unwrap_or_default()
            .split(|byte| *byte == b'\n')
            .filter_map(|line| serde_json::from_slice(line).ok())
            .collect()
    }
    let root = crate::plugin::inventory::root().unwrap();
    crate::test_isolation::assert_below_check_root(&root);
    let config = crate::settings::directory()
        .unwrap()
        .join("managed-plugins.json");
    let package = if phase.as_deref() == Some("review-and-edit") {
        let mut receipt = crate::plugin::inventory::tests::fixture(&root);
        let package = receipt.package_path(&root);
        std::fs::copy(fixture("managed-echo").remove(0), package.join("plugin")).unwrap();
        std::fs::set_permissions(
            package.join("plugin"),
            std::os::unix::fs::PermissionsExt::from_mode(0o700),
        )
        .unwrap();
        std::fs::write(package.join("among-fake-mode"), "managed-on-event").unwrap();
        let mut manifest: serde_json::Value =
            serde_json::from_slice(&std::fs::read(package.join("among-plugin.json")).unwrap())
                .unwrap();
        manifest["required_features"] = serde_json::json!(["status", "settings"]);
        manifest["optional_features"] = serde_json::json!([
            "context.terminals",
            "context.cwd",
            "context.title",
            "context.process",
            "events.terminal"
        ]);
        let bytes = manifest.to_string();
        std::fs::write(package.join("among-plugin.json"), &bytes).unwrap();
        receipt.manifest_sha256 = crate::plugin::inventory::checksum(bytes.as_bytes()).unwrap();
        std::fs::write(
            root.join("installed.json"),
            serde_json::json!({"version":1,"installations":[receipt]}).to_string(),
        )
        .unwrap();
        package
    } else {
        crate::plugin::inventory::read(&root).unwrap()[0]
            .receipt
            .package_path(&root)
    };
    let previous = std::fs::read(&config).ok();
    let app = application("com.github.among-terminal.managed-runtime-test");
    let ctx = build_with_writer(&app, None, crate::session::Writer::new).unwrap();
    // a saved layout may offer restoration in the second real process; the test
    // explicitly chooses fresh shells rather than treating saved focus as input.
    wait_for(
        || ctx.state.borrow().session_ready || ctx.window.visible_dialog().is_some(),
        Duration::from_secs(5),
    );
    if !ctx.state.borrow().session_ready {
        let dialog = ctx
            .window
            .visible_dialog()
            .unwrap()
            .downcast::<adw::AlertDialog>()
            .unwrap();
        assert!(dialog
            .heading()
            .is_some_and(|heading| heading.contains("Restore")));
        let fresh = widgets(dialog.upcast_ref())
            .into_iter()
            .filter_map(|widget| widget.downcast::<gtk::Button>().ok())
            .find(|button| button.label().as_deref() == Some("Start new"))
            .unwrap();
        wait_for(|| fresh.is_mapped(), Duration::from_secs(2));
        focus(&ctx.window);
        press(&fresh);
        wait_for(|| ctx.state.borrow().session_ready, Duration::from_secs(5));
    }
    if phase.as_deref() == Some("fresh-process") {
        wait_for(
            || {
                ctx.state.borrow().plugins.configuration("github:12/sample")
                    == crate::plugin::host::Configuration::Applied
                    && has_label(ctx.app_header.upcast_ref(), "runtime edit")
            },
            Duration::from_secs(6),
        );
        assert_eq!(
            previous,
            std::fs::read(&config).ok(),
            "startup does not rewrite accepted settings/grants"
        );
        let hellos: Vec<_> = frames(&package)
            .into_iter()
            .filter(|frame| frame["type"] == "managed_hello")
            .collect();
        assert!(hellos.len() >= 2);
        assert_ne!(hellos[0]["session"], hellos.last().unwrap()["session"]);
        assert_eq!(
            hellos.last().unwrap()["features"],
            serde_json::json!(["context.cwd", "context.terminals", "settings", "status"])
        );
        println!("PASS fresh process: retained publisher/grants/settings admit the real plugin with a new session; accepted values reach the actual header without inspection writes");
        ctx.window.destroy();
        wait_for(
            || ctx.state.borrow().plugins.is_stopped(),
            Duration::from_secs(2),
        );
        return;
    }
    assert_eq!(ctx.state.borrow().plugins.supervised_count(), 0);
    assert!(!package.join("executed").exists());
    open_preferences(&ctx);
    let preferences = gtk::Window::list_toplevels()
        .into_iter()
        .find_map(|window| window.downcast::<adw::PreferencesWindow>().ok())
        .unwrap();
    preferences.set_title(Some("Primary Preferences"));
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
    let switch = widgets(row.upcast_ref())
        .into_iter()
        .find_map(|widget| widget.downcast::<gtk::Switch>().ok())
        .unwrap();
    wait_for(|| switch.is_mapped(), Duration::from_secs(2));
    focus(&preferences);
    assert!(switch.grab_focus());
    xdo(&["key", "--clearmodifiers", "space"]);
    wait_for(
        || preferences.visible_dialog().is_some(),
        Duration::from_secs(2),
    );
    let cancelled = preferences.visible_dialog().unwrap();
    wait_for(
        || {
            widgets(cancelled.upcast_ref()).iter().any(|widget| {
                widget.is_mapped()
                    && widget
                        .downcast_ref::<gtk::Button>()
                        .is_some_and(|button| button.label().as_deref() == Some("Cancel"))
            })
        },
        Duration::from_secs(2),
    );
    wait_for(
        || {
            cancelled
                .root()
                .and_then(|root| root.focus())
                .is_some_and(|focus| focus.is_ancestor(&cancelled))
        },
        Duration::from_secs(2),
    );
    xdo(&["key", "--clearmodifiers", "Escape"]);
    wait_for(
        || preferences.visible_dialog().is_none(),
        Duration::from_secs(2),
    );
    assert!(!package.join("executed").exists());
    assert_eq!(previous, std::fs::read(&config).ok());
    wait_for(|| !switch.is_active(), Duration::from_secs(2));
    assert!(switch.grab_focus());
    xdo(&["key", "--clearmodifiers", "space"]);
    wait_for(
        || preferences.visible_dialog().is_some(),
        Duration::from_secs(2),
    );
    let dialog = preferences
        .visible_dialog()
        .unwrap()
        .downcast::<adw::AlertDialog>()
        .unwrap();
    assert!(dialog
        .heading()
        .is_some_and(|heading| heading.contains("Review")));
    assert!(!package.join("executed").exists());
    assert_eq!(
        previous,
        std::fs::read(&config).ok(),
        "opening access review does not grant, write or execute"
    );
    for feature in ["context.terminals", "context.cwd"] {
        let control = widgets(dialog.upcast_ref())
            .into_iter()
            .filter_map(|widget| widget.downcast::<gtk::CheckButton>().ok())
            .find(|control| control.label().as_deref() == Some(feature))
            .unwrap();
        wait_for(|| control.is_mapped(), Duration::from_secs(2));
        assert!(!control.is_active());
        for _ in 0..16 {
            if control.has_focus() {
                break;
            }
            xdo(&["key", "--clearmodifiers", "Tab"]);
            wait_for(
                || !glib::MainContext::default().pending(),
                Duration::from_secs(2),
            );
        }
        assert!(
            control.has_focus(),
            "optional grant must be keyboard reachable: {feature}"
        );
        xdo(&["key", "--clearmodifiers", "space"]);
        wait_for(|| control.is_active(), Duration::from_secs(2));
    }
    let accept = widgets(dialog.upcast_ref())
        .into_iter()
        .filter_map(|widget| widget.downcast::<gtk::Button>().ok())
        .find(|button| button.label().as_deref() == Some("Accept and enable"))
        .unwrap();
    press(&accept);
    wait_for(
        || {
            ctx.state.borrow().plugins.configuration("github:12/sample")
                == crate::plugin::host::Configuration::Applied
                && has_label(ctx.app_header.upcast_ref(), "default")
        },
        Duration::from_secs(6),
    );
    assert!(package.join("executed").exists());
    let accepted = std::fs::read(&config).unwrap();
    // replay a retained native review's late callback; the first Cancel above
    // and this generation's current Accept were driven by real keyboard input.
    cancelled.emit_by_name::<()>("response", &[&"enable"]);
    assert_eq!(
        std::fs::read(&config).unwrap(),
        accepted,
        "a stale review cannot persist grants that the running generation rejected"
    );
    let explanation = preferences
        .visible_dialog()
        .unwrap()
        .downcast::<adw::AlertDialog>()
        .unwrap();
    assert!(explanation
        .body()
        .contains("stop the current plugin generation"));
    let close = widgets(explanation.upcast_ref())
        .into_iter()
        .filter_map(|widget| widget.downcast::<gtk::Button>().ok())
        .find(|button| button.label().as_deref() == Some("Close"))
        .unwrap();
    press(&close);
    println!("PASS a retained native access review cannot persist reduced grants through a late callback after real keyboard acceptance starts the current generation; rejection is visible and retained bytes stay unchanged");
    let terminal = ctx
        .state
        .borrow()
        .pane(PaneId(1))
        .unwrap()
        .terminal()
        .clone();
    let settings = widgets(row.upcast_ref())
        .into_iter()
        .filter_map(|widget| widget.downcast::<gtk::Button>().ok())
        .find(|button| button.label().as_deref() == Some("Settings…"))
        .unwrap();
    focus(&preferences);
    press(&settings);
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
    let field = widgets(editor.upcast_ref())
        .into_iter()
        .find_map(|widget| widget.downcast::<adw::EntryRow>().ok())
        .unwrap();
    assert!(field.grab_focus());
    xdo(&["key", "--clearmodifiers", "ctrl+a"]);
    xdo(&["type", "--clearmodifiers", "--delay", "0", "runtime edit"]);
    wait_for(|| field.text() == "runtime edit", Duration::from_secs(2));
    let save = widgets(editor.upcast_ref())
        .into_iter()
        .filter_map(|widget| widget.downcast::<gtk::Button>().ok())
        .find(|button| button.label().as_deref() == Some("Save"))
        .unwrap();
    press(&save);
    wait_for(
        || {
            ctx.state.borrow().plugins.configuration("github:12/sample")
                == crate::plugin::host::Configuration::Applied
                && has_label(ctx.app_header.upcast_ref(), "runtime edit")
        },
        Duration::from_secs(6),
    );
    wait_for(
        || has_label(editor.upcast_ref(), "applied by the current process"),
        Duration::from_secs(2),
    );
    assert_eq!(
        ctx.state.borrow().pane(PaneId(1)).unwrap().terminal(),
        &terminal
    );
    editor.destroy();
    preferences.destroy();
    focus(&ctx.window);
    apply_focus(&ctx);
    wait_for(|| terminal.has_focus(), Duration::from_secs(2));
    xdo(&[
        "type",
        "--clearmodifiers",
        "--delay",
        "0",
        "printf '\\033]2;Managed target\\007' ; printf ready > \"$HOME/managed-input-proof\"",
    ]);
    xdo(&["key", "--clearmodifiers", "Return"]);
    let proof =
        std::path::PathBuf::from(std::env::var_os("HOME").unwrap()).join("managed-input-proof");
    wait_for(|| proof.exists(), Duration::from_secs(2));
    wait_for(
        || {
            frames(&package).iter().any(|frame| {
                frame["type"] == "status"
                    && frame["context"]["terminals"]
                        .as_array()
                        .is_some_and(|terminals| !terminals.is_empty())
            })
        },
        Duration::from_secs(3),
    );
    for frame in frames(&package)
        .into_iter()
        .filter(|frame| frame["type"] == "status")
    {
        for terminal in frame["context"]["terminals"]
            .as_array()
            .into_iter()
            .flatten()
        {
            assert!(terminal.get("title").is_none());
            assert!(terminal.get("running").is_none());
            assert!(terminal.get("cwd").is_some());
        }
    }
    let original = ctx.state.borrow().workspaces.active().reference;
    xdo(&["key", "--clearmodifiers", "ctrl+shift+n"]);
    wait_for(
        || ctx.state.borrow().workspaces.active().panes().len() == 2,
        Duration::from_secs(2),
    );
    assert!(terminal.grab_focus());
    wait_for(
        || ctx.state.borrow().workspaces.active().focused == PaneId(1),
        Duration::from_secs(2),
    );
    xdo(&["key", "--clearmodifiers", "ctrl+shift+w"]);
    wait_for(
        || ctx.state.borrow().pane(PaneId(1)).is_none(),
        Duration::from_secs(2),
    );
    assert_eq!(ctx.state.borrow().workspaces.active().reference, original);
    xdo(&["key", "--clearmodifiers", "ctrl+shift+t"]);
    wait_for(
        || ctx.state.borrow().workspaces.len() == 2,
        Duration::from_secs(2),
    );
    let survivor = ctx.state.borrow().workspaces.active().reference;
    let surviving_terminal = ctx
        .state
        .borrow()
        .pane(ctx.state.borrow().workspaces.active().focused)
        .unwrap()
        .terminal()
        .clone();
    std::fs::write(package.join("target-labels"), b"yes").unwrap();
    let original_label = format!("workspace-{original}");
    let survivor_label = format!("workspace-{survivor}");
    // a summary can have just refreshed before the fixture changes. waiting
    // exactly one interval races the next production tick and child I/O.
    let summary_wait = Duration::from_millis(SUMMARY_REFRESH_MS + 2_000);
    wait_for(
        || {
            has_label(ctx.workspace_list.upcast_ref(), &original_label)
                && has_label(ctx.workspace_list.upcast_ref(), &survivor_label)
        },
        summary_wait,
    );
    std::fs::write(package.join("hold-summary"), original.to_string()).unwrap();
    wait_for(|| package.join("status-held").exists(), summary_wait);
    xdo(&["key", "--clearmodifiers", "alt+1"]);
    wait_for(
        || ctx.state.borrow().workspaces.active().reference == original,
        Duration::from_secs(2),
    );
    xdo(&["key", "--clearmodifiers", "ctrl+shift+w"]);
    wait_for(
        || ctx.state.borrow().workspaces.len() == 1,
        Duration::from_secs(2),
    );
    assert_eq!(ctx.state.borrow().workspaces.active().reference, survivor);
    assert!(!has_label(ctx.workspace_list.upcast_ref(), &original_label));
    assert!(
        has_label(ctx.workspace_list.upcast_ref(), &survivor_label),
        "cached content follows the surviving target before any new response"
    );
    let stale_seen = Rc::new(std::cell::Cell::new(false));
    let seen = stale_seen.clone();
    let sidebar = ctx.workspace_list.clone();
    let observation = glib::timeout_add_local(Duration::from_millis(5), move || {
        if has_label(sidebar.upcast_ref(), "retired-target-marker") {
            seen.set(true);
        }
        glib::ControlFlow::Continue
    });
    let before_release = frames(&package).len();
    let survivor_reference = survivor.to_string();
    std::fs::write(package.join("allow-status"), b"yes").unwrap();
    wait_for(
        || package.join("summary-released").exists(),
        Duration::from_secs(2),
    );
    wait_for(
        || {
            frames(&package).iter().skip(before_release).any(|frame| {
                frame["type"] == "status"
                    && frame["scope"]["slot"] == "workspace_summary"
                    && frame["scope"]["target"]["reference"].as_str()
                        == Some(survivor_reference.as_str())
            }) && has_label(ctx.workspace_list.upcast_ref(), &survivor_label)
        },
        summary_wait,
    );
    observation.remove();
    std::fs::remove_file(package.join("hold-summary")).unwrap();
    assert!(
        !stale_seen.get(),
        "old summary must never appear on the replacement row"
    );
    assert_eq!(
        ctx.state
            .borrow()
            .pane(ctx.state.borrow().workspaces.active().focused)
            .unwrap()
            .terminal(),
        &surviving_terminal
    );
    println!("PASS real keyboard split/close preserves the birth-workspace reference; closing it and shifting its sibling to index zero discards the held old-target reply without replacing the surviving terminal");
    println!("PASS actual native access review and optional grant controls precede execution; live Save/applied acknowledgement changes the real header, preserves the original terminal and allows real input without ungranted title/process disclosure");
    ctx.window.destroy();
    wait_for(
        || ctx.state.borrow().plugins.is_stopped(),
        Duration::from_secs(2),
    );
}

#[test]
#[ignore = "requires an isolated graphical session, built fake-plugin and real keyboard input"]
fn managed_metadata_and_disabled_settings_are_inert_and_persist_real_edits() {
    fn descendants(widget: &gtk::Widget) -> Vec<gtk::Widget> {
        let mut result = vec![widget.clone()];
        let mut child = widget.first_child();
        while let Some(widget) = child {
            result.extend(descendants(&widget));
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
    }
    fn press(button: &gtk::Button) {
        assert!(button.grab_focus());
        xdo(&["key", "--clearmodifiers", "space"]);
        wait_for(
            || !glib::MainContext::default().pending(),
            Duration::from_secs(2),
        );
    }
    fn settings_window() -> adw::PreferencesWindow {
        gtk::Window::list_toplevels()
            .into_iter()
            .filter_map(|window| window.downcast::<adw::PreferencesWindow>().ok())
            .find(|window| window.title().as_deref() == Some("Plugin settings"))
            .unwrap()
    }
    let root = crate::plugin::inventory::root().unwrap();
    crate::test_isolation::assert_below_check_root(&root);
    let receipt = crate::plugin::inventory::tests::fixture(&root);
    let config = crate::settings::directory().unwrap();
    std::fs::create_dir_all(&config).unwrap();
    std::fs::set_permissions(&config, std::os::unix::fs::PermissionsExt::from_mode(0o700)).unwrap();
    let legacy = b"{\"plugins\":[],\"unrelated\":\"preserved\"}";
    std::fs::write(config.join("plugins.json"), legacy).unwrap();
    let local = serde_json::json!({"version":1,"plugins":{"github:12/sample":{"enabled":true,"future":"keep",
        "settings":{"version":1,"values":{"future":"opaque"}}}}});
    let local_path = config.join("managed-plugins.json");
    std::fs::write(&local_path, local.to_string()).unwrap();
    std::fs::set_permissions(
        &local_path,
        std::os::unix::fs::PermissionsExt::from_mode(0o600),
    )
    .unwrap();
    let before = std::fs::read(&local_path).unwrap();
    let manifest_before =
        std::fs::read(receipt.package_path(&root).join("among-plugin.json")).unwrap();
    let app = application("com.github.among-terminal.managed-inspection-test");
    let ctx = build_with_writer(&app, None, crate::session::Writer::new).unwrap();
    wait_for(|| ctx.state.borrow().session_ready, Duration::from_secs(5));
    assert!(ctx
        .state
        .borrow()
        .plugins
        .desired_enabled("github:12/sample"));
    assert_eq!(ctx.state.borrow().plugins.supervised_count(), 0);
    assert!(!root.join("executed").exists());
    open_preferences(&ctx);
    let preferences = gtk::Window::list_toplevels()
        .into_iter()
        .find_map(|window| window.downcast::<adw::PreferencesWindow>().ok())
        .unwrap();
    let row = descendants(preferences.upcast_ref())
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
    focus(&preferences);
    let switch = descendants(row.upcast_ref())
        .into_iter()
        .find_map(|widget| widget.downcast::<gtk::Switch>().ok())
        .unwrap();
    assert!(
        switch.is_active() && switch.is_sensitive(),
        "incompatible desired-on entries must always be switchable off"
    );
    assert_eq!(
        before,
        std::fs::read(&local_path).unwrap(),
        "opening metadata must not materialize defaults"
    );
    assert!(switch.grab_focus());
    xdo(&["key", "--clearmodifiers", "space"]);
    wait_for(
        || {
            !ctx.state
                .borrow()
                .plugins
                .desired_enabled("github:12/sample")
        },
        Duration::from_secs(2),
    );
    let after_disable = std::fs::read(&local_path).unwrap();
    let settings = descendants(row.upcast_ref())
        .into_iter()
        .filter_map(|widget| widget.downcast::<gtk::Button>().ok())
        .find(|button| button.label().as_deref() == Some("Settings…"))
        .unwrap();
    press(&settings);
    let editor = settings_window();
    wait_for(|| editor.is_mapped(), Duration::from_secs(2));
    focus(&editor);
    let field = descendants(editor.upcast_ref())
        .into_iter()
        .find_map(|widget| widget.downcast::<adw::EntryRow>().ok())
        .unwrap();
    assert_eq!(field.text(), "default");
    assert_eq!(after_disable, std::fs::read(&local_path).unwrap());
    assert!(field.grab_focus());
    xdo(&["key", "--clearmodifiers", "ctrl+a"]);
    xdo(&["type", "--clearmodifiers", "--delay", "0", "offline value"]);
    // X11 enqueue completion is not GTK dispatch completion. keep the editor's
    // focus until real text events arrive before moving focus onto Save.
    wait_for(|| field.text() == "offline value", Duration::from_secs(2));
    let save = descendants(editor.upcast_ref())
        .into_iter()
        .filter_map(|widget| widget.downcast::<gtk::Button>().ok())
        .find(|button| button.label().as_deref() == Some("Save"))
        .unwrap();
    press(&save);
    wait_for(
        || {
            std::fs::read_to_string(&local_path)
                .unwrap()
                .contains("offline value")
        },
        Duration::from_secs(2),
    );
    let saved =
        crate::settings::load(&local_path, crate::settings::Document::ManagedPlugins).unwrap();
    assert_eq!(
        saved["plugins"]["github:12/sample"]["settings"]["values"]["label"],
        "offline value"
    );
    assert_eq!(
        saved["plugins"]["github:12/sample"]["settings"]["values"]["future"],
        "opaque"
    );
    assert_eq!(saved["plugins"]["github:12/sample"]["future"], "keep");
    assert_eq!(saved["plugins"]["github:12/sample"]["enabled"], false);
    assert!(has_label(editor.upcast_ref(), "Saved locally"));
    editor.destroy();
    focus(&preferences);
    press(&settings);
    let editor = settings_window();
    let field = descendants(editor.upcast_ref())
        .into_iter()
        .find_map(|widget| widget.downcast::<adw::EntryRow>().ok())
        .unwrap();
    assert_eq!(
        field.text(),
        "offline value",
        "reopening reads latest accepted values, not a row's old capture"
    );
    assert_eq!(ctx.state.borrow().plugins.supervised_count(), 0);
    assert!(!root.join("executed").exists());
    assert_eq!(
        legacy,
        std::fs::read(config.join("plugins.json"))
            .unwrap()
            .as_slice()
    );
    assert_eq!(
        manifest_before,
        std::fs::read(receipt.package_path(&root).join("among-plugin.json")).unwrap()
    );
    println!("PASS real managed Preferences: inert metadata/defaults, desired-on incompatible off via Space, disabled settings edited/saved/reopened, unknown values and legacy bytes preserved, no process or executable probe");
    editor.destroy();
    preferences.destroy();
    ctx.window.destroy();
}

#[test]
#[ignore = "requires an isolated graphical session, built fake-plugin and real keyboard input"]
fn preferences_switch_withdraws_sole_sidebar_summary_without_other_results() {
    fn descendants(widget: &gtk::Widget) -> Vec<gtk::Widget> {
        let mut result = vec![widget.clone()];
        let mut child = widget.first_child();
        while let Some(widget) = child {
            result.extend(descendants(&widget));
            child = widget.next_sibling();
        }
        result
    }
    let app = application("com.github.among-terminal.plugin-withdrawal-test");
    let ctx = build_with_writer(&app, None, crate::session::Writer::new).unwrap();
    wait_for(|| ctx.state.borrow().session_ready, Duration::from_secs(5));
    let terminal = ctx
        .state
        .borrow()
        .pane(PaneId(1))
        .unwrap()
        .terminal()
        .clone();
    ctx.state.borrow_mut().plugins.register(PluginSpec {
        name: "sole-summary".into(),
        command: fixture("echo"),
        enabled: true,
    });
    wait_for(
        || {
            mapped_labels(ctx.workspace_list.upcast_ref(), "ok") == 1
                && has_label(ctx.app_header.upcast_ref(), "ok")
        },
        Duration::from_secs(5),
    );
    assert_eq!(ctx.state.borrow().store.summaries(0).len(), 1);
    gtk::prelude::WidgetExt::activate_action(&ctx.window, "win.preferences", None).unwrap();
    let preferences = gtk::Window::list_toplevels()
        .into_iter()
        .find_map(|window| window.downcast::<adw::PreferencesWindow>().ok())
        .unwrap();
    let row = descendants(preferences.upcast_ref())
        .into_iter()
        .filter_map(|widget| widget.downcast::<adw::ActionRow>().ok())
        .find(|row| row.title() == "sole-summary")
        .unwrap();
    let switch = descendants(row.upcast_ref())
        .into_iter()
        .find_map(|widget| widget.downcast::<gtk::Switch>().ok())
        .unwrap();
    let page = switch
        .ancestor(adw::PreferencesPage::static_type())
        .unwrap()
        .downcast::<adw::PreferencesPage>()
        .unwrap();
    preferences.set_visible_page(&page);
    wait_for(|| switch.is_mapped(), Duration::from_secs(2));
    assert!(switch.is_active());
    let ids = xdo(&[
        "search",
        "--onlyvisible",
        "--pid",
        &std::process::id().to_string(),
        "--name",
        "Preferences",
    ]);
    let id = ids.lines().last().unwrap();
    xdo(&["windowactivate", "--sync", id]);
    xdo(&["windowfocus", "--sync", id]);
    assert!(switch.grab_focus());
    xdo(&["key", "--clearmodifiers", "space"]);
    wait_for(
        || !switch.is_active() && ctx.state.borrow().plugins.is_stopped(),
        Duration::from_secs(2),
    );
    assert!(ctx.state.borrow().store.summaries(0).is_empty());
    assert!(ctx.state.borrow().store.is_empty(Scope::AppHeader));
    assert!(
        !has_label(ctx.workspace_list.upcast_ref(), "ok"),
        "actual Preferences disable left stale sidebar content after all supervisors stopped"
    );
    assert!(!has_label(ctx.app_header.upcast_ref(), "ok"));
    let saved = crate::settings::load(
        &plugin_config_path().unwrap(),
        crate::settings::Document::Plugins,
    )
    .unwrap();
    assert_eq!(saved["plugins"][0]["enabled"], false);
    assert_eq!(
        ctx.state.borrow().pane(PaneId(1)).unwrap().terminal(),
        &terminal
    );
    println!("PASS real Space on the Preferences switch removes the sole rendered sidebar summary and header without another plugin result; saved disabled state and the original terminal survive");
    preferences.destroy();
    ctx.window.destroy();
}

#[test]
#[ignore = "requires an isolated graphical session, built fake-plugin and real keyboard input"]
fn eight_pane_collection_is_fair_to_fast_slow_and_interval_zero_sources() {
    use std::os::fd::AsRawFd;
    let app = application("com.github.among-terminal.plugin-burst-test");
    let ctx = build_with_writer(&app, None, crate::session::Writer::new).unwrap();
    wait_for(|| ctx.state.borrow().session_ready, Duration::from_secs(5));
    for _ in 1..8 {
        split_focused(&ctx);
    }
    let panes = ctx.state.borrow().workspaces.active().panes();
    assert_eq!(panes.len(), 8);
    wait_for(
        || {
            panes.iter().all(|id| {
                ctx.state
                    .borrow()
                    .pane(*id)
                    .unwrap()
                    .terminal()
                    .pty()
                    .is_some_and(|pty| unsafe { libc::tcgetpgrp(pty.fd().as_raw_fd()) } > 0)
            })
        },
        Duration::from_secs(5),
    );
    for (name, mode) in [("early", "echo"), ("late", "delayed"), ("hot", "rapid")] {
        ctx.state.borrow_mut().plugins.register(PluginSpec {
            name: name.into(),
            command: fixture(mode),
            enabled: true,
        });
    }
    let ids = xdo(&[
        "search",
        "--onlyvisible",
        "--pid",
        &std::process::id().to_string(),
        "--name",
        "Among Terminal",
    ]);
    let id = ids.lines().last().unwrap();
    xdo(&["windowactivate", "--sync", id]);
    xdo(&["windowfocus", "--sync", id]);
    xdo(&[
        "type",
        "--clearmodifiers",
        "--delay",
        "0",
        "printf collected > \"$HOME/burst-input\"",
    ]);
    xdo(&["key", "--clearmodifiers", "Return"]);
    let input = std::path::PathBuf::from(std::env::var_os("HOME").unwrap()).join("burst-input");
    wait_for(|| input.exists(), Duration::from_secs(2));
    assert_eq!(std::fs::read_to_string(input).unwrap(), "collected");
    let mut scopes = vec![Scope::AppHeader, Scope::WorkspaceHeader, Scope::AppFooter];
    for id in &panes {
        scopes.push(Scope::PaneHeader(*id));
        scopes.push(Scope::PaneFooter(*id));
    }
    wait_for(
        || {
            let state = ctx.state.borrow();
            assert!(
                state.plugins.listing().iter().all(|(_, state)| matches!(
                    state,
                    PluginState::Starting | PluginState::Running
                )),
                "{:?}",
                state.plugins.listing()
            );
            scopes
                .iter()
                .all(|scope| state.store.sources_in(*scope).len() == 3)
                && state.store.summaries(0).len() == 3
        },
        Duration::from_secs(8),
    );
    for id in &panes {
        let widget = ctx.state.borrow().pane(*id).unwrap().widget().unwrap();
        assert_eq!(
            mapped_labels(widget.upcast_ref(), "ok"),
            6,
            "three sources must reach both actual pane slots"
        );
    }
    println!("PASS eight panes: all 20 scopes collected from fast, delayed and interval-zero sources without faults; deferred siblings remain due, real input and all 48 pane fragments survive");
    ctx.window.destroy();
    wait_for(
        || ctx.state.borrow().plugins.is_stopped(),
        Duration::from_secs(2),
    );
}

#[test]
#[ignore = "requires an isolated graphical session and built fake-plugin"]
fn reactivation_keeps_retiring_and_replacement_plugins_within_one_budget() {
    let app = application("com.github.among-terminal.plugin-reactivation-test");
    let config =
        std::path::PathBuf::from(std::env::var_os("XDG_CONFIG_HOME").unwrap()).join("among");
    let installed =
        std::path::PathBuf::from(std::env::var_os("XDG_DATA_HOME").unwrap()).join("among/plugins");
    std::fs::create_dir_all(&config).unwrap();
    std::fs::create_dir_all(&installed).unwrap();
    let executable = fixture("stubborn").remove(0);
    let mut plugins = Vec::new();
    for index in 0..32 {
        let name = format!("retire-{index}");
        std::os::unix::fs::symlink(&executable, installed.join(&name)).unwrap();
        plugins.push(serde_json::json!({"name":name,"enabled":true}));
    }
    std::fs::write(
        config.join("plugins.json"),
        serde_json::json!({"plugins":plugins}).to_string(),
    )
    .unwrap();
    std::env::set_var("AMONG_FAKE_PLUGIN_MODE", "stubborn");
    let latest = Rc::new(RefCell::new(None));
    app.connect_activate({
        let latest = latest.clone();
        move |app| {
            if let Some(ctx) = build_with_writer(app, None, crate::session::Writer::new) {
                *latest.borrow_mut() = Some(ctx);
            }
        }
    });
    app.activate();
    let mut current = latest.borrow().as_ref().unwrap().handle();
    wait_for(
        || current.state.borrow().plugins.healthy().len() == 32,
        Duration::from_secs(5),
    );
    let previous_pids = current.state.borrow().plugins.supervised_pids();
    assert_eq!(previous_pids.len(), 32);
    let proof_holds: Vec<_> = (0..32)
        .map(|index| {
            current
                .state
                .borrow()
                .plugins
                .hold_retirement(&format!("retire-{index}"), None)
                .unwrap()
        })
        .collect();
    let mut retired = Vec::new();
    for _ in 0..3 {
        current.window.destroy();
        app.activate();
        let replacement = latest.borrow().as_ref().unwrap().handle();
        assert_ne!(replacement.window, current.window);
        assert!(replacement.window.is_visible());
        assert_eq!(replacement.state.borrow().plugins.supervised_count(), 0);
        assert!(replacement
            .state
            .borrow()
            .plugins
            .listing()
            .iter()
            .all(|(_, state)| matches!(state, PluginState::Stopping { .. })));
        retired.push(current);
        assert!(
            retired
                .iter()
                .map(|old| old.state.borrow().plugins.supervised_count())
                .sum::<usize>()
                <= 32
        );
        current = replacement;
    }
    drop(proof_holds);
    if !current.state.borrow().session_ready {
        let dialog = current
            .window
            .visible_dialog()
            .unwrap()
            .downcast::<adw::AlertDialog>()
            .unwrap();
        assert!(dialog
            .heading()
            .is_some_and(|heading| heading.contains("Restore")));
        dialog.set_close_response("new");
        dialog.force_close();
    }
    wait_for(
        || {
            let old_count = retired
                .iter()
                .map(|old| old.state.borrow().plugins.supervised_count())
                .sum::<usize>();
            let new_count = current.state.borrow().plugins.supervised_count();
            assert!(
                old_count + new_count <= 32,
                "application supervision budget was exceeded"
            );
            let old_identities: std::collections::BTreeSet<_> = retired
                .iter()
                .flat_map(|old| old.state.borrow().plugins.supervised_identities())
                .collect();
            let new_identities = current.state.borrow().plugins.supervised_identities();
            assert!(
                old_identities.is_disjoint(&new_identities),
                "replacement overlapped an unreaped predecessor identity"
            );
            old_count == 0 && current.state.borrow().plugins.healthy().len() == 32
        },
        Duration::from_secs(5),
    );
    for pid in previous_pids {
        assert!(
            !std::path::Path::new(&format!("/proc/{pid}")).exists(),
            "old process {pid} was not reaped"
        );
    }
    println!("PASS three real reactivations with 32 TERM-ignoring plugins: one shared budget, no overlapping identity generations, and old children reaped before their replacements start");
    current.window.destroy();
    wait_for(
        || current.state.borrow().plugins.is_stopped(),
        Duration::from_secs(2),
    );
    std::env::remove_var("AMONG_FAKE_PLUGIN_MODE");
}

#[test]
#[ignore = "requires an isolated graphical session and built fake-plugin"]
fn one_unproven_retirement_keeps_replacement_window_terminals_and_other_plugins_usable() {
    let app = application("com.github.among-terminal.isolated-retirement-test");
    let config =
        std::path::PathBuf::from(std::env::var_os("XDG_CONFIG_HOME").unwrap()).join("among");
    let installed =
        std::path::PathBuf::from(std::env::var_os("XDG_DATA_HOME").unwrap()).join("among/plugins");
    std::fs::create_dir_all(&config).unwrap();
    std::fs::create_dir_all(&installed).unwrap();
    let executable = fixture("stubborn").remove(0);
    for name in ["blocked", "unrelated"] {
        std::os::unix::fs::symlink(&executable, installed.join(name)).unwrap();
    }
    std::fs::write(config.join("plugins.json"), serde_json::json!({"plugins":[{"name":"blocked","enabled":true},{"name":"unrelated","enabled":true}]}).to_string()).unwrap();
    std::env::set_var("AMONG_FAKE_PLUGIN_MODE", "stubborn");
    let latest = Rc::new(RefCell::new(None));
    app.connect_activate({
        let latest = latest.clone();
        move |app| {
            if let Some(ctx) = build_with_writer(app, None, crate::session::Writer::new) {
                *latest.borrow_mut() = Some(ctx);
            }
        }
    });
    app.activate();
    let old = latest.borrow().as_ref().unwrap().handle();
    wait_for(
        || old.state.borrow().plugins.healthy().len() == 2,
        Duration::from_secs(5),
    );
    let hold = old
        .state
        .borrow()
        .plugins
        .hold_retirement("blocked", Some(libc::EACCES))
        .unwrap();
    old.window.destroy();
    app.activate();
    let current = latest.borrow().as_ref().unwrap().handle();
    assert_ne!(old.window, current.window);
    assert!(current.window.is_visible());
    if !current.state.borrow().session_ready {
        let dialog = current
            .window
            .visible_dialog()
            .unwrap()
            .downcast::<adw::AlertDialog>()
            .unwrap();
        dialog.set_close_response("new");
        dialog.force_close();
    }
    wait_for(
        || {
            let listing = current.state.borrow().plugins.listing();
            hold.observations() >= 2
                && listing
                    .iter()
                    .any(|(name, state)| name == "unrelated" && state.is_healthy())
        },
        Duration::from_secs(5),
    );
    assert!(!old.state.borrow().plugins.is_stopped());
    assert!(!current.state.borrow().plugins.supervised_stopped("blocked"));
    assert!(current.state.borrow().plugins.listing().iter().any(|(name, state)| name == "blocked" && matches!(state, PluginState::Stopping { reason, .. } if reason.contains("permission denied"))));
    assert_eq!(
        old.state.borrow().plugins.supervised_count()
            + current.state.borrow().plugins.supervised_count(),
        2
    );
    current.window.present();
    xdo(&[
        "type",
        "--clearmodifiers",
        "--delay",
        "0",
        "printf usable > \"$HOME/isolated-retirement-input\"",
    ]);
    xdo(&["key", "--clearmodifiers", "Return"]);
    let proof = std::path::PathBuf::from(std::env::var_os("HOME").unwrap())
        .join("isolated-retirement-input");
    wait_for(|| proof.exists(), Duration::from_secs(1));
    assert_eq!(std::fs::read_to_string(proof).unwrap(), "usable");
    assert!(!old.state.borrow().plugins.is_stopped());
    drop(hold);
    wait_for(
        || {
            old.state.borrow().plugins.is_stopped()
                && current.state.borrow().plugins.healthy().len() == 2
        },
        Duration::from_secs(5),
    );
    println!("PASS unresolved retirement blocks only its identity: replacement window, real terminal input and unrelated plugin remain usable; no premature stop acknowledgement");
    current.window.destroy();
    wait_for(
        || current.state.borrow().plugins.is_stopped(),
        Duration::from_secs(2),
    );
    std::env::remove_var("AMONG_FAKE_PLUGIN_MODE");
}
