//! Actual freedesktop notification delivery into the real application action.
//!
//! The fixture owns only the FDO service on the private session bus. The real
//! application selects the FDO route naturally, receives its real `Notify`, and
//! runs the installed `focus-pane` handler after the fixture emits
//! `ActivationToken` and `ActionInvoked`.

use super::*;
use crate::app::bell_tests::{pump_for, real_bell, terminal, wait};
use serde_json::{json, Value};
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

const FDO: &str = "org.freedesktop.Notifications";
const PATH: &str = "/org/freedesktop/Notifications";
const TOKEN: &str = "fdo-window-token";

const XML: &str = r#"<node><interface name="org.freedesktop.Notifications">
<method name="Notify"><arg type="s" direction="in"/><arg type="u" direction="in"/><arg type="s" direction="in"/><arg type="s" direction="in"/><arg type="s" direction="in"/><arg type="as" direction="in"/><arg type="a{sv}" direction="in"/><arg type="i" direction="in"/><arg type="u" direction="out"/></method>
<method name="CloseNotification"><arg type="u" direction="in"/></method>
<signal name="ActionInvoked"><arg type="u"/><arg type="s"/></signal>
<signal name="ActivationToken"><arg type="u"/><arg type="s"/></signal>
<signal name="NotificationClosed"><arg type="u"/><arg type="u"/></signal>
</interface></node>"#;

struct Pending {
    id: u32,
    invocation: gio::DBusMethodInvocation,
}

/// Copied out of the D-Bus message-handling thread. It contains no GTK, GIO
/// object or Rc/RefCell value.
struct ObservedActivation {
    incoming: bool,
    target: String,
    token: Option<String>,
}

#[derive(Default)]
struct State {
    next_id: u32,
    pending: Option<Pending>,
    pending_close: Option<(u32, gio::DBusMethodInvocation)>,
    hold_close: Option<u32>,
    notifications: Vec<Value>,
    closes: Vec<u32>,
    signals: Vec<Value>,
}

struct FdoServer {
    connection: gio::DBusConnection,
    registration: Option<gio::RegistrationId>,
    state: Rc<RefCell<State>>,
}

