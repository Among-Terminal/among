use super::super::{GioSender, Route, Services, FDO, GTK, PORTAL};
use super::serialized_payload;
use among::notifications::desktop::{Sender, Target, Text};
use gio::prelude::*;
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::rc::Rc;
use std::time::{Duration, Instant};

const ROLE: &str = "AMONG_NAMED_TEST_ROLE";
const ROOT: &str = "AMONG_NAMED_TEST_ROOT";
const GTK_TEST: &str = "app::bell::desktop::named::tests::gtk_named_private_bus";
const PORTAL_TEST: &str = "app::bell::desktop::named::tests::portal_named_private_bus";

const GTK_PATH: &str = "/org/gtk/Notifications";
const GTK_INTERFACE: &str = "org.gtk.Notifications";
const PORTAL_PATH: &str = "/org/freedesktop/portal/desktop";
const PORTAL_INTERFACE: &str = "org.freedesktop.portal.Notification";

fn target() -> Target {
    Target {
        instance: "named-test".to_string(),
        session: 1,
        generation: 2,
        pane: 3,
    }
}

fn text(title: &str, body: &str) -> Text {
    Text {
        title: title.to_string(),
        body: body.to_string(),
    }
}

fn pump() {
    let context = glib::MainContext::default();
    while context.pending() {
        context.iteration(false);
    }
}

