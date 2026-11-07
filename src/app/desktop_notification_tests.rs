use super::bell_tests::{pump_for, real_bell, terminal, wait, xdo};
use super::*;
use among::notifications::{preferences::Edit, Eligibility, Policy, COOLDOWN};
use std::collections::BTreeMap;

#[test]
fn missing_notification_preferences_keep_defaults_without_creating_a_file() {
    let root = crate::plugin::install::TestOwnedRoot::new();
    let mut storage = crate::settings::PreferencesStorage::new(Some(root.0.clone()));
    let loaded = initial_notification_preferences(&mut storage);
    assert_eq!(loaded.desktop, Policy::DESKTOP_DEFAULT);
    assert!(storage.problems.is_empty());
    assert!(!root.0.join("general.json").exists());
}

#[test]
fn rejected_notification_preferences_disable_desktop_without_repairing_saved_bytes() {
    use std::os::unix::fs::PermissionsExt;
    for bytes in [
        "{",
        r#"{"version":2}"#,
        r#"{"version":1,"notifications":{"desktop":{"enabled":"true"}}}"#,
        r#"{"version":1,"notifications":{"desktop":{"eligibility":"future"}}}"#,
        r#"{"version":1,"notifications":{"audio":{"enabled":17}}}"#,
    ] {
        let root = crate::plugin::install::TestOwnedRoot::new();
        let file = root.0.join("general.json");
        std::fs::write(&file, bytes).unwrap();
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600)).unwrap();
        let mut storage = crate::settings::PreferencesStorage::new(Some(root.0.clone()));
        let loaded = initial_notification_preferences(&mut storage);
        assert!(
            !loaded.desktop.enabled,
            "rejected policy enabled desktop: {bytes}"
        );
        assert!(storage.problems.contains_key("general.json"));
        assert_eq!(std::fs::read_to_string(&file).unwrap(), bytes);
    }
}

#[test]
fn unsafe_or_unavailable_notification_preferences_disable_desktop_without_writing() {
    use std::os::unix::fs::{symlink, PermissionsExt};
    const VALID: &str = r#"{"version":1,"notifications":{"desktop":{"enabled":true}}}"#;
    for unsafe_kind in ["symlink", "writable-file", "writable-directory"] {
        let root = crate::plugin::install::TestOwnedRoot::new();
        let profile = root.0.join("profile");
        std::fs::create_dir(&profile).unwrap();
        std::fs::set_permissions(&profile, std::fs::Permissions::from_mode(0o700)).unwrap();
        let file = profile.join("general.json");
        let target = root.0.join("target.json");
        std::fs::write(&target, VALID).unwrap();
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o600)).unwrap();
        if unsafe_kind == "symlink" {
            symlink(&target, &file).unwrap();
        } else {
            std::fs::write(&file, VALID).unwrap();
            std::fs::set_permissions(
                &file,
                std::fs::Permissions::from_mode(if unsafe_kind == "writable-file" {
                    0o666
                } else {
                    0o600
                }),
            )
            .unwrap();
        }
        if unsafe_kind == "writable-directory" {
            std::fs::set_permissions(&profile, std::fs::Permissions::from_mode(0o777)).unwrap();
        }
        let mut storage = crate::settings::PreferencesStorage::new(Some(profile));
        assert!(
            !initial_notification_preferences(&mut storage)
                .desktop
                .enabled,
            "unsafe {unsafe_kind} enabled desktop"
        );
        assert!(storage.problems.contains_key("general.json"));
        assert_eq!(std::fs::read_to_string(&target).unwrap(), VALID);
        if unsafe_kind == "symlink" {
            assert_eq!(std::fs::read_link(&file).unwrap(), target);
        } else {
            assert_eq!(std::fs::read_to_string(&file).unwrap(), VALID);
        }
    }
    let mut unavailable = crate::settings::PreferencesStorage::new(None);
    assert!(
        !initial_notification_preferences(&mut unavailable)
            .desktop
            .enabled
    );
    assert!(unavailable.problems.contains_key("general.json"));
}