impl FdoServer {
    fn new() -> Self {
        let connection = gio::bus_get_sync(gio::BusType::Session, gio::Cancellable::NONE).unwrap();
        let node = gio::DBusNodeInfo::for_xml(XML).unwrap();
        let state = Rc::new(RefCell::new(State::default()));
        let registration = connection
            .register_object(PATH, &node.interfaces()[0])
            .method_call({
                let state = state.clone();
                move |_, _, _, _, method, parameters, invocation| match method {
                    "Notify" => {
                        let mut state = state.borrow_mut();
                        assert!(
                            state.pending.is_none(),
                            "only one delayed Notify is in flight"
                        );
                        let id = 40 + state.next_id;
                        state.next_id += 1;
                        let hints = parameters
                            .child_value(6)
                            .get::<BTreeMap<String, glib::Variant>>()
                            .unwrap();
                        state.notifications.push(json!({
                            "method": "Notify",
                            "reply_held": true,
                            "assigned_id": id,
                            "app_name": parameters.child_value(0).get::<String>().unwrap(),
                            "replace_id": parameters.child_value(1).get::<u32>().unwrap(),
                            "app_icon": parameters.child_value(2).get::<String>().unwrap(),
                            "title": parameters.child_value(3).get::<String>().unwrap(),
                            "body": parameters.child_value(4).get::<String>().unwrap(),
                            "actions": parameters.child_value(5).get::<Vec<String>>().unwrap(),
                            "hint_names": hints.keys().cloned().collect::<Vec<_>>(),
                            "expire_timeout": parameters.child_value(7).get::<i32>().unwrap(),
                        }));
                        state.pending = Some(Pending { id, invocation });
                    }
                    "CloseNotification" => {
                        let (id,) = parameters.get::<(u32,)>().unwrap();
                        let mut state = state.borrow_mut();
                        state.closes.push(id);
                        if state.hold_close == Some(id) {
                            state.hold_close = None;
                            assert!(
                                state.pending_close.is_none(),
                                "only one CloseNotification is held"
                            );
                            state.pending_close = Some((id, invocation));
                        } else {
                            invocation.return_value(Some(&().to_variant()));
                        }
                    }
                    other => panic!("unexpected FDO method {other}"),
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
                Some(&(FDO, 4_u32).to_variant()),
                None,
                gio::DBusCallFlags::NONE,
                5_000,
                gio::Cancellable::NONE,
            )
            .unwrap();
        assert_eq!(reply.get::<(u32,)>(), Some((1,)));
        Self {
            connection,
            registration: Some(registration),
            state,
        }
    }

    fn notifications(&self) -> Vec<Value> {
        self.state.borrow().notifications.clone()
    }

    fn last_notify(&self) -> Option<Value> {
        self.state.borrow().notifications.last().cloned()
    }

    fn release_notify(&self) -> u32 {
        let pending = self
            .state
            .borrow_mut()
            .pending
            .take()
            .expect("delayed FDO Notify");
        let id = pending.id;
        pending.invocation.return_value(Some(&(id,).to_variant()));
        id
    }

    fn emit_token(&self, id: u32) {
        self.state.borrow_mut().signals.push(json!({
            "signal": "ActivationToken",
            "server_id": id,
            "token": TOKEN,
        }));
        self.connection
            .emit_signal(
                None,
                PATH,
                FDO,
                "ActivationToken",
                Some(&(id, TOKEN).to_variant()),
            )
            .unwrap();
    }

    fn emit_action(&self, id: u32) {
        self.state.borrow_mut().signals.push(json!({
            "signal": "ActionInvoked",
            "server_id": id,
            "action": "default",
        }));
        self.connection
            .emit_signal(
                None,
                PATH,
                FDO,
                "ActionInvoked",
                Some(&(id, "default").to_variant()),
            )
            .unwrap();
    }

    fn closes(&self) -> Vec<u32> {
        self.state.borrow().closes.clone()
    }

    fn hold_close(&self, id: u32) {
        self.state.borrow_mut().hold_close = Some(id);
    }

    fn close_pending(&self, id: u32) -> bool {
        self.state
            .borrow()
            .pending_close
            .as_ref()
            .is_some_and(|(pending, _)| *pending == id)
    }

    fn release_close(&self, id: u32) {
        let (pending, invocation) = self
            .state
            .borrow_mut()
            .pending_close
            .take()
            .expect("held CloseNotification");
        assert_eq!(pending, id);
        invocation.return_value(Some(&().to_variant()));
    }

    fn raw(&self) -> Value {
        json!({
            "notifications": self.notifications(),
            "closes": self.closes(),
            "signals": self.state.borrow().signals.clone(),
        })
    }

    fn stop(&mut self) {
        if let Some(registration) = self.registration.take() {
            let _ = self.connection.call_sync(
                Some("org.freedesktop.DBus"),
                "/org/freedesktop/DBus",
                "org.freedesktop.DBus",
                "ReleaseName",
                Some(&(FDO,).to_variant()),
                None,
                gio::DBusCallFlags::NONE,
                5_000,
                gio::Cancellable::NONE,
            );
            let _ = self.connection.unregister_object(registration);
        }
    }
}

impl Drop for FdoServer {
    fn drop(&mut self) {
        self.stop();
    }
}

#[test]
#[ignore = "requires an owned X11 display, private session bus and xdotool"]
fn real_fdo_action_invoked_with_token_focuses_exact_pane_and_preserves_zoom_closed_and_closing_targets(
) {
    let config = std::path::PathBuf::from(std::env::var_os("XDG_CONFIG_HOME").unwrap());
    crate::test_isolation::assert_below_check_root(&config);
    assert!(
        !gtk::is_initialized(),
        "FDO window test must run in a fresh process"
    );
    // The private bus has only this FDO owner. Do not force a route through
    // production environment handling; the real Notify below proves the route.
    std::env::remove_var("GNOTIFICATION_BACKEND");
    std::env::remove_var("GIO_USE_PORTALS");
    let fdo = FdoServer::new();

    let application = adw::Application::builder().application_id(APP_ID).build();
    application.register(gio::Cancellable::NONE).unwrap();
    let connection = application
        .dbus_connection()
        .expect("registered application has a D-Bus connection");
    let object_path = application
        .dbus_object_path()
        .expect("registered application has an object path")
        .to_string();
    // add_filter is called on Gio's dedicated message-handling thread. Only
    // owned strings and a bounded SyncSender cross that boundary.
    let (activation_tx, activation_rx) = std::sync::mpsc::sync_channel::<ObservedActivation>(8);
    let filter = connection.add_filter(move |_, message, incoming| {
        if message.message_type() == gio::DBusMessageType::MethodCall
            && message.interface().as_deref() == Some("org.freedesktop.Application")
            && message.path().as_deref() == Some(object_path.as_str())
            && message.member().as_deref() == Some("ActivateAction")
        {
            let body = message.body();
            if let Some((action, arguments, platform)) = body.and_then(|body| {
                body.get::<(String, Vec<glib::Variant>, BTreeMap<String, glib::Variant>)>()
            }) {
                if action == "focus-pane" {
                    let target = arguments
                        .first()
                        .map(|target| target.print(true).to_string())
                        .unwrap_or_default();
                    let token = platform
                        .get("activation-token")
                        .and_then(|value| value.get::<String>());
                    let _ = activation_tx.try_send(ObservedActivation {
                        incoming,
                        target,
                        token,
                    });
                }
            }
        }
        Some(message.clone())
    });
    let ctx = build_with_writer(&application, None, crate::session::Writer::new).unwrap();
    wait(
        || ctx.state.borrow().session_ready,
        "initial terminal ready",
    );
    activate(&ctx.window, "initial-main");
    let backend = ctx.state.borrow().bells.clone().unwrap();
    wait(
        || backend.desktop_report().is_none(),
        "FDO notification service observed",
    );

    let first = ctx.state.borrow().workspaces.active().focused;
    ctx.state
        .borrow_mut()
        .workspaces
        .active_mut()
        .rename("fdo-window");
    terminal(&ctx, first)
        .feed_child(b"printf '\\033]0;make\\007\\033]7;file://localhost/work\\007'\n");
    wait(
        || {
            ctx.state
                .borrow()
                .pane(first)
                .and_then(|pane| pane.title())
                .as_deref()
                == Some("make")
        },
        "controlled terminal title",
    );
    wait(
        || {
            ctx.state
                .borrow()
                .pane(first)
                .and_then(|pane| pane.cwd())
                .as_deref()
                == Some("/work")
        },
        "controlled terminal directory",
    );
    let other = gtk::Window::builder()
        .title("Outside Among FDO")
        .default_width(300)
        .default_height(160)
        .build();
    other.present();
    activate(&other, "background-before-fdo-bell");
    wait(
        || !backend.focus().application_active,
        "Among in background before FDO bell",
    );
    real_bell(&ctx, &backend, first);
    wait(
        || fdo.notifications().len() == 1,
        "actual FDO Notify reached the private service",
    );
    let first_raw = fdo.last_notify().expect("first FDO Notify");
    assert_eq!(first_raw["method"], "Notify");
    assert_eq!(first_raw["title"], "fdo-window");
    assert_eq!(
        first_raw["body"], "Bell in 'make' /work",
        "actual FDO body must use the controlled terminal title and directory"
    );
    assert_eq!(first_raw["actions"], json!(["default", ""]));
    assert_eq!(first_raw["expire_timeout"], -1);
    let hints = first_raw["hint_names"].as_array().unwrap();
    for expected in ["desktop-entry", "urgency", "image-path"] {
        assert!(
            hints.iter().any(|hint| hint == expected),
            "raw FDO hint {expected} missing"
        );
    }
    let first_id = first_raw["assigned_id"].as_u64().unwrap() as u32;
    let first_target = backend.desktop_target(first.0).expect("first FDO target");

    fdo.release_notify();
    pump_for(std::time::Duration::from_millis(120));
    fdo.emit_token(first_id);
    pump_for(std::time::Duration::from_millis(80));
    fdo.emit_action(first_id);
    wait(
        || ctx.window.is_active() && terminal(&ctx, first).has_focus(),
        "actual FDO ActionInvoked focused the exact pane",
    );
    assert_eq!(ctx.state.borrow().workspaces.active_index(), 0);
    let mut activations = Vec::new();
    while let Ok(activation) = activation_rx.try_recv() {
        activations.push(activation);
    }
    let expected_target = first_target.print(true).to_string();
    let observed = activations
        .iter()
        .find(|activation| activation.target == expected_target)
        .expect("actual ActivateAction for the exact target");
    assert_eq!(
        observed.token.as_deref(),
        Some(TOKEN),
        "ActivateAction platform data did not carry the FDO activation token"
    );

    // Runtime::observe withdraws the notification after its first successful
    // click, so every later user-visible click needs a fresh real bell and a
    // fresh FDO server id.
    let new_bell = |pane: PaneId, phase: &str| -> (u32, glib::Variant) {
        let before = fdo.notifications().len();
        activate(&other, phase);
        wait(
            || !backend.focus().application_active,
            "Among in background before a fresh FDO bell",
        );
        real_bell(&ctx, &backend, pane);
        wait(
            || fdo.notifications().len() == before + 1,
            "fresh actual FDO Notify",
        );
        let raw = fdo.last_notify().expect("fresh FDO Notify");
        let id = raw["assigned_id"].as_u64().unwrap() as u32;
        fdo.release_notify();
        pump_for(std::time::Duration::from_millis(120));
        let target = backend.desktop_target(pane.0).expect("fresh FDO target");
        (id, target)
    };

    new_workspace(&ctx);
    wait(
        || ctx.state.borrow().workspaces.len() == 2,
        "second workspace",
    );
    set_sidebar_collapsed(&ctx, true);
    let (cross_id, _) = new_bell(first, "background-cross-workspace-fdo-bell");
    fdo.emit_action(cross_id);
    wait(
        || ctx.state.borrow().workspaces.active_index() == 0 && terminal(&ctx, first).has_focus(),
        "FDO action focuses the exact pane across workspaces",
    );
    assert!(ctx.state.borrow().sidebar_collapsed);

    split_focused(&ctx);
    let sibling = ctx.state.borrow().workspaces.active().focused;
    toggle_zoom(&ctx);
    assert_eq!(ctx.state.borrow().workspaces.active().zoomed, Some(sibling));
    let (zoom_id, _) = new_bell(first, "background-zoom-fdo-bell");
    fdo.emit_action(zoom_id);
    wait(
        || {
            ctx.state.borrow().workspaces.active().zoomed.is_none()
                && terminal(&ctx, first).has_focus()
        },
        "FDO action restores the hidden sibling and focuses the target",
    );

    let (closed_id, closed_target) = new_bell(sibling, "background-closed-fdo-bell");
    fdo.hold_close(closed_id);
    activate(&other, "background-before-closed-fdo-target");
    wait(
        || !backend.focus().application_active,
        "Among in background before the closed-target click",
    );
    let selected = ctx.state.borrow().workspaces.active().focused;
    close_pane(&ctx, sibling);
    wait(
        || fdo.close_pending(closed_id),
        "FDO CloseNotification for the closed target is held",
    );
    fdo.emit_action(closed_id);
    pump_for(std::time::Duration::from_millis(300));
    assert!(
        ctx.window.is_active(),
        "click while the terminal close is pending must raise the window"
    );
    assert_eq!(
        ctx.state.borrow().workspaces.active().focused,
        selected,
        "click while the terminal close is pending changed the selected pane"
    );
    assert_eq!(
        ctx.state.borrow().workspaces.active().zoomed,
        None,
        "click while the terminal close is pending changed zoom"
    );
    fdo.release_close(closed_id);
    pump_for(std::time::Duration::from_millis(120));
    // Once deletion is confirmed, an arbitrary old ActionInvoked is not a
    // user-visible banner and must not navigate.
    fdo.emit_action(closed_id);
    pump_for(std::time::Duration::from_millis(150));
    assert_eq!(
        ctx.state.borrow().workspaces.active().focused,
        selected,
        "confirmed removed notification navigated on a stale ActionInvoked"
    );

    let (closing_id, closing_target) = new_bell(first, "background-closing-fdo-bell");
    closing_activation_preserves_the_final_snapshot_until_close_is_cancelled(
        &ctx,
        &other,
        &closing_target,
        first,
        &|| fdo.emit_action(closing_id),
    );

    // The filter closure owns only a bounded SyncSender, so an asynchronous
    // release after removal cannot touch GTK or a main-thread object.
    connection.remove_filter(filter);
    while let Ok(activation) = activation_rx.try_recv() {
        activations.push(activation);
    }
    let activations_json = activations
        .iter()
        .map(|activation| {
            json!({
                "incoming": activation.incoming,
                "target": activation.target,
                "token": activation.token,
            })
        })
        .collect::<Vec<_>>();
    let evidence =
        crate::test_isolation::check_root().join(format!("fdo-window-{}", std::process::id()));
    crate::test_isolation::assert_below_check_root(&evidence);
    std::fs::create_dir_all(&evidence).unwrap();
    std::fs::write(
        evidence.join("observations.json"),
        serde_json::to_vec_pretty(&json!({
            "notify": first_raw,
            "raw_bus": fdo.raw(),
            "activations": activations_json,
            "first_target": first_target.print(true).to_string(),
            "closed_target": closed_target.print(true).to_string(),
            "active": ctx.window.is_active(),
            "workspace": ctx.state.borrow().workspaces.active_index(),
            "focused": ctx.state.borrow().workspaces.active().focused.0,
            "zoomed": ctx.state.borrow().workspaces.active().zoomed.map(|pane| pane.0),
        }))
        .unwrap(),
    )
    .unwrap();
    other.destroy();
    ctx.window.destroy();
    println!("PASS actual FDO Notify/ActionInvoked with activation token focuses the exact pane, preserves zoom, stale/closed target state and closing focus");
}
