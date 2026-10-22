use super::bell_tests::{activate, pump_for, real_bell, terminal, wait, xdo};
use super::*;
use std::path::PathBuf;

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
    let app = adw::Application::builder()
        .application_id("com.github.among-terminal.notification-ui-test")
        .flags(gio::ApplicationFlags::NON_UNIQUE)
        .build();
    app.register(None::<&gio::Cancellable>).unwrap();
    let ctx = build_with_writer(&app, None, crate::session::Writer::new).unwrap();
    pump_for(Duration::from_millis(350));
    activate(&ctx.window);
    (app, ctx)
}

fn accessible_label(widget: &impl IsA<gtk::Accessible>, expected: &str) {
    use glib::translate::*;
    let text = std::ffi::CString::new(expected).unwrap();
    let error = unsafe {
        gtk::ffi::gtk_test_accessible_check_property(
            widget.as_ref().to_glib_none().0,
            gtk::ffi::GTK_ACCESSIBLE_PROPERTY_LABEL,
            text.as_ptr(),
        )
    };
    if !error.is_null() {
        let problem = unsafe { std::ffi::CStr::from_ptr(error) }
            .to_string_lossy()
            .into_owned();
        unsafe {
            glib::ffi::g_free(error.cast());
        }
        panic!("accessible label mismatch: {problem}");
    }
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
#[ignore = "requires owned X11/private XDG plus prebuilt Rust command fixture; ordinary small trees only, security acceptance still blocked pending PID review"]
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
    println!("PASS real labelled General keyboard program/literal-argv/condition/enable controls drive the normal VTE backend registration, lazy off-loop directory preparation, shared-tick execution/completion, stable ownership and nonblocking GApplication hold through real small-tree reaping; this is NOT security approval of the pending PID-parser repair");
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