pub(super) const APP_ID: &str = "com.github.among-terminal.desktop-notification-test";
// Wire contract: GNOME GLib gio/ggtknotificationbackend.c AddNotification(ssa{sv})
// and RemoveNotification(ss); the recorder checks the real GIO calls.
const INTERFACE: &str = r#"<node><interface name="org.gtk.Notifications">
<method name="AddNotification"><arg type="s" direction="in"/><arg type="s" direction="in"/><arg type="a{sv}" direction="in"/></method>
<method name="RemoveNotification"><arg type="s" direction="in"/><arg type="s" direction="in"/></method>
</interface></node>"#;

#[derive(Clone)]
enum Call {
    Add {
        application: String,
        id: String,
        fields: BTreeMap<String, glib::Variant>,
    },
    Remove {
        application: String,
        id: String,
    },
}

pub(super) struct Server {
    connection: gio::DBusConnection,
    registration: Option<gio::RegistrationId>,
    calls: Rc<RefCell<Vec<Call>>>,
}

impl Server {
    pub(super) fn new() -> Self {
        let connection = gio::bus_get_sync(gio::BusType::Session, gio::Cancellable::NONE).unwrap();
        let node = gio::DBusNodeInfo::for_xml(INTERFACE).unwrap();
        let calls = Rc::new(RefCell::new(Vec::new()));
        let registration = connection
            .register_object("/org/gtk/Notifications", &node.interfaces()[0])
            .method_call({
                let calls = calls.clone();
                move |_, _, _, _, method, params, invocation| {
                    println!("GTK_NOTIFICATION {method} {}", params.print(true));
                    let call = match method {
                        "AddNotification" => {
                            let (application, id, fields) = params
                                .get::<(String, String, BTreeMap<String, glib::Variant>)>()
                                .unwrap();
                            Call::Add {
                                application,
                                id,
                                fields,
                            }
                        }
                        "RemoveNotification" => {
                            let (application, id) = params.get::<(String, String)>().unwrap();
                            Call::Remove { application, id }
                        }
                        _ => panic!("unexpected notification method {method}"),
                    };
                    calls.borrow_mut().push(call);
                    invocation.return_value(Some(&().to_variant()));
                }
            })
            .build()
            .unwrap();
        let reply = connection
            .call_sync(
                Some("org.freedesktop.DBus"),
                "/org/freedesktop/DBus",
                "org.freedesktop.DBus",
                "RequestName",
                Some(&("org.gtk.Notifications", 4_u32).to_variant()),
                None,
                gio::DBusCallFlags::NONE,
                5_000,
                gio::Cancellable::NONE,
            )
            .unwrap();
        assert_eq!(
            reply.get::<(u32,)>().unwrap(),
            (1,),
            "a real notification daemon must never be replaced"
        );
        Self {
            connection,
            registration: Some(registration),
            calls,
        }
    }

    pub(super) fn additions(&self) -> Vec<(String, BTreeMap<String, glib::Variant>)> {
        self.calls
            .borrow()
            .iter()
            .filter_map(|call| match call {
                Call::Add {
                    application,
                    id,
                    fields,
                } => {
                    assert_eq!(application, APP_ID);
                    Some((id.clone(), fields.clone()))
                }
                _ => None,
            })
            .collect()
    }

    pub(super) fn removed(&self, expected: &str) -> bool {
        self.calls.borrow().iter().any(|call| matches!(call, Call::Remove { application, id } if application == APP_ID && id == expected))
    }