fn wait(mut predicate: impl FnMut() -> bool, reason: &str) {
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        if predicate() {
            return;
        }
        assert!(Instant::now() < deadline, "timed out: {reason}");
        glib::MainContext::default().iteration(false);
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn wait_file_blocking(path: &Path, reason: &str) {
    let deadline = Instant::now() + Duration::from_secs(8);
    while !path.exists() {
        assert!(Instant::now() < deadline, "timed out: {reason}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

struct OwnedChild(Child);

impl Drop for OwnedChild {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn root(name: &str) -> PathBuf {
    let path =
        crate::test_isolation::check_root().join(format!("named-{}-{name}", std::process::id()));
    crate::test_isolation::assert_below_check_root(&path);
    fs::create_dir(&path).expect("named evidence root already exists");
    path
}

fn spawn_role(root: &Path, role: &str, test: &str) -> OwnedChild {
    let executable = std::env::current_exe().unwrap();
    let log = fs::File::create(root.join(format!("{role}.log"))).unwrap();
    OwnedChild(
        Command::new(executable)
            .args([
                "--exact",
                test,
                "--ignored",
                "--test-threads=1",
                "--nocapture",
            ])
            .env(ROLE, role)
            .env(ROOT, root)
            .stdin(Stdio::null())
            .stdout(Stdio::from(log.try_clone().unwrap()))
            .stderr(Stdio::from(log))
            .spawn()
            .unwrap(),
    )
}

fn entries(root: &Path, role: &str) -> Vec<serde_json::Value> {
    fs::read_to_string(root.join(format!("{role}.jsonl")))
        .unwrap_or_default()
        .lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect()
}

fn has(root: &Path, role: &str, method: &str, id: &str) -> bool {
    entries(root, role)
        .iter()
        .any(|entry| entry["method"] == method && entry["id"] == id)
}

fn append(path: &Path, value: &serde_json::Value) {
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .unwrap();
    writeln!(file, "{value}").unwrap();
}

fn connection() -> gio::DBusConnection {
    gio::bus_get_sync(gio::BusType::Session, gio::Cancellable::NONE).unwrap()
}

fn release_gtk(connection: &gio::DBusConnection) {
    connection
        .call_sync(
            Some(GTK),
            GTK_PATH,
            GTK_INTERFACE,
            "ReleaseName",
            None,
            None,
            gio::DBusCallFlags::NONE,
            5_000,
            gio::Cancellable::NONE,
        )
        .unwrap();
}

fn release_portal_remove(connection: &gio::DBusConnection) {
    connection
        .call_sync(
            Some(PORTAL),
            PORTAL_PATH,
            PORTAL_INTERFACE,
            "ReleaseRemove",
            None,
            None,
            gio::DBusCallFlags::NONE,
            5_000,
            gio::Cancellable::NONE,
        )
        .unwrap();
}

fn release_portal_add(connection: &gio::DBusConnection) {
    connection
        .call_sync(
            Some(PORTAL),
            PORTAL_PATH,
            PORTAL_INTERFACE,
            "ReleaseAdd",
            None,
            None,
            gio::DBusCallFlags::NONE,
            5_000,
            gio::Cancellable::NONE,
        )
        .unwrap();
}

fn serve(role: &str) {
    let root = PathBuf::from(std::env::var_os(ROOT).unwrap());
    let connection = connection();
    let (name, path, _interface, xml) = if role == "fdo" {
        (
            FDO,
            "/org/freedesktop/Notifications",
            FDO,
            r#"<node><interface name="org.freedesktop.Notifications">
<method name="Notify"><arg type="s" direction="in"/><arg type="u" direction="in"/><arg type="s" direction="in"/><arg type="s" direction="in"/><arg type="s" direction="in"/><arg type="as" direction="in"/><arg type="a{sv}" direction="in"/><arg type="i" direction="in"/><arg type="u" direction="out"/></method>
<method name="CloseNotification"><arg type="u" direction="in"/></method>
</interface></node>"#,
        )
    } else if role.starts_with("gtk-malformed") {
        (
            GTK,
            GTK_PATH,
            GTK_INTERFACE,
            r#"<node><interface name="org.gtk.Notifications">
<method name="AddNotification"><arg type="s" direction="in"/><arg type="s" direction="in"/><arg type="a{sv}" direction="in"/><arg type="s" direction="out"/></method>
<method name="RemoveNotification"><arg type="s" direction="in"/><arg type="s" direction="in"/><arg type="s" direction="out"/></method>
<method name="ReleaseName"/>
</interface></node>"#,
        )
    } else if role.starts_with("gtk") {
        (
            GTK,
            GTK_PATH,
            GTK_INTERFACE,
            r#"<node><interface name="org.gtk.Notifications">
<method name="AddNotification"><arg type="s" direction="in"/><arg type="s" direction="in"/><arg type="a{sv}" direction="in"/></method>
<method name="RemoveNotification"><arg type="s" direction="in"/><arg type="s" direction="in"/></method>
<method name="ReleaseName"/>
</interface></node>"#,
        )
    } else {
        (
            PORTAL,
            PORTAL_PATH,
            PORTAL_INTERFACE,
            r#"<node><interface name="org.freedesktop.portal.Notification">
<method name="AddNotification"><arg type="s" direction="in"/><arg type="a{sv}" direction="in"/></method>
<method name="RemoveNotification"><arg type="s" direction="in"/></method>
<method name="ReleaseAdd"/><method name="ReleaseRemove"/>
</interface></node>"#,
        )
    };
    let node = gio::DBusNodeInfo::for_xml(xml).unwrap();
    let log = root.join(format!("{role}.jsonl"));
    let pending_add: Rc<RefCell<Option<gio::DBusMethodInvocation>>> = Rc::new(RefCell::new(None));
    let pending_remove: Rc<RefCell<Option<gio::DBusMethodInvocation>>> =
        Rc::new(RefCell::new(None));
    let role_name = role.to_string();
    let role = role_name.clone();
    let bus = connection.clone();
    let _registration = connection
        .register_object(path, &node.interfaces()[0])
        .method_call(
            move |_, _, _, _, method, parameters, invocation| match method {
                "AddNotification" if role.starts_with("gtk") => {
                    let (application, id, payload) = parameters
                        .get::<(String, String, BTreeMap<String, glib::Variant>)>()
                        .unwrap();
                    let title = payload
                        .get("title")
                        .and_then(|value| value.get::<String>())
                        .unwrap_or_default();
                    let body = payload
                        .get("body")
                        .and_then(|value| value.get::<String>())
                        .unwrap_or_default();
                    let action = payload
                        .get("default-action")
                        .and_then(|value| value.get::<String>())
                        .unwrap_or_default();
                    let priority = payload
                        .get("priority")
                        .and_then(|value| value.get::<String>())
                        .unwrap_or_default();
                    let target = payload
                        .get("default-action-target")
                        .and_then(|value| value.get::<(String, u64, u64, u64)>());
                    append(
                        &log,
                        &serde_json::json!({
                            "method": method, "application": application, "id": id,
                            "title": title, "body": body, "action": action,
                            "priority": priority, "target": target,
                            "has_icon": payload.contains_key("icon")
                        }),
                    );
                    if role.starts_with("gtk-error") {
                        invocation.return_dbus_error(
                            "org.freedesktop.DBus.Error.Failed",
                            "raw Add failure",
                        );
                    } else if role.starts_with("gtk-malformed") {
                        invocation.return_value(Some(&("raw-malformed-add",).to_variant()));
                    } else {
                        invocation.return_value(Some(&().to_variant()));
                    }
                }
                "AddNotification" => {
                    let (id, payload) = parameters
                        .get::<(String, BTreeMap<String, glib::Variant>)>()
                        .unwrap();
                    let title = payload
                        .get("title")
                        .and_then(|value| value.get::<String>())
                        .unwrap_or_default();
                    let action = payload
                        .get("default-action")
                        .and_then(|value| value.get::<String>())
                        .unwrap_or_default();
                    append(
                        &log,
                        &serde_json::json!({
                            "method": method, "id": id, "title": title,
                            "action": action, "has_icon": payload.contains_key("icon")
                        }),
                    );
                    *pending_add.borrow_mut() = Some(invocation);
                }
                "RemoveNotification" if role.starts_with("gtk") => {
                    let (application, id) = parameters.get::<(String, String)>().unwrap();
                    append(
                        &log,
                        &serde_json::json!({
                            "method": method, "application": application, "id": id
                        }),
                    );
                    if role.starts_with("gtk-error") {
                        invocation.return_dbus_error(
                            "org.freedesktop.DBus.Error.Failed",
                            "raw Remove failure",
                        );
                    } else if role.starts_with("gtk-malformed") {
                        invocation.return_value(Some(&("raw-malformed-remove",).to_variant()));
                    } else {
                        invocation.return_value(Some(&().to_variant()));
                    }
                }
                "RemoveNotification" => {
                    let (id,) = parameters.get::<(String,)>().unwrap();
                    append(&log, &serde_json::json!({"method": method, "id": id}));
                    if role.starts_with("portal") {
                        *pending_remove.borrow_mut() = Some(invocation);
                    } else {
                        invocation.return_value(Some(&().to_variant()));
                    }
                }
                "Notify" if role.as_str() == "fdo" => {
                    append(&log, &serde_json::json!({"method": method}));
                    invocation.return_value(Some(&(99_u32,).to_variant()));
                }
                "CloseNotification" if role.as_str() == "fdo" => {
                    let (id,) = parameters.get::<(u32,)>().unwrap();
                    append(
                        &log,
                        &serde_json::json!({"method": method, "server_id": id}),
                    );
                    invocation.return_value(Some(&().to_variant()));
                }
                "ReleaseName" => {
                    bus.call_sync(
                        Some("org.freedesktop.DBus"),
                        "/org/freedesktop/DBus",
                        "org.freedesktop.DBus",
                        "ReleaseName",
                        Some(&(name,).to_variant()),
                        None,
                        gio::DBusCallFlags::NONE,
                        5_000,
                        gio::Cancellable::NONE,
                    )
                    .unwrap();
                    invocation.return_value(Some(&().to_variant()));
                }
                "ReleaseAdd" => {
                    if let Some(pending) = pending_add.borrow_mut().take() {
                        pending.return_value(Some(&().to_variant()));
                    }
                    invocation.return_value(Some(&().to_variant()));
                }
                "ReleaseRemove" => {
                    if let Some(pending) = pending_remove.borrow_mut().take() {
                        pending.return_value(Some(&().to_variant()));
                    }
                    invocation.return_value(Some(&().to_variant()));
                }
                _ => panic!("unexpected named fixture method {method}"),
            },
        )
        .build()
        .unwrap();
    let reply = connection
        .call_sync(
            Some("org.freedesktop.DBus"),
            "/org/freedesktop/DBus",
            "org.freedesktop.DBus",
            "RequestName",
            Some(&(name, 4_u32).to_variant()),
            None,
            gio::DBusCallFlags::NONE,
            5_000,
            gio::Cancellable::NONE,
        )
        .unwrap();
    assert_eq!(reply.get::<(u32,)>(), Some((1,)));
    fs::write(root.join(format!("{role_name}.ready")), b"").unwrap();
    let loop_ = glib::MainLoop::new(None, false);
    loop_.run();
}

#[test]
fn payload_keeps_the_public_gtk_contract() {
    let payload = serialized_payload(&text("build", "Bell in 'make'"), &target());
    let map = payload
        .get::<BTreeMap<String, glib::Variant>>()
        .expect("a{sv}");
    assert_eq!(
        map.get("title").and_then(|value| value.get::<String>()),
        Some("build".to_string())
    );
    assert_eq!(
        map.get("body").and_then(|value| value.get::<String>()),
        Some("Bell in 'make'".to_string())
    );
    assert_eq!(
        map.get("priority").and_then(|value| value.get::<String>()),
        Some("normal".to_string())
    );
    assert_eq!(
        map.get("default-action")
            .and_then(|value| value.get::<String>()),
        Some("app.focus-pane".to_string())
    );
    assert_eq!(
        map.get("default-action-target")
            .and_then(|value| value.get::<(String, u64, u64, u64)>()),
        Some(("named-test".to_string(), 1, 2, 3))
    );
    assert!(map.contains_key("icon"));
}

#[test]
fn parameters_keep_the_dbus_tuple_and_payload_types() {
    let payload = serialized_payload(&text("build", "body"), &target());
    assert_eq!(payload.type_().as_str(), "a{sv}");
    assert_eq!(
        super::gtk_add_parameters("com.github.among-terminal", "pane", payload.clone())
            .type_()
            .as_str(),
        "(ssa{sv})"
    );
    assert_eq!(
        super::portal_add_parameters("pane", payload)
            .type_()
            .as_str(),
        "(sa{sv})"
    );
    assert_eq!(
        super::gtk_remove_parameters("com.github.among-terminal", "pane")
            .type_()
            .as_str(),
        "(ss)"
    );
    assert_eq!(
        super::portal_remove_parameters("pane").type_().as_str(),
        "(s)"
    );
}

#[test]
fn forced_gtk_fallback_is_portal_and_does_not_switch_when_gtk_appears() {
    let mut services = Services::new(true, Some("gtk".to_string()));
    services.owners.insert(GTK, Some(false));
    services.owners.insert(super::super::FDO, Some(false));
    services.activatable = Some(vec![PORTAL.to_string()]);
    assert_eq!(services.freeze(), Route::Portal);
    services.owners.insert(GTK, Some(true));
    assert_eq!(services.effective_route(), Route::Portal);
    assert_eq!(services.freeze(), Route::Portal);
}

#[test]
#[ignore = "requires an isolated private session bus and fake named services"]
fn gtk_named_private_bus() {
    if let Ok(role) = std::env::var(ROLE) {
        serve(&role);
        return;
    }
    std::env::remove_var("GNOTIFICATION_BACKEND");
    std::env::remove_var("GIO_USE_PORTALS");
    let root = root("gtk");
    let bus = gio::TestDBus::new(gio::TestDBusFlags::NONE);
    bus.up();
    let _fdo = spawn_role(&root, "fdo", GTK_TEST);
    wait(|| root.join("fdo.ready").exists(), "FDO recorder owner");
    let _a = spawn_role(&root, "gtk-a", GTK_TEST);
    wait(|| root.join("gtk-a.ready").exists(), "first GTK owner");
    let app = gio::Application::builder()
        .application_id("com.github.among-terminal.named-test")
        .flags(gio::ApplicationFlags::NON_UNIQUE)
        .build();
    app.register(gio::Cancellable::NONE).unwrap();
    let mut sender = GioSender::for_application(Some(app.clone()));
    wait(
        || sender.availability() == among::notifications::desktop::Availability::Available,
        "GTK named route",
    );

    // Discovery knows A. Release A from an independent connection and keep the
    // main context uniterated, so the first freeze still selects Gtk with A.
    std::thread::spawn(|| {
        let connection = connection();
        release_gtk(&connection);
    })
    .join()
    .unwrap();
    sender.attempt("pane-1", &text("build", "stale first"), &target());
    wait(
        || has(&root, "gtk-a", "AddNotification", "pane-1"),
        "stale first Add reaches old unique A",
    );
    let add = entries(&root, "gtk-a")
        .into_iter()
        .find(|entry| entry["method"] == "AddNotification" && entry["id"] == "pane-1")
        .unwrap();
    assert_eq!(add["title"], "build");
    assert_eq!(add["action"], "app.focus-pane");
    assert_eq!(add["priority"], "normal");
    assert_eq!(add["target"], serde_json::json!(["named-test", 1, 2, 3]));
    assert_eq!(add["has_icon"].as_bool(), Some(true));
    assert!(
        !has(&root, "fdo", "Notify", "pane-1"),
        "stale GTK discovery must not fall back to FDO"
    );

    let b = spawn_role(&root, "gtk-b", GTK_TEST);
    wait(|| root.join("gtk-b.ready").exists(), "second GTK owner");
    wait(|| sender.gtk_owner_for_test().is_some(), "B bound");
    sender.attempt(
        "pane-1",
        &text("build", "same id after owner change"),
        &target(),
    );
    wait(
        || has(&root, "gtk-a", "RemoveNotification", "pane-1"),
        "old id removed from A before the new Add",
    );
    wait(
        || has(&root, "gtk-b", "AddNotification", "pane-1"),
        "same id added to B",
    );
    assert!(
        !has(&root, "gtk-b", "RemoveNotification", "pane-1"),
        "old owner removal must not be redirected to B"
    );
    sender.withdraw("pane-1");
    wait(
        || has(&root, "gtk-b", "RemoveNotification", "pane-1"),
        "current owner removes the id",
    );
    wait(|| sender.is_quiescent(), "GTK named sender quiescent");

    drop(b);
    wait(|| sender.gtk_owner_for_test().is_none(), "B released");
    let c = spawn_role(&root, "gtk-error", GTK_TEST);
    wait(|| root.join("gtk-error.ready").exists(), "error GTK owner");
    wait(
        || sender.gtk_owner_for_test().is_some(),
        "error owner bound",
    );
    sender.attempt("error-add", &text("build", "error Add"), &target());
    wait(
        || sender.named_error_counts_for_test().0 == 1,
        "raw Add error observed",
    );
    sender.withdraw("error-add");
    wait(
        || sender.named_error_counts_for_test().1 == 1,
        "raw Remove error observed",
    );
    wait(|| sender.is_quiescent(), "error replies settle");
    drop(c);
    wait(
        || sender.gtk_owner_for_test().is_none(),
        "error owner released",
    );

    let d = spawn_role(&root, "gtk-malformed", GTK_TEST);
    wait(
        || root.join("gtk-malformed.ready").exists(),
        "malformed GTK owner",
    );
    wait(
        || sender.gtk_owner_for_test().is_some(),
        "malformed owner bound",
    );
    sender.attempt("malformed-add", &text("build", "malformed Add"), &target());
    wait(
        || sender.named_error_counts_for_test().0 == 2,
        "malformed Add reply observed",
    );
    sender.withdraw("malformed-add");
    wait(
        || sender.named_error_counts_for_test().1 == 2,
        "malformed Remove reply observed",
    );
    wait(|| sender.is_quiescent(), "malformed replies settle");
    assert!(
        !entries(&root, "fdo")
            .iter()
            .any(|entry| entry["method"] == "Notify"),
        "named routes must never reach the FDO recorder"
    );
    drop(d);
    drop(sender);
    drop(app);
    drop(_a);
    drop(_fdo);
    pump();
    bus.down();
}

#[test]
#[ignore = "requires an isolated private session bus with an activatable portal stub"]
fn portal_named_private_bus() {
    if let Ok(role) = std::env::var(ROLE) {
        serve(&role);
        return;
    }
    std::env::set_var("GIO_USE_PORTALS", "1");
    std::env::set_var("GNOTIFICATION_BACKEND", "gtk");
    let root = root("portal");
    let services = root.join("services");
    fs::create_dir_all(&services).unwrap();
    let executable = glib::shell_quote(std::env::current_exe().unwrap())
        .into_string()
        .unwrap();
    fs::write(
        services.join(format!("{PORTAL}.service")),
        format!(
            "[D-BUS Service]\nName={PORTAL}\nExec=/usr/bin/env {ROLE}=portal {ROOT}={} {executable} --exact {PORTAL_TEST} --ignored --test-threads=1 --nocapture\n",
            root.display()
        ),
    )
    .unwrap();
    let bus = gio::TestDBus::new(gio::TestDBusFlags::NONE);
    bus.add_service_dir(services.to_str().unwrap());
    bus.up();
    let _fdo = spawn_role(&root, "fdo", PORTAL_TEST);
    wait(|| root.join("fdo.ready").exists(), "FDO recorder owner");
    let app = gio::Application::builder()
        .application_id("com.github.among-terminal.named-portal-test")
        .flags(gio::ApplicationFlags::NON_UNIQUE)
        .build();
    app.register(gio::Cancellable::NONE).unwrap();
    let mut sender = GioSender::for_application(Some(app.clone()));
    wait(
        || sender.availability() == among::notifications::desktop::Availability::Available,
        "portal named route",
    );

    // Discovery knows GTK is absent. A peer appears, but the main context is
    // not iterated before the first attempt, so the forced-GTK fallback stays
    // Portal instead of independently selecting GTK.
    let gtk_peer = spawn_role(&root, "gtk-peer", PORTAL_TEST);
    wait_file_blocking(&root.join("gtk-peer.ready"), "late GTK peer");
    sender.attempt("forced-race", &text("build", "forced race"), &target());
    wait(
        || has(&root, "portal", "AddNotification", "forced-race"),
        "forced-GTK fallback Add reaches portal",
    );
    assert!(
        !has(&root, "gtk-peer", "AddNotification", "forced-race"),
        "late GTK peer must not receive the first attempt"
    );
    assert!(
        !has(&root, "fdo", "Notify", "forced-race"),
        "forced-GTK fallback must not reach FDO"
    );
    sender.withdraw("forced-race");
    wait(
        || has(&root, "portal", "RemoveNotification", "forced-race"),
        "forced-GTK fallback early Remove",
    );
    release_portal_add(&connection());
    release_portal_remove(&connection());
    wait(|| sender.is_quiescent(), "forced-GTK fallback settles");

    sender.attempt("portal-1", &text("build", "portal"), &target());
    sender.withdraw("portal-1");
    wait(
        || {
            has(&root, "portal", "AddNotification", "portal-1")
                && has(&root, "portal", "RemoveNotification", "portal-1")
        },
        "portal autostart Add and early Remove",
    );
    let calls = entries(&root, "portal");
    let add = calls
        .iter()
        .position(|entry| entry["method"] == "AddNotification" && entry["id"] == "portal-1")
        .unwrap();
    let remove = calls
        .iter()
        .position(|entry| entry["method"] == "RemoveNotification" && entry["id"] == "portal-1")
        .unwrap();
    assert!(add < remove, "Remove arrived before Add");
    release_portal_add(&connection());
    release_portal_remove(&connection());
    wait(
        || sender.is_quiescent(),
        "first held Add and Remove complete",
    );

    sender.attempt("portal-2", &text("build", "held add"), &target());
    wait(
        || has(&root, "portal", "AddNotification", "portal-2"),
        "held Add",
    );
    sender.withdraw("portal-2");
    wait(
        || has(&root, "portal", "RemoveNotification", "portal-2"),
        "early Remove",
    );
    release_portal_remove(&connection());
    pump();
    assert!(
        !sender.is_quiescent(),
        "Remove reply alone must not prove the held Add complete"
    );
    release_portal_add(&connection());
    wait(|| sender.is_quiescent(), "held Add completes after Remove");

    sender.attempt("portal-3", &text("build", "deadline"), &target());
    wait(
        || has(&root, "portal", "AddNotification", "portal-3"),
        "deadline Add",
    );
    sender.set_named_drain_budget_for_test(Duration::from_millis(150));
    sender.shutdown();
    assert!(
        !sender.is_quiescent(),
        "held Add and Remove must keep the named sender open"
    );
    wait(|| sender.is_quiescent(), "shared named drain budget expiry");
    assert!(
        !entries(&root, "fdo")
            .iter()
            .any(|entry| entry["method"] == "Notify"),
        "named routes must never reach the FDO recorder"
    );
    drop(sender);
    drop(app);
    drop(gtk_peer);
    drop(_fdo);
    pump();
    bus.down();
}