    fn stop(&mut self) {
        if let Some(registration) = self.registration.take() {
            self.connection
                .call_sync(
                    Some("org.freedesktop.DBus"),
                    "/org/freedesktop/DBus",
                    "org.freedesktop.DBus",
                    "ReleaseName",
                    Some(&("org.gtk.Notifications",).to_variant()),
                    None,
                    gio::DBusCallFlags::NONE,
                    5_000,
                    gio::Cancellable::NONE,
                )
                .unwrap();
            self.connection.unregister_object(registration).unwrap();
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop();
    }
}

fn call_activation(app: &adw::Application, target: glib::Variant) {
    let connection = app.dbus_connection().unwrap();
    let completed = Rc::new(Cell::new(false));
    connection.call(
        connection.unique_name().as_deref(),
        app.dbus_object_path().unwrap().as_str(),
        "org.freedesktop.Application",
        "ActivateAction",
        Some(
            &(
                "focus-pane",
                vec![target],
                BTreeMap::<String, glib::Variant>::new(),
            )
                .to_variant(),
        ),
        None,
        gio::DBusCallFlags::NONE,
        5_000,
        gio::Cancellable::NONE,
        {
            let completed = completed.clone();
            move |result| {
                result.unwrap();
                completed.set(true);
            }
        },
    );
    wait(|| completed.get(), "D-Bus application action completed");
}

pub(super) fn named<T: IsA<gtk::Widget> + glib::object::IsClass>(
    root: &impl IsA<gtk::Widget>,
    name: &str,
) -> T {
    fn find(widget: &gtk::Widget, name: &str) -> Option<gtk::Widget> {
        if widget.widget_name() == name {
            return Some(widget.clone());
        }
        let mut child = widget.first_child();
        while let Some(widget) = child {
            if let Some(found) = find(&widget, name) {
                return Some(found);
            }
            child = widget.next_sibling();
        }
        None
    }
    find(root.as_ref(), name)
        .unwrap_or_else(|| panic!("missing {name}"))
        .downcast()
        .ok()
        .unwrap()
}

fn activation_diagnostic(window: &gtk::Window, phase: &str, xid: &str) {
    let observe = |argument: &str| {
        let output = std::process::Command::new("xdotool")
            .arg(argument)
            .output()
            .unwrap();
        format!(
            "{}:{}",
            output.status,
            String::from_utf8_lossy(&output.stdout).trim()
        )
    };
    let windows: Vec<_> = gtk::Window::list_toplevels()
        .into_iter()
        .filter_map(|widget| widget.downcast::<gtk::Window>().ok())
        .map(|candidate| {
            (
                candidate.title(),
                candidate.is_mapped(),
                candidate.is_active(),
            )
        })
        .collect();
    eprintln!("desktop activation phase={phase} title={:?} xid={xid} mapped={} active={} x_active={} x_focus={} windows={windows:?}",
        window.title(), window.is_mapped(), window.is_active(), observe("getactivewindow"), observe("getwindowfocus"));
}

/// Search predicates are ANDed explicitly: xdotool's default OR would also
/// select the other test window with the same PID, regardless of its title.
pub(super) fn activate(window: &impl IsA<gtk::Window>, phase: &str) {
    let window = window.as_ref();
    let title = window.title().unwrap();
    let exact_title = format!("^{title}$");
    let ids = xdo(&[
        "search",
        "--all",
        "--onlyvisible",
        "--pid",
        &std::process::id().to_string(),
        "--name",
        &exact_title,
    ]);
    assert_eq!(
        ids.lines().count(),
        1,
        "{phase}: PID and exact title must select one owned visible window; ids={ids:?}"
    );
    let xid = ids.trim();
    activation_diagnostic(window, &format!("{phase}:before"), xid);
    xdo(&["windowactivate", "--sync", xid]);
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while !window.is_active() && std::time::Instant::now() < deadline {
        pump_for(Duration::from_millis(20));
    }
    activation_diagnostic(window, &format!("{phase}:after"), xid);
    assert!(
        window.is_active(),
        "{phase}: target window {title:?} xid={xid} did not become active"
    );
    pump_for(Duration::from_millis(100));
}

fn closing_activation_preserves_the_final_snapshot_until_close_is_cancelled(
    ctx: &Window,
    application: &adw::Application,
    other: &gtk::Window,
    target: &glib::Variant,
    originating_pane: PaneId,
) {
    struct ReleaseWriter(Option<std::sync::mpsc::Sender<()>>);
    impl Drop for ReleaseWriter {
        fn drop(&mut self) {
            if let Some(sender) = self.0.take() {
                let _ = sender.send(());
            }
        }
    }
    fn find_button(widget: &gtk::Widget) -> Option<gtk::Button> {
        if let Some(button) = widget.downcast_ref::<gtk::Button>() {
            if button.label().as_deref() == Some("Keep open") {
                return Some(button.clone());
            }
        }
        let mut child = widget.first_child();
        while let Some(widget) = child {
            if let Some(button) = find_button(&widget) {
                return Some(button);
            }
            child = widget.next_sibling();
        }
        None
    }

    let backend = ctx.state.borrow().bells.clone().unwrap();
    let source = backend
        .resolve_desktop_target(target)
        .expect("existing core source target");
    assert_eq!(source.pane(), originating_pane.0);
    assert!(ctx.state.borrow_mut().workspaces.activate(1));
    switch_workspace(ctx);
    split_focused(ctx);
    toggle_zoom(ctx);
    let selected = ctx.state.borrow().workspaces.active().focused;
    assert_eq!(
        ctx.state.borrow().workspaces.active().zoomed,
        Some(selected)
    );
    let snapshot = capture_session(ctx).unwrap();
    let path = crate::session::path().unwrap();
    crate::test_isolation::assert_below_check_root(&path);
    let (captured_tx, captured_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let release = ReleaseWriter(Some(release_tx));
    ctx.state.borrow_mut().session_writer = Some(
        crate::session::Writer::with_save(path, move |_, snapshot| {
            captured_tx
                .send(snapshot.clone())
                .map_err(|error| error.to_string())?;
            release_rx.recv().map_err(|error| error.to_string())?;
            Err("controlled final session write failure".into())
        })
        .unwrap(),
    );
    quit(ctx);
    assert!(ctx.state.borrow().session_closing);
    let mut captured = None;
    wait(
        || {
            if let Ok(snapshot) = captured_rx.try_recv() {
                captured = Some(snapshot);
            }
            captured.is_some()
        },
        "final session writer captured its snapshot",
    );
    assert_eq!(captured.as_ref(), Some(&snapshot));

    activate(other, "background-during-final-session-write");
    call_activation(application, target.clone());
    wait(
        || ctx.window.is_active(),
        "closing notification activation raises the window",
    );
    assert_eq!(backend.resolve_desktop_target(target), Some(source));
    {
        let state = ctx.state.borrow();
        assert!(state.session_closing);
        assert_eq!(state.workspaces.active_index(), snapshot.active);
        assert_eq!(state.workspaces.active().focused, selected);
        assert_eq!(state.workspaces.active().zoomed, Some(selected));
        for (workspace, saved) in state
            .workspaces
            .workspaces()
            .iter()
            .zip(&snapshot.workspaces)
        {
            assert_eq!(workspace.focused.0, saved.focused);
        }
    }
    assert!(!terminal(ctx, originating_pane).has_focus());

    // Let the real final-write completion open the existing failure dialog;
    // cancelling that close must restore ordinary notification navigation.
    drop(release);
    wait(
        || ctx.window.visible_dialog().is_some(),
        "failed final write offers to keep the session open",
    );
    let dialog = ctx.window.visible_dialog().unwrap();
    let keep_open = find_button(dialog.upcast_ref()).expect("Keep open response");
    activate(&ctx.window, "keep-open-after-final-write-refusal");
    wait(
        || keep_open.is_mapped() && keep_open.is_sensitive(),
        "Keep open is operable",
    );
    assert!(keep_open.grab_focus());
    xdo(&["key", "--clearmodifiers", "Return"]);
    wait(
        || !ctx.state.borrow().session_closing && ctx.window.visible_dialog().is_none(),
        "cancelled close restores the live session",
    );
    assert!(!ctx.state.borrow().exiting);
    call_activation(application, target.clone());
    wait(
        || {
            ctx.state.borrow().workspaces.active_index() == 0
                && terminal(ctx, originating_pane).has_focus()
        },
        "notification navigation works after cancelling close",
    );
    assert_eq!(backend.resolve_desktop_target(target), Some(source));
    assert_eq!(
        ctx.state.borrow().workspaces.workspaces()[1].zoomed,
        Some(selected)
    );
    println!("PASS notification activation during final session write raises only; captured active/focus and live zoom stay fixed, and cancelling close restores exact-target navigation");
}

#[test]
#[ignore = "requires an owned X11 display, private session bus and xdotool"]
fn real_bells_send_exact_desktop_payloads_and_clicks_focus_only_current_targets() {
    let config = std::path::PathBuf::from(std::env::var_os("XDG_CONFIG_HOME").unwrap());
    crate::test_isolation::assert_below_check_root(&config);
    assert!(
        !gtk::is_initialized(),
        "desktop test must run in a fresh process"
    );
    let mut server = Server::new();
    let application = adw::Application::builder().application_id(APP_ID).build();
    application.register(gio::Cancellable::NONE).unwrap();
    let ctx = build_with_writer(&application, None, crate::session::Writer::new).unwrap();
    wait(
        || ctx.state.borrow().session_ready,
        "initial terminal ready",
    );
    activate(&ctx.window, "initial-main");
    let backend = ctx.state.borrow().bells.clone().unwrap();
    wait(
        || backend.desktop_report().is_none(),
        "notification service observed",
    );
    let first = ctx.state.borrow().workspaces.active().focused;
    ctx.state
        .borrow_mut()
        .workspaces
        .active_mut()
        .rename("build");
    terminal(&ctx, first)
        .feed_child(b"printf '\\033]0;make\\007\\033]7;file://localhost/work\\007'\n");
    wait(
        || ctx.state.borrow().pane(first).unwrap().title().as_deref() == Some("make"),
        "terminal title",
    );
    wait(
        || ctx.state.borrow().pane(first).unwrap().cwd().as_deref() == Some("/work"),
        "terminal directory",
    );
    let other = gtk::Window::builder()
        .title("Outside Among")
        .default_width(300)
        .default_height(160)
        .build();
    other.present();
    activate(&other, "background-before-first-bell");
    wait(
        || !backend.focus().application_active,
        "Among in background",
    );
    pump_for(Duration::from_millis(200));
    assert_eq!(
        ctx.state.borrow().desktop_text_builds.get(),
        0,
        "idle ticks built desktop text"
    );
    real_bell(&ctx, &backend, first);
    wait(
        || server.additions().len() == 1,
        "first notification on private bus",
    );
    assert_eq!(
        ctx.state.borrow().desktop_text_builds.get(),
        1,
        "one eligible bell must build its current text once"
    );
    let (first_id, fields) = server.additions().remove(0);
    assert_eq!(
        fields.keys().map(String::as_str).collect::<Vec<_>>(),
        [
            "body",
            "default-action",
            "default-action-target",
            "icon",
            "priority",
            "title"
        ]
    );
    assert_eq!(fields["title"].get::<String>().as_deref(), Some("build"));
    assert_eq!(
        fields["body"].get::<String>().as_deref(),
        Some("Bell in 'make' /work")
    );
    assert_eq!(
        fields["priority"].get::<String>().as_deref(),
        Some("normal")
    );
    assert_eq!(
        fields["default-action"].get::<String>().as_deref(),
        Some("app.focus-pane")
    );
    let (icon_kind, icon_names) = fields["icon"].get::<(String, glib::Variant)>().unwrap();
    assert_eq!(icon_kind, "themed");
    assert!(icon_names
        .get::<Vec<String>>()
        .unwrap()
        .iter()
        .any(|name| name == "com.github.among-terminal"));
    let target = fields["default-action-target"].clone();
    assert_eq!(backend.desktop_target(first.0), Some(target.clone()));
    pump_for(COOLDOWN + Duration::from_millis(100));
    real_bell(&ctx, &backend, first);
    wait(
        || server.additions().len() == 2,
        "replacement on private bus",
    );
    assert_eq!(server.additions()[1].0, first_id);
    activate(&ctx.window, "main-after-replacement");
    terminal(&ctx, first).grab_focus();
    wait(|| server.removed(&first_id), "focus withdraws notification");
    backend
        .edit(&Edit::Desktop(Policy {
            enabled: true,
            eligibility: Eligibility::Always,
        }))
        .unwrap();
    let count = server.additions().len();
    real_bell(&ctx, &backend, first);
    pump_for(Duration::from_millis(200));
    assert_eq!(
        server.additions().len(),
        count,
        "Always notified for a viewed terminal"
    );
    backend
        .edit(&Edit::Desktop(Policy {
            enabled: false,
            eligibility: Eligibility::Always,
        }))
        .unwrap();
    activate(&other, "background-disabled-channel");
    let builds_before_disabled = ctx.state.borrow().desktop_text_builds.get();
    real_bell(&ctx, &backend, first);
    pump_for(Duration::from_millis(200));
    assert_eq!(
        server.additions().len(),
        count,
        "disabled channel sent a notification"
    );
    assert_eq!(
        ctx.state.borrow().desktop_text_builds.get(),
        builds_before_disabled,
        "disabled desktop bells or ticks built notification text"
    );

    // Opening the row is inert; its actual controls immediately change policy.
    activate(&ctx.window, "main-before-preferences");
    let settings_file = crate::settings::directory().unwrap().join("general.json");
    let before = std::fs::read(&settings_file).ok();
    open_preferences(&ctx);
    let preferences = gtk::Window::list_toplevels()
        .into_iter()
        .find_map(|widget| widget.downcast::<adw::PreferencesWindow>().ok())
        .unwrap();
    preferences.set_visible_page_name("notifications");
    activate(&preferences, "notification-preferences");
    pump_for(Duration::from_millis(250));
    assert_eq!(std::fs::read(&settings_file).ok(), before);
    let enabled: adw::SwitchRow = named(&preferences, "notification-desktop-switch");
    assert!(!enabled.is_active());
    assert!(enabled.grab_focus());
    xdo(&["key", "--clearmodifiers", "space"]);
    wait(
        || backend.preferences().desktop.enabled,
        "desktop switch applies immediately",
    );
    let when: adw::ComboRow = named(&preferences, "notification-desktop-when");
    assert!(when
        .tooltip_text()
        .is_some_and(|text| text.contains("Always has the same effect as Terminal not in view")));
    assert!(enabled.grab_focus());
    xdo(&["key", "--clearmodifiers", "space"]);
    wait(
        || !backend.preferences().desktop.enabled,
        "desktop switch changed to its non-default value",
    );
    when.set_selected(1);
    let reloaded =
        crate::settings::load(&settings_file, crate::settings::Document::General).unwrap();
    assert_eq!(
        among::notifications::preferences::Preferences::parse(&reloaded)
            .unwrap()
            .desktop,
        Policy {
            enabled: false,
            eligibility: Eligibility::OriginatingTerminalUnfocused
        }
    );
    assert!(enabled.grab_focus());
    xdo(&["key", "--clearmodifiers", "space"]);
    wait(
        || backend.preferences().desktop.enabled,
        "desktop switch restored for the remaining bell checks",
    );
    when.set_selected(2);
    assert_eq!(
        backend.preferences().desktop.eligibility,
        Eligibility::ApplicationBackground
    );
    let restored =
        crate::settings::load(&settings_file, crate::settings::Document::General).unwrap();
    assert_eq!(
        among::notifications::preferences::Preferences::parse(&restored)
            .unwrap()
            .desktop,
        Policy::DESKTOP_DEFAULT
    );
    preferences.destroy();

    // A hostile OSC title is bounded and plain on the actual bus call.
    terminal(&ctx, first).feed_child(b"printf '\\033]0;<b>hostile</b>&\\007'\n");
    wait(
        || terminal(&ctx, first).window_title().as_deref() == Some("<b>hostile</b>&"),
        "hostile title received",
    );
    activate(&other, "background-hostile-title");
    pump_for(COOLDOWN + Duration::from_millis(100));
    real_bell(&ctx, &backend, first);
    wait(
        || server.additions().len() == count + 1,
        "sanitised notification",
    );
    let body = server.additions().last().unwrap().1["body"]
        .get::<String>()
        .unwrap();
    assert_eq!(body, "Bell in 'bhostile/b' /work");
    assert!(!body.contains(['<', '>', '&']));

    // The daemon invokes the exported application action over D-Bus.
    new_workspace(&ctx);
    wait(
        || ctx.state.borrow().workspaces.len() == 2,
        "second workspace",
    );
    set_sidebar_collapsed(&ctx, true);
    call_activation(&application, target.clone());
    wait(
        || ctx.state.borrow().workspaces.active_index() == 0 && terminal(&ctx, first).has_focus(),
        "click selects exact pane across workspaces",
    );
    assert!(ctx.state.borrow().sidebar_collapsed);
    split_focused(&ctx);
    let sibling = ctx.state.borrow().workspaces.active().focused;
    let closed_target = backend.desktop_target(sibling.0).unwrap();
    activate(&other, "background-sibling-bell");
    let count = server.additions().len();
    real_bell(&ctx, &backend, sibling);
    wait(
        || server.additions().len() == count + 1,
        "sibling notification",
    );
    let sibling_id = server.additions().last().unwrap().0.clone();
    toggle_zoom(&ctx);
    assert_eq!(ctx.state.borrow().workspaces.active().zoomed, Some(sibling));
    call_activation(&application, target.clone());
    wait(
        || {
            ctx.state.borrow().workspaces.active().zoomed.is_none()
                && terminal(&ctx, first).has_focus()
        },
        "click restores a hidden sibling",
    );
    close_pane(&ctx, sibling);
    wait(
        || server.removed(&sibling_id),
        "closing the source withdraws its notification",
    );
    let selected = ctx.state.borrow().workspaces.active().focused;
    activate(&other, "background-before-closed-target-activation");
    call_activation(&application, closed_target);
    wait(|| ctx.window.is_active(), "closed target raises window");
    assert_eq!(ctx.state.borrow().workspaces.active().focused, selected);
    let (_, session, generation, pane) = target.get::<(String, u64, u64, u64)>().unwrap();
    let foreign = ("previous-instance", session, generation, pane).to_variant();
    activate(&other, "background-before-stale-target-activation");
    call_activation(&application, foreign);
    wait(|| ctx.window.is_active(), "stale target raises window");
    assert_eq!(ctx.state.borrow().workspaces.active().focused, selected);
    assert!(backend
        .resolve_desktop_target(&"wrong type".to_variant())
        .is_none());
    assert_eq!(
        application
            .lookup_action("focus-pane")
            .unwrap()
            .parameter_type()
            .unwrap()
            .as_str(),
        "(sttt)",
        "the exported action must reject parameters of a different wire type"
    );

    closing_activation_preserves_the_final_snapshot_until_close_is_cancelled(
        &ctx,
        &application,
        &other,
        &target,
        first,
    );

    // Losing the service reports a known limitation and never replays bells.
    let count = server.additions().len();
    server.stop();
    wait(
        || {
            backend
                .desktop_report()
                .is_some_and(|report| report.contains("No notification service"))
        },
        "missing service is reported",
    );
    activate(&other, "background-missing-service");
    real_bell(&ctx, &backend, first);
    assert!(
        backend.attention(first.0).unread,
        "missing desktop service affected visual attention"
    );
    pump_for(Duration::from_millis(200));
    assert_eq!(server.additions().len(), count);
    let restored = Server::new();
    wait(
        || backend.desktop_report().is_none(),
        "notification service returned",
    );
    pump_for(COOLDOWN + Duration::from_millis(100));
    assert!(
        restored.additions().is_empty(),
        "an unavailable-service bell was replayed"
    );
    real_bell(&ctx, &backend, first);
    wait(
        || restored.additions().len() == 1,
        "new bell after service return",
    );
    let final_id = restored.additions()[0].0.clone();
    backend.shutdown();
    wait(
        || restored.removed(&final_id),
        "normal shutdown withdraws outstanding notifications",
    );
    other.destroy();
    ctx.window.destroy();
    println!("PASS desktop bus payload, replacement, focus withdrawal, viewed/off suppression, sanitisation, settings, cross-workspace activation, zoom restoration, closed/foreign targets and missing-service reporting");
}
