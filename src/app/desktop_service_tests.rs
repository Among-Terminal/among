use super::*;
use among::notifications::desktop::Runtime;
use among::notifications::{Focus, Host};
use std::cell::Cell;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const TEST: &str = "app::bell::desktop::service_tests::real_gio_activates_the_selected_route_and_missing_service_bells_do_not_replay";
const ROLE: &str = "AMONG_DESKTOP_SERVICE_ROLE";
const CASE: &str = "AMONG_DESKTOP_SERVICE_CASE";
const ROOT: &str = "AMONG_DESKTOP_SERVICE_ROOT";
const FDO: &str = "org.freedesktop.Notifications";
const PORTAL: &str = "org.freedesktop.portal.Desktop";
const EARLY_TEST: &str = "app::bell::desktop::service_tests::real_fdo_early_withdrawal_is_measured_before_delayed_notify_reply_and_cleaned_up";

#[test]
fn portal_selection_matches_flatpak_and_gios_native_environment_switch() {
    for (flatpak, value, expected) in [
        (false, None, false),
        (false, Some(""), false),
        (false, Some("0"), false),
        (false, Some("true"), false),
        (false, Some("1"), true),
        (false, Some("1anything"), true),
        (true, None, true),
        (true, Some("0"), true),
    ] {
        assert_eq!(
            portal_route(flatpak, value.map(std::ffi::OsStr::new)),
            expected,
            "flatpak={flatpak} value={value:?}"
        );
    }
}

#[test]
fn route_freezes_by_backend_support_before_the_first_attempt() {
    let mut services = Services::new(false, None);
    assert_eq!(services.availability(), Availability::Checking);

    services.owners.insert(GTK, Some(false));
    services.owners.insert(FDO, Some(false));
    services.activatable = Some(Vec::new());
    assert_eq!(services.availability(), Availability::Unavailable);

    services.activatable = Some(vec![GTK.into(), PORTAL.into()]);
    assert_eq!(
        services.availability(),
        Availability::Unavailable,
        "activatable GTK alone is not a GIO route"
    );

    services.activatable = Some(vec![FDO.into()]);
    assert_eq!(services.availability(), Availability::Available);
    assert!(
        services.route.is_none(),
        "availability must not freeze the route"
    );
    assert_eq!(services.freeze(), Route::Fdo);

    services.owners.insert(GTK, Some(true));
    services.activatable = Some(vec![GTK.into(), PORTAL.into()]);
    assert_eq!(
        services.effective_route(),
        Route::Fdo,
        "a later GTK service does not switch the frozen route"
    );
    assert_eq!(
        services.availability(),
        Availability::Unavailable,
        "availability follows the frozen FDO route, not the newly appeared GTK service"
    );
}

#[test]
fn forced_routes_check_support_before_falling_back() {
    let mut services = Services::new(false, Some("gtk".to_string()));
    services.owners.insert(GTK, Some(false));
    services.owners.insert(FDO, Some(false));
    services.activatable = Some(vec![FDO.into()]);
    assert_eq!(services.freeze(), Route::Fdo);

    let mut services = Services::new(true, Some("gtk".to_string()));
    services.owners.insert(GTK, Some(false));
    services.activatable = Some(vec![PORTAL.into()]);
    assert_eq!(services.freeze(), Route::Portal);

    let mut services = Services::new(false, Some("portal".to_string()));
    services.owners.insert(GTK, Some(true));
    services.activatable = Some(Vec::new());
    assert_eq!(services.freeze(), Route::Gtk);

    let mut services = Services::new(false, Some("help".to_string()));
    services.owners.insert(GTK, Some(false));
    services.owners.insert(FDO, Some(false));
    services.activatable = Some(vec![FDO.into()]);
    assert_eq!(
        services.freeze(),
        Route::Fdo,
        "help must not leave an unsafe GIO-FDO route"
    );
}

struct OwnedChild(Child);

impl OwnedChild {
    fn finish(mut self) {
        let deadline = Instant::now() + Duration::from_secs(40);
        loop {
            if let Some(status) = self.0.try_wait().unwrap() {
                assert!(
                    status.success(),
                    "private service scenario failed: {status}"
                );
                return;
            }
            assert!(
                Instant::now() < deadline,
                "private service scenario timed out"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

impl Drop for OwnedChild {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn pump() {
    let context = glib::MainContext::default();
    while context.pending() {
        context.iteration(false);
    }
    std::thread::sleep(Duration::from_millis(5));
}

fn wait(mut predicate: impl FnMut() -> bool, reason: &str) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !predicate() && Instant::now() < deadline {
        pump();
    }
    assert!(predicate(), "timed out: {reason}");
}

fn daemon(root: &Path, portal: bool) {
    let connection = gio::bus_get_sync(gio::BusType::Session, gio::Cancellable::NONE).unwrap();
    let (name, path, xml) = if portal {
        (
            PORTAL,
            "/org/freedesktop/portal/desktop",
            r#"<node><interface name="org.freedesktop.portal.Notification"><method name="AddNotification"><arg type="s" direction="in"/><arg type="a{sv}" direction="in"/></method><method name="RemoveNotification"><arg type="s" direction="in"/></method><method name="Barrier"/></interface></node>"#,
        )
    } else {
        (
            FDO,
            "/org/freedesktop/Notifications",
            r#"<node><interface name="org.freedesktop.Notifications"><method name="Notify"><arg type="s" direction="in"/><arg type="u" direction="in"/><arg type="s" direction="in"/><arg type="s" direction="in"/><arg type="s" direction="in"/><arg type="as" direction="in"/><arg type="a{sv}" direction="in"/><arg type="i" direction="in"/><arg type="u" direction="out"/></method><method name="CloseNotification"><arg type="u" direction="in"/></method><method name="Barrier"/></interface></node>"#,
        )
    };
    let node = gio::DBusNodeInfo::for_xml(xml).unwrap();
    let received = root.join("received.json");
    let withdrawn = root.join("withdrawn");
    let registration = connection
        .register_object(path, &node.interfaces()[0])
        .method_call(move |_, _, _, _, method, parameters, invocation| {
            let payload = match method {
                "AddNotification" => {
                    let (_, fields) = parameters
                        .get::<(String, BTreeMap<String, glib::Variant>)>()
                        .unwrap();
                    Some((
                        fields["title"].get::<String>().unwrap(),
                        fields["body"].get::<String>().unwrap(),
                    ))
                }
                "Notify" => Some((
                    parameters.child_value(3).get::<String>().unwrap(),
                    parameters.child_value(4).get::<String>().unwrap(),
                )),
                "RemoveNotification" | "CloseNotification" => {
                    let pending = withdrawn.with_extension("pending");
                    fs::write(&pending, method).unwrap();
                    fs::rename(pending, &withdrawn).unwrap();
                    None
                }
                "Barrier" => None,
                other => panic!("unexpected fixture method {other}"),
            };
            if let Some((title, body)) = payload {
                assert!(!received.exists(), "the first bell was sent more than once");
                // The client treats publication as completion, so it must never
                // observe the JSON while this other process is still writing it.
                let pending = received.with_extension("pending");
                fs::write(
                    &pending,
                    serde_json::to_vec(
                        &serde_json::json!({"method":method,"title":title,"body":body}),
                    )
                    .unwrap(),
                )
                .unwrap();
                fs::rename(pending, &received).unwrap();
            }
            invocation.return_value(Some(&if method == "Notify" {
                (1_u32,).to_variant()
            } else {
                ().to_variant()
            }));
        })
        .build()
        .unwrap();
    let result = connection
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
    assert_eq!(
        result.get::<(u32,)>(),
        Some((1,)),
        "fixture name already owned"
    );
    let loop_ = glib::MainLoop::new(None, false);
    connection.connect_closed({
        let loop_ = loop_.clone();
        move |_, _, _| loop_.quit()
    });
    glib::timeout_add_seconds_local_once(20, {
        let loop_ = loop_.clone();
        move || loop_.quit()
    });
    loop_.run();
    let _ = connection.unregister_object(registration);
}

fn client(root: &Path, scenario: &str) {
    let bus = gio::TestDBus::new(gio::TestDBusFlags::NONE);
    let services = root.join("services");
    fs::create_dir(&services).unwrap();
    let portal = scenario == "portal";
    let service_name = if portal { PORTAL } else { FDO };
    if scenario != "missing" {
        let executable = glib::shell_quote(std::env::current_exe().unwrap())
            .into_string()
            .expect("fixture executable path must be UTF-8");
        fs::write(
            services.join(format!("{service_name}.service")),
            format!("[D-BUS Service]\nName={service_name}\nExec=/usr/bin/env {ROLE}=daemon {executable} --exact {TEST} --ignored --test-threads=1 --nocapture\n"),
        ).unwrap();
    }
    bus.add_service_dir(services.to_str().unwrap());
    bus.up();
    {
        let connection = gio::bus_get_sync(gio::BusType::Session, gio::Cancellable::NONE).unwrap();
        let owner = connection
            .call_sync(
                Some("org.freedesktop.DBus"),
                "/org/freedesktop/DBus",
                "org.freedesktop.DBus",
                "NameHasOwner",
                Some(&(service_name,).to_variant()),
                None,
                gio::DBusCallFlags::NONE,
                5_000,
                gio::Cancellable::NONE,
            )
            .unwrap();
        assert_eq!(owner.get::<(bool,)>(), Some((false,)));
        let app = gio::Application::builder()
            .application_id("com.github.among-terminal.desktop-service-test")
            .flags(gio::ApplicationFlags::NON_UNIQUE)
            .build();
        app.register(gio::Cancellable::NONE).unwrap();
        let sender = GioSender::for_application(Some(app.clone()));
        let mut host = Host::new().unwrap();
        let source = host.attach(1).unwrap();
        let mut runtime = Runtime::new(&mut host, Box::new(sender), "service-test".into()).unwrap();
        wait(
            || runtime.availability() != Availability::Checking,
            "service discovery completed",
        );
        assert_eq!(
            runtime.availability(),
            if scenario == "missing" {
                Availability::Unavailable
            } else {
                Availability::Available
            }
        );
        assert!(!root.join("received.json").exists());
        let now = Instant::now();
        host.bell(source, Focus::default(), now).unwrap();
        runtime.ingress(&mut host, source, Focus::default());
        runtime.poll(&mut host, Focus::default(), now, |_| {
            Some(Text {
                title: "First bell".into(),
                body: "build finished".into(),
            })
        });
        let mut restored = None;
        if scenario == "missing" {
            let child = Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    TEST,
                    "--ignored",
                    "--test-threads=1",
                    "--nocapture",
                ])
                .env(ROLE, "daemon")
                .stdin(Stdio::null())
                .spawn()
                .unwrap();
            restored = Some(OwnedChild(child));
            wait(
                || runtime.availability() == Availability::Available,
                "new service observed",
            );
            runtime.poll(&mut host, Focus::default(), Instant::now(), |_| {
                Some(Text {
                    title: "Old bell".into(),
                    body: "must not replay".into(),
                })
            });
            for _ in 0..20 {
                pump();
            }
            assert!(
                !root.join("received.json").exists(),
                "missing-service bell replayed"
            );
            let later = now + among::notifications::COOLDOWN;
            host.bell(source, Focus::default(), later).unwrap();
            runtime.ingress(&mut host, source, Focus::default());
            runtime.poll(&mut host, Focus::default(), later, |_| {
                Some(Text {
                    title: "First bell".into(),
                    body: "build finished".into(),
                })
            });
        }
        wait(
            || root.join("received.json").exists(),
            "real GIO notification reached the selected service",
        );
        eprintln!(
            "SERVICE_CLIENT {scenario} pid={} before received parse",
            std::process::id()
        );
        let received: serde_json::Value =
            serde_json::from_slice(&fs::read(root.join("received.json")).unwrap()).unwrap();
        eprintln!("SERVICE_CLIENT {scenario} after received parse");
        assert_eq!(received["title"], "First bell");
        assert_eq!(received["body"], "build finished");
        assert_eq!(
            received["method"],
            if portal { "AddNotification" } else { "Notify" }
        );
        // The receipt is published before Notify's reply. A later reply from
        // that same service connection orders the earlier reply ahead of it;
        // dispatch it while the application still owns its GIO backend.
        eprintln!("SERVICE_CLIENT {scenario} before notification reply barrier");
        connection
            .call_sync(
                Some(service_name),
                if portal {
                    "/org/freedesktop/portal/desktop"
                } else {
                    "/org/freedesktop/Notifications"
                },
                if portal {
                    "org.freedesktop.portal.Notification"
                } else {
                    FDO
                },
                "Barrier",
                None,
                None,
                gio::DBusCallFlags::NONE,
                5_000,
                gio::Cancellable::NONE,
            )
            .unwrap();
        pump();
        eprintln!("SERVICE_CLIENT {scenario} after notification reply barrier");
        eprintln!("SERVICE_CLIENT {scenario} before runtime shutdown");
        runtime.shutdown(&mut host);
        wait(
            || root.join("withdrawn").exists(),
            "GIO withdrawal reached the service before application teardown",
        );
        eprintln!("SERVICE_CLIENT {scenario} after runtime shutdown; before runtime drop");
        drop(runtime);
        eprintln!("SERVICE_CLIENT {scenario} after runtime drop; before application drop");
        drop(app);
        eprintln!("SERVICE_CLIENT {scenario} after application drop; before pending dispatch");
        for pass in 0..10 {
            eprintln!("SERVICE_CLIENT {scenario} before pending dispatch {pass}");
            pump();
        }
        eprintln!("SERVICE_CLIENT {scenario} after pending dispatch; before restored fixture drop");
        drop(restored);
        eprintln!(
            "SERVICE_CLIENT {scenario} after restored fixture drop; before client scope drop"
        );
    }
    eprintln!("SERVICE_CLIENT {scenario} after client scope drop; before private bus down");
    bus.down();
    eprintln!("SERVICE_CLIENT {scenario} after private bus down");
}

#[test]
#[ignore = "requires isolated subprocesses and an activatable private session-bus fixture"]
fn real_gio_activates_the_selected_route_and_missing_service_bells_do_not_replay() {
    if let Ok(role) = std::env::var(ROLE) {
        let root = PathBuf::from(std::env::var_os(ROOT).unwrap());
        crate::test_isolation::assert_below_check_root(&root);
        let scenario = std::env::var(CASE).unwrap();
        match role.as_str() {
            "daemon" => daemon(&root, scenario == "portal"),
            "client" => client(&root, &scenario),
            other => panic!("unknown private-bus fixture role {other}"),
        }
        return;
    }
    let base = crate::test_isolation::check_root()
        .join(format!("desktop-services-{}", std::process::id()));
    fs::create_dir(&base).unwrap();
    for scenario in ["fdo", "portal", "missing"] {
        let root = base.join(scenario);
        fs::create_dir(&root).unwrap();
        let log = fs::File::create(root.join("scenario.log")).unwrap();
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                TEST,
                "--ignored",
                "--test-threads=1",
                "--nocapture",
            ])
            .env(ROLE, "client")
            .env(CASE, scenario)
            .env(ROOT, &root)
            .env_remove("GNOTIFICATION_BACKEND")
            .env_remove("GIO_USE_PORTALS")
            .env_remove("SNAP")
            .stdin(Stdio::null())
            .stdout(log.try_clone().unwrap())
            .stderr(log);
        for (key, directory) in [
            ("HOME", "home"),
            ("XDG_CONFIG_HOME", "config"),
            ("XDG_DATA_HOME", "data"),
            ("XDG_CACHE_HOME", "cache"),
            ("XDG_STATE_HOME", "state"),
        ] {
            let path = root.join(directory);
            fs::create_dir(&path).unwrap();
            command.env(key, path);
        }
        if scenario == "portal" {
            command.env("GIO_USE_PORTALS", "1");
        }
        OwnedChild(command.spawn().unwrap()).finish();
        assert!(fs::read_to_string(root.join("scenario.log"))
            .unwrap()
            .contains("test result: ok. 1 passed; 0 failed;"));
        println!(
            "PASS real GIO private-bus scenario {scenario}; evidence={}",
            root.display()
        );
    }
}

fn publish_measurement(path: &Path, value: &serde_json::Value) {
    let pending = path.with_extension("pending");
    fs::write(&pending, serde_json::to_vec_pretty(value).unwrap()).unwrap();
    fs::rename(pending, path).unwrap();
}

fn fdo_control(connection: &gio::DBusConnection, method: &str) -> glib::Variant {
    fdo_control_to(connection, FDO, method)
}

fn fdo_control_to(
    connection: &gio::DBusConnection,
    destination: &str,
    method: &str,
) -> glib::Variant {
    connection
        .call_sync(
            Some(destination),
            "/org/freedesktop/Notifications",
            FDO,
            method,
            None,
            None,
            gio::DBusCallFlags::NONE,
            5_000,
            gio::Cancellable::NONE,
        )
        .unwrap()
}

fn name_owner(connection: &gio::DBusConnection, name: &str) -> Option<String> {
    connection
        .call_sync(
            Some("org.freedesktop.DBus"),
            "/org/freedesktop/DBus",
            "org.freedesktop.DBus",
            "GetNameOwner",
            Some(&(name,).to_variant()),
            None,
            gio::DBusCallFlags::NONE,
            5_000,
            gio::Cancellable::NONE,
        )
        .ok()
        .and_then(|reply| reply.get::<(String,)>())
        .map(|(owner,)| owner)
}

fn spawn_delayed_daemon(root: &Path) -> Child {
    fs::create_dir_all(root).unwrap();
    Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            EARLY_TEST,
            "--ignored",
            "--test-threads=1",
            "--nocapture",
        ])
        .env(ROLE, "delayed-daemon")
        .env(ROOT, root)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap()
}

fn delayed_fdo_daemon(root: &Path) {
    #[derive(Default)]
    struct Delayed {
        requests: u32,
        pending: Option<(u32, gio::DBusMethodInvocation)>,
        pending_close: Option<(u32, gio::DBusMethodInvocation)>,
        closed: Vec<u32>,
        fail_notify: bool,
        fail_close: bool,
        malformed_close: bool,
        same_id: bool,
        hold_close: bool,
    }
    let connection = gio::bus_get_sync(gio::BusType::Session, gio::Cancellable::NONE).unwrap();
    let xml = r#"<node><interface name="org.freedesktop.Notifications">
<method name="Notify"><arg type="s" direction="in"/><arg type="u" direction="in"/><arg type="s" direction="in"/><arg type="s" direction="in"/><arg type="s" direction="in"/><arg type="as" direction="in"/><arg type="a{sv}" direction="in"/><arg type="i" direction="in"/><arg type="u" direction="out"/></method>
<method name="CloseNotification"><arg type="u" direction="in"/></method>
<method name="ReleaseNotify"/><method name="Counts"><arg type="u" direction="out"/><arg type="au" direction="out"/><arg type="b" direction="out"/></method>
<method name="EmitToken"/><method name="EmitAction"/><method name="EmitClosed"/><method name="ReleaseName"/>
<method name="FailNextNotify"/><method name="FailNextClose"/><method name="ClearFailures"/><method name="MalformedClose"/>
<method name="SameId"/><method name="HoldClose"/><method name="ReleaseClose"/><method name="EmitToken2"/>
</interface></node>"#;
    let node = gio::DBusNodeInfo::for_xml(xml).unwrap();
    let state = Rc::new(RefCell::new(Delayed::default()));
    let root = root.to_owned();
    let signal_connection = connection.clone();
    let registration = connection
        .register_object("/org/freedesktop/Notifications", &node.interfaces()[0])
        .method_call(
            move |_, _, _, _, method, parameters, invocation| match method {
                "Notify" => {
                    let mut state = state.borrow_mut();
                    if state.fail_notify {
                        state.fail_notify = false;
                        fs::write(root.join("notify-error.json"), "refused").unwrap();
                        invocation.return_dbus_error(
                            "org.freedesktop.Notifications.Error",
                            "notify refused",
                        );
                        return;
                    }
                    assert!(
                        state.pending.is_none(),
                        "only one delayed call is in flight"
                    );
                    state.requests += 1;
                    let replace_id = parameters.child_value(1).get::<u32>().unwrap();
                    let id = if state.same_id && replace_id > 0 {
                        replace_id
                    } else {
                        36 + state.requests
                    };
                    let hints = parameters
                        .child_value(6)
                        .get::<BTreeMap<String, glib::Variant>>()
                        .unwrap();
                    let path = if replace_id > 0 && id == replace_id {
                        root.join(format!("replace-{id}.json"))
                    } else {
                        root.join(format!("request-{id}.json"))
                    };
                    publish_measurement(
                        &path,
                        &serde_json::json!({
                            "method":"Notify", "reply_held":true, "assigned_id":id,
                            "app_name":parameters.child_value(0).get::<String>().unwrap(),
                            "replace_id":parameters.child_value(1).get::<u32>().unwrap(),
                            "app_icon":parameters.child_value(2).get::<String>().unwrap(),
                            "title":parameters.child_value(3).get::<String>().unwrap(),
                            "body":parameters.child_value(4).get::<String>().unwrap(),
                            "actions":parameters.child_value(5).get::<Vec<String>>().unwrap(),
                            "hint_names":hints.keys().cloned().collect::<Vec<_>>(),
                            "expire_timeout":parameters.child_value(7).get::<i32>().unwrap()
                        }),
                    );
                    // Keep the real invocation alive: no numeric FDO id reaches
                    // GIO until the client explicitly releases this exact reply.
                    state.pending = Some((id, invocation));
                }
                "ReleaseNotify" => {
                    let (id, pending) = state
                        .borrow_mut()
                        .pending
                        .take()
                        .expect("no delayed Notify");
                    pending.return_value(Some(&(id,).to_variant()));
                    invocation.return_value(Some(&().to_variant()));
                }
                "Counts" => {
                    let snapshot = {
                        let state = state.borrow();
                        (
                            state.requests,
                            state.closed.clone(),
                            state.pending.is_some(),
                        )
                    };
                    invocation.return_value(Some(&snapshot.to_variant()));
                }
                "CloseNotification" => {
                    let (id,) = parameters.get::<(u32,)>().unwrap();
                    {
                        let mut state = state.borrow_mut();
                        if state.fail_close {
                            state.fail_close = false;
                            fs::write(root.join("close-error.json"), id.to_string()).unwrap();
                            invocation.return_dbus_error(
                                "org.freedesktop.Notifications.Error",
                                "close refused",
                            );
                            return;
                        }
                        if state.malformed_close {
                            state.malformed_close = false;
                            fs::write(root.join("close-malformed.json"), id.to_string()).unwrap();
                            invocation.return_value(Some(&(id,).to_variant()));
                            return;
                        }
                        if state.hold_close {
                            state.hold_close = false;
                            fs::write(root.join("close-held.json"), id.to_string()).unwrap();
                            state.pending_close = Some((id, invocation));
                            return;
                        }
                        assert!((37..=36 + state.requests).contains(&id));
                        state.closed.push(id);
                    }
                    fs::write(root.join(format!("closed-{id}.json")), id.to_string()).unwrap();
                    invocation.return_value(Some(&().to_variant()));
                }
                "FailNextNotify" => {
                    state.borrow_mut().fail_notify = true;
                    invocation.return_value(Some(&().to_variant()));
                }
                "FailNextClose" => {
                    state.borrow_mut().fail_close = true;
                    invocation.return_value(Some(&().to_variant()));
                }
                "ClearFailures" => {
                    let mut state = state.borrow_mut();
                    state.fail_notify = false;
                    state.fail_close = false;
                    state.malformed_close = false;
                    invocation.return_value(Some(&().to_variant()));
                }
                "MalformedClose" => {
                    state.borrow_mut().malformed_close = true;
                    invocation.return_value(Some(&().to_variant()));
                }
                "SameId" => {
                    state.borrow_mut().same_id = true;
                    invocation.return_value(Some(&().to_variant()));
                }
                "HoldClose" => {
                    state.borrow_mut().hold_close = true;
                    invocation.return_value(Some(&().to_variant()));
                }
                "ReleaseClose" => {
                    let (id, pending) = state
                        .borrow_mut()
                        .pending_close
                        .take()
                        .expect("no held Close");
                    pending.return_value(Some(&().to_variant()));
                    fs::write(root.join(format!("closed-{id}.json")), id.to_string()).unwrap();
                    invocation.return_value(Some(&().to_variant()));
                }
                "EmitToken2" => {
                    signal_connection
                        .emit_signal(
                            None,
                            "/org/freedesktop/Notifications",
                            FDO,
                            "ActivationToken",
                            Some(&(37_u32, "replacement-token").to_variant()),
                        )
                        .unwrap();
                    invocation.return_value(Some(&().to_variant()));
                }
                "ReleaseName" => {
                    signal_connection
                        .call_sync(
                            Some("org.freedesktop.DBus"),
                            "/org/freedesktop/DBus",
                            "org.freedesktop.DBus",
                            "ReleaseName",
                            Some(&(FDO,).to_variant()),
                            None,
                            gio::DBusCallFlags::NONE,
                            5_000,
                            gio::Cancellable::NONE,
                        )
                        .unwrap();
                    invocation.return_value(Some(&().to_variant()));
                }
                "EmitToken" => {
                    signal_connection
                        .emit_signal(
                            None,
                            "/org/freedesktop/Notifications",
                            FDO,
                            "ActivationToken",
                            Some(&(37_u32, "test-token").to_variant()),
                        )
                        .unwrap();
                    invocation.return_value(Some(&().to_variant()));
                }
                "EmitAction" => {
                    signal_connection
                        .emit_signal(
                            None,
                            "/org/freedesktop/Notifications",
                            FDO,
                            "ActionInvoked",
                            Some(&(37_u32, "default").to_variant()),
                        )
                        .unwrap();
                    invocation.return_value(Some(&().to_variant()));
                }
                "EmitClosed" => {
                    signal_connection
                        .emit_signal(
                            None,
                            "/org/freedesktop/Notifications",
                            FDO,
                            "NotificationClosed",
                            Some(&(37_u32, 1_u32).to_variant()),
                        )
                        .unwrap();
                    invocation.return_value(Some(&().to_variant()));
                }
                other => panic!("unexpected delayed fixture method {other}"),
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
            Some(&(FDO, 4_u32).to_variant()),
            None,
            gio::DBusCallFlags::NONE,
            5_000,
            gio::Cancellable::NONE,
        )
        .unwrap();
    assert_eq!(reply.get::<(u32,)>(), Some((1,)));
    let loop_ = glib::MainLoop::new(None, false);
    connection.connect_closed({
        let loop_ = loop_.clone();
        move |_, _, _| loop_.quit()
    });
    glib::timeout_add_seconds_local_once(20, {
        let loop_ = loop_.clone();
        move || loop_.quit()
    });
    loop_.run();
    let _ = connection.unregister_object(registration);
}

struct ObservedSender {
    sender: GioSender,
    id: Rc<RefCell<Option<String>>>,
    withdrawals: Rc<Cell<u32>>,
}

impl Sender for ObservedSender {
    fn availability(&self) -> Availability {
        self.sender.availability()
    }

    fn attempt(&mut self, id: &str, text: &Text, target: &Target) {
        *self.id.borrow_mut() = Some(id.to_owned());
        self.sender.attempt(id, text, target);
    }

    fn withdraw(&mut self, id: &str) {
        self.withdrawals.set(self.withdrawals.get() + 1);
        self.sender.withdraw(id);
    }

    fn shutdown(&mut self) {
        self.sender.shutdown();
    }

    fn is_quiescent(&self) -> bool {
        self.sender.is_quiescent()
    }
}

fn gio_control(app: &gio::Application, connection: &gio::DBusConnection, root: &Path) {
    let counts = || {
        fdo_control(connection, "Counts")
            .get::<(u32, Vec<u32>, bool)>()
            .unwrap()
    };
    let notification = gio::Notification::new("GIO early withdrawal control");
    notification.set_body(Some("reply deliberately held"));
    app.send_notification(Some("gio-early-control"), &notification);
    wait(
        || root.join("request-37.json").exists(),
        "GIO control Notify invocation held by daemon",
    );
    assert_eq!(counts(), (1, Vec::new(), true));
    app.withdraw_notification("gio-early-control");
    assert_eq!(
        counts(),
        (1, Vec::new(), true),
        "GIO has no server id before the reply"
    );
    fdo_control(connection, "ReleaseNotify");
    wait(|| !counts().2, "GIO control reply released");
    for _ in 0..10 {
        pump();
    }
    let after_reply = counts();
    assert_eq!(after_reply.0, 1);
    assert!(!after_reply.2);
    assert!(
        !after_reply.1.contains(&37),
        "GIO control unexpectedly closed the early notification"
    );
    app.withdraw_notification("gio-early-control");
    wait(
        || counts().1.contains(&37),
        "explicit GIO cleanup closes the original id",
    );
    publish_measurement(
        &root.join("early-withdrawal.json"),
        &serde_json::json!({
            "schema":1,
            "scope":"real private FDO method calls; shell display was not observed",
            "observation_boundary":"ordered Notify/ReleaseNotify replies, main-context dispatch, then Counts roundtrip",
            "withdraw_trigger":"gio-control",
            "notify_reply_was_held":true,
            "automatic_close_ids_after_reply":after_reply.1,
            "early_withdraw_satisfied":false,
            "positive_cleanup_control_closed_id":37,
            "result":"measured_gio_limit"
        }),
    );
}

fn coalesce_client(
    app: &gio::Application,
    connection: &gio::DBusConnection,
    root: &Path,
    actions: Rc<Cell<u32>>,
) {
    let counts = || {
        fdo_control(connection, "Counts")
            .get::<(u32, Vec<u32>, bool)>()
            .unwrap()
    };
    let mut sender = GioSender::for_application(Some(app.clone()));
    wait(
        || sender.availability() == Availability::Available,
        "coalesce FDO route",
    );
    let target = Target {
        instance: "coalesce".to_string(),
        session: 1,
        generation: 1,
        pane: 1,
    };
    sender.attempt(
        "coalesce",
        &Text {
            title: "first".to_string(),
            body: "first".to_string(),
        },
        &target,
    );
    wait(
        || root.join("request-37.json").exists(),
        "first coalesced Notify held",
    );
    sender.attempt(
        "coalesce",
        &Text {
            title: "second".to_string(),
            body: "second".to_string(),
        },
        &target,
    );
    assert!(
        !root.join("request-38.json").exists(),
        "a second Notify was sent before the first reply"
    );
    assert_eq!(counts(), (1, Vec::new(), true));
    fdo_control(connection, "ReleaseNotify");
    wait(
        || root.join("request-38.json").exists(),
        "coalesced follow-up Notify after the first reply",
    );
    let follow_up: serde_json::Value =
        serde_json::from_slice(&fs::read(root.join("request-38.json")).unwrap()).unwrap();
    assert_eq!(follow_up["title"], "second");
    assert_eq!(follow_up["replace_id"], 37);
    assert_eq!(counts(), (2, Vec::new(), true));
    fdo_control(connection, "EmitToken");
    pump();
    fdo_control(connection, "EmitAction");
    wait(
        || actions.get() == 1,
        "provisional posted banner activates during follow-up",
    );
    sender.withdraw("coalesce");
    wait(
        || counts().1.contains(&37),
        "withdraw during follow-up closes the visible first id",
    );
    assert!(
        !counts().1.contains(&38),
        "the follow-up id is not known before its reply"
    );
    fdo_control(connection, "ReleaseNotify");
    wait(
        || counts().1.contains(&38),
        "cancelled follow-up closes its reply id",
    );
    publish_measurement(
        &root.join("early-withdrawal.json"),
        &serde_json::json!({
            "schema":1,
            "scope":"real private FDO method calls; shell display was not observed",
            "withdraw_trigger":"coalesce-withdraw-before-second-reply",
            "notify_reply_was_held":true,
            "follow_up_replace_id":follow_up["replace_id"],
            "closed_ids":counts().1,
            "result":"coalesce_withdraw_observed"
        }),
    );
}

fn errors_client(app: &gio::Application, connection: &gio::DBusConnection, root: &Path) {
    let counts = || {
        fdo_control(connection, "Counts")
            .get::<(u32, Vec<u32>, bool)>()
            .unwrap()
    };
    let mut sender = GioSender::for_application(Some(app.clone()));
    wait(
        || sender.availability() == Availability::Available,
        "errors FDO route",
    );
    let target = Target {
        instance: "errors".to_string(),
        session: 1,
        generation: 1,
        pane: 1,
    };
    fdo_control(connection, "FailNextNotify");
    sender.attempt(
        "error",
        &Text {
            title: "refused".to_string(),
            body: "refused".to_string(),
        },
        &target,
    );
    wait(
        || root.join("notify-error.json").exists(),
        "raw Notify error returned",
    );
    for _ in 0..10 {
        pump();
    }
    assert!(
        !root.join("request-37.json").exists(),
        "failed Notify created a server-side notification"
    );
    sender.attempt(
        "error",
        &Text {
            title: "after notify error".to_string(),
            body: "after notify error".to_string(),
        },
        &target,
    );
    wait(
        || root.join("request-37.json").exists(),
        "Notify succeeds after an error",
    );
    fdo_control(connection, "ReleaseNotify");
    wait(|| !counts().2, "error-control reply released");
    for _ in 0..10 {
        pump();
    }
    fdo_control(connection, "FailNextClose");
    sender.withdraw("error");
    wait(
        || root.join("close-error.json").exists(),
        "raw CloseNotification error returned",
    );
    for _ in 0..10 {
        pump();
    }
    assert!(
        !counts().1.contains(&37),
        "failed CloseNotification was reported as a successful close"
    );
    sender.attempt(
        "after",
        &Text {
            title: "after close error".to_string(),
            body: "after close error".to_string(),
        },
        &target,
    );
    wait(
        || root.join("request-38.json").exists(),
        "Notify succeeds after a Close error",
    );
    fdo_control(connection, "ReleaseNotify");
    wait(|| !counts().2, "post-error reply released");
    for _ in 0..10 {
        pump();
    }
    sender.withdraw("after");
    wait(
        || counts().1.contains(&38),
        "later CloseNotification succeeds",
    );
    sender.attempt(
        "malformed",
        &Text {
            title: "malformed close".to_string(),
            body: "malformed close".to_string(),
        },
        &target,
    );
    wait(
        || root.join("request-39.json").exists(),
        "malformed-close Notify held",
    );
    fdo_control(connection, "ReleaseNotify");
    wait(|| !counts().2, "malformed-close reply released");
    for _ in 0..10 {
        pump();
    }
    fdo_control(connection, "MalformedClose");
    sender.withdraw("malformed");
    wait(
        || root.join("close-malformed.json").exists(),
        "malformed successful Close reply returned",
    );
    assert!(
        !counts().1.contains(&39),
        "malformed Close reply was accepted as a successful close"
    );
    publish_measurement(
        &root.join("early-withdrawal.json"),
        &serde_json::json!({
            "schema":1,
            "scope":"real private FDO method calls; shell display was not observed",
            "withdraw_trigger":"notify-and-close-errors",
            "notify_error":fs::read_to_string(root.join("notify-error.json")).unwrap(),
            "close_error":fs::read_to_string(root.join("close-error.json")).unwrap(),
            "malformed_close_id":fs::read_to_string(root.join("close-malformed.json")).unwrap(),
            "closed_ids":counts().1,
            "result":"errors_observed"
        }),
    );
}

fn same_id_client(app: &gio::Application, connection: &gio::DBusConnection, root: &Path) {
    // add_filter runs on Gio's dedicated message-handling thread. Cross it with a
    // bounded non-blocking channel; never touch GTK or Rc<RefCell> in the filter.
    let (token_tx, token_rx) = std::sync::mpsc::sync_channel::<String>(8);
    let filter = connection.add_filter(move |_, message, incoming| {
        if !incoming
            && message.message_type() == gio::DBusMessageType::MethodCall
            && message
                .member()
                .is_some_and(|member| member.as_str() == "ActivateAction")
        {
            if let Some((_name, _arguments, platform)) = message.body().and_then(|body| {
                body.get::<(String, Vec<glib::Variant>, BTreeMap<String, glib::Variant>)>()
            }) {
                if let Some(token) = platform
                    .get("activation-token")
                    .and_then(|value| value.get::<String>())
                {
                    let _ = token_tx.try_send(token);
                }
            }
        }
        Some(message.clone())
    });
    let mut captured = None;
    let counts = || {
        fdo_control(connection, "Counts")
            .get::<(u32, Vec<u32>, bool)>()
            .unwrap()
    };
    let mut sender = GioSender::for_application(Some(app.clone()));
    wait(
        || sender.availability() == Availability::Available,
        "same-id FDO route",
    );
    let target = Target {
        instance: "same-id".to_string(),
        session: 1,
        generation: 1,
        pane: 1,
    };
    fdo_control(connection, "SameId");
    sender.attempt(
        "same",
        &Text {
            title: "first".to_string(),
            body: "first".to_string(),
        },
        &target,
    );
    wait(
        || root.join("request-37.json").exists(),
        "same-id first Notify held",
    );
    fdo_control(connection, "ReleaseNotify");
    wait(|| !counts().2, "same-id first reply released");
    for _ in 0..10 {
        pump();
    }
    fdo_control(connection, "EmitToken");
    pump();
    sender.attempt(
        "same",
        &Text {
            title: "second".to_string(),
            body: "second".to_string(),
        },
        &target,
    );
    wait(
        || root.join("replace-37.json").exists(),
        "same-id replacement Notify held",
    );
    fdo_control(connection, "EmitToken2");
    pump();
    fdo_control(connection, "ReleaseNotify");
    wait(|| !counts().2, "same-id replacement reply released");
    for _ in 0..10 {
        pump();
    }
    fdo_control(connection, "EmitAction");
    wait(
        || {
            while let Ok(token) = token_rx.try_recv() {
                captured = Some(token);
            }
            captured.as_deref() == Some("replacement-token")
        },
        "same-id replacement keeps the newest activation token",
    );
    sender.withdraw("same");
    wait(|| counts().1.contains(&37), "same-id notification closed");
    connection.remove_filter(filter);
    publish_measurement(
        &root.join("early-withdrawal.json"),
        &serde_json::json!({
            "schema":1,
            "scope":"real private FDO method calls; shell display was not observed",
            "withdraw_trigger":"same-owner-same-id-token",
            "server_id":37,
            "activation_token":captured,
            "closed_ids":counts().1,
            "result":"same_id_token_observed"
        }),
    );
}

fn retired_owner_client(app: &gio::Application, connection: &gio::DBusConnection, root: &Path) {
    let (activation_tx, activation_rx) = std::sync::mpsc::sync_channel::<(Target, String)>(8);
    let filter = connection.add_filter(move |_, message, incoming| {
        if !incoming
            && message.message_type() == gio::DBusMessageType::MethodCall
            && message
                .member()
                .is_some_and(|member| member.as_str() == "ActivateAction")
        {
            if let Some((_name, arguments, platform)) = message.body().and_then(|body| {
                body.get::<(String, Vec<glib::Variant>, BTreeMap<String, glib::Variant>)>()
            }) {
                if let (Some(target), Some(token)) = (
                    arguments.first().and_then(target),
                    platform
                        .get("activation-token")
                        .and_then(|value| value.get::<String>()),
                ) {
                    let _ = activation_tx.try_send((target, token));
                }
            }
        }
        Some(message.clone())
    });
    let counts = || {
        fdo_control(connection, "Counts")
            .get::<(u32, Vec<u32>, bool)>()
            .unwrap()
    };
    let a_root = root.join("a");
    let _a = OwnedChild(spawn_delayed_daemon(&a_root));
    wait(
        || name_owner(connection, FDO).is_some(),
        "retired-owner first service",
    );
    let mut sender = GioSender::for_application(Some(app.clone()));
    wait(
        || sender.availability() == Availability::Available,
        "retired-owner FDO route",
    );
    let expected = Target {
        instance: "retired-owner".to_string(),
        session: 1,
        generation: 2,
        pane: 9,
    };
    sender.attempt(
        "retired",
        &Text {
            title: "retired".to_string(),
            body: "retired".to_string(),
        },
        &expected,
    );
    wait(
        || a_root.join("request-37.json").exists(),
        "retired-owner Notify held",
    );
    fdo_control(connection, "ReleaseNotify");
    wait(|| !counts().2, "retired-owner Notify reply released");
    for _ in 0..10 {
        pump();
    }
    fdo_control(connection, "HoldClose");
    sender.withdraw("retired");
    wait(
        || a_root.join("close-held.json").exists(),
        "retired-owner Close held by A",
    );
    let a_unique = name_owner(connection, FDO).expect("owner A disappeared");
    fdo_control(connection, "ReleaseName");
    let _b = OwnedChild(spawn_delayed_daemon(&root.join("b")));
    wait(
        || name_owner(connection, FDO).is_some_and(|owner| owner != a_unique),
        "retired-owner new service B",
    );
    let b_unique = name_owner(connection, FDO).unwrap();
    // Let the application process the full owner transition before A's late
    // action. The retired identity must survive that transition while A's Close
    // reply is still held.
    wait(
        || sender.fdo_owner_for_test().as_deref() == Some(b_unique.as_str()),
        "retired-owner application bound new owner B",
    );
    assert!(
        sender.fdo_has_retired_for_test(&a_unique, 37),
        "owner transition removed the retirement of the held Close"
    );

    fdo_control_to(connection, &a_unique, "EmitToken");
    fdo_control_to(connection, &a_unique, "EmitAction");
    let mut late = None;
    wait(
        || {
            while let Ok(value) = activation_rx.try_recv() {
                late = Some(value);
            }
            late.is_some()
        },
        "late A action activates the retired target",
    );
    let (observed, token) = late.unwrap();
    assert_eq!(observed, expected);
    assert_eq!(token, "test-token");

    fdo_control_to(connection, &b_unique, "EmitAction");
    for _ in 0..20 {
        pump();
    }
    assert!(
        activation_rx.try_recv().is_err(),
        "foreign owner action activated the retired target"
    );
    let b_counts = fdo_control_to(connection, &b_unique, "Counts")
        .get::<(u32, Vec<u32>, bool)>()
        .unwrap();
    assert_eq!(
        b_counts,
        (0, Vec::new(), false),
        "new owner received the old Close or a foreign notification"
    );

    fdo_control_to(connection, &a_unique, "ReleaseClose");
    wait(
        || !sender.fdo_has_retired_for_test(&a_unique, 37),
        "Close reply removed the retired identity",
    );
    fdo_control_to(connection, &a_unique, "EmitAction");
    for _ in 0..20 {
        pump();
    }
    assert!(
        activation_rx.try_recv().is_err(),
        "retired identity survived its Close reply"
    );
    connection.remove_filter(filter);
    publish_measurement(
        &root.join("early-withdrawal.json"),
        &serde_json::json!({
            "schema":1,
            "scope":"real private FDO method calls; shell display was not observed",
            "withdraw_trigger":"retired-owner-late-action",
            "observed_target":["retired-owner", 1, 2, 9],
            "activation_token":token,
            "new_owner_counts":b_counts,
            "result":"retired_owner_action_observed"
        }),
    );
}

fn notification_closed_client(
    app: &gio::Application,
    connection: &gio::DBusConnection,
    root: &Path,
) {
    let (activation_tx, activation_rx) = std::sync::mpsc::sync_channel::<(Target, String)>(8);
    let filter = connection.add_filter(move |_, message, incoming| {
        if !incoming
            && message.message_type() == gio::DBusMessageType::MethodCall
            && message
                .member()
                .is_some_and(|member| member.as_str() == "ActivateAction")
        {
            if let Some((_name, arguments, platform)) = message.body().and_then(|body| {
                body.get::<(String, Vec<glib::Variant>, BTreeMap<String, glib::Variant>)>()
            }) {
                if let (Some(target), Some(token)) = (
                    arguments.first().and_then(target),
                    platform
                        .get("activation-token")
                        .and_then(|value| value.get::<String>()),
                ) {
                    let _ = activation_tx.try_send((target, token));
                }
            }
        }
        Some(message.clone())
    });
    let counts = || {
        fdo_control(connection, "Counts")
            .get::<(u32, Vec<u32>, bool)>()
            .unwrap()
    };
    let a_root = root.join("a");
    let _a = OwnedChild(spawn_delayed_daemon(&a_root));
    wait(
        || name_owner(connection, FDO).is_some(),
        "notification-closed FDO service",
    );
    let mut sender = GioSender::for_application(Some(app.clone()));
    wait(
        || sender.availability() == Availability::Available,
        "notification-closed FDO route",
    );
    let expected = Target {
        instance: "notification-closed".to_string(),
        session: 1,
        generation: 2,
        pane: 7,
    };
    sender.attempt(
        "closed",
        &Text {
            title: "closed".to_string(),
            body: "closed".to_string(),
        },
        &expected,
    );
    wait(
        || a_root.join("request-37.json").exists(),
        "notification-closed Notify held",
    );
    fdo_control(connection, "ReleaseNotify");
    wait(|| !counts().2, "notification-closed Notify reply released");
    for _ in 0..10 {
        pump();
    }
    fdo_control(connection, "EmitToken");
    pump();

    // A NotificationClosed from a foreign unique sender with the live id must
    // not remove the entry. Flush the foreign signal before the owner action.
    let foreign = gio::bus_get_sync(gio::BusType::Session, gio::Cancellable::NONE).unwrap();
    foreign
        .emit_signal(
            None,
            "/org/freedesktop/Notifications",
            FDO,
            "NotificationClosed",
            Some(&(37_u32, 1_u32).to_variant()),
        )
        .unwrap();
    foreign
        .call_sync(
            Some("org.freedesktop.DBus"),
            "/org/freedesktop/DBus",
            "org.freedesktop.DBus",
            "GetNameOwner",
            Some(&(FDO,).to_variant()),
            None,
            gio::DBusCallFlags::NONE,
            5_000,
            gio::Cancellable::NONE,
        )
        .unwrap();
    drop(foreign);
    pump();
    fdo_control(connection, "EmitAction");
    let mut observed = None;
    wait(
        || {
            while let Ok(value) = activation_rx.try_recv() {
                observed = Some(value);
            }
            observed.is_some()
        },
        "foreign NotificationClosed removed the live identity",
    );
    let (target, token) = observed.unwrap();
    assert_eq!(target, expected);
    assert_eq!(token, "test-token");

    // The owning service closes the same id. The stale action must not fire.
    fdo_control(connection, "EmitClosed");
    pump();
    fdo_control(connection, "EmitAction");
    for _ in 0..20 {
        pump();
    }
    assert!(
        activation_rx.try_recv().is_err(),
        "stale action activated a NotificationClosed identity"
    );

    // The next attempt under the same local id must not reuse the removed id.
    sender.attempt(
        "closed",
        &Text {
            title: "new".to_string(),
            body: "new".to_string(),
        },
        &expected,
    );
    wait(
        || a_root.join("request-38.json").exists(),
        "notification-closed new Notify held",
    );
    let request: serde_json::Value =
        serde_json::from_slice(&fs::read(a_root.join("request-38.json")).unwrap()).unwrap();
    assert_eq!(
        request["replace_id"], 0,
        "closed identity left a replaces_id tombstone"
    );
    fdo_control(connection, "ReleaseNotify");
    wait(|| !counts().2, "notification-closed new reply released");
    sender.withdraw("closed");
    wait(
        || counts().1.contains(&38),
        "notification-closed new identity was withdrawn",
    );
    wait(
        || sender.is_quiescent(),
        "notification-closed sender quiescent",
    );
    connection.remove_filter(filter);
    publish_measurement(
        &root.join("early-withdrawal.json"),
        &serde_json::json!({
            "schema":1,
            "scope":"real private FDO method calls; shell display was not observed",
            "withdraw_trigger":"owner-notification-closed",
            "closed_server_id":37,
            "wrong_owner_signal_ignored":true,
            "stale_action_fired":false,
            "new_replace_id":request["replace_id"],
            "new_closed_ids":counts().1,
            "result":"notification_closed_observed"
        }),
    );
}

fn deadline_client(app: &gio::Application, connection: &gio::DBusConnection, root: &Path) {
    let counts = || {
        fdo_control(connection, "Counts")
            .get::<(u32, Vec<u32>, bool)>()
            .unwrap()
    };
    let target = Target {
        instance: "deadline".to_string(),
        session: 1,
        generation: 1,
        pane: 1,
    };

    let mut notify_sender = GioSender::for_application(Some(app.clone()));
    wait(
        || notify_sender.availability() == Availability::Available,
        "deadline Notify route",
    );
    notify_sender.set_fdo_drain_budget_for_test(Duration::from_millis(200));
    notify_sender.attempt(
        "deadline-notify",
        &Text {
            title: "held notify".to_string(),
            body: "held notify".to_string(),
        },
        &target,
    );
    wait(
        || root.join("request-37.json").exists(),
        "deadline Notify held",
    );
    notify_sender.shutdown();
    assert!(
        !notify_sender.is_quiescent(),
        "held Notify was immediately reported quiescent"
    );
    let notify_started = Instant::now();
    wait(
        || notify_sender.is_quiescent(),
        "injected Notify drain budget expires",
    );
    let notify_elapsed = notify_started.elapsed();
    assert!(
        notify_elapsed >= Duration::from_millis(120),
        "Notify deadline returned too early: {notify_elapsed:?}"
    );
    assert!(
        notify_elapsed < Duration::from_secs(2),
        "injected Notify drain took {notify_elapsed:?}"
    );
    fdo_control(connection, "ReleaseNotify");
    wait(|| !counts().2, "expired Notify reply released");
    assert!(
        counts().1.is_empty(),
        "expired Notify drain reported a successful Close"
    );
    drop(notify_sender);

    let mut close_sender = GioSender::for_application(Some(app.clone()));
    wait(
        || close_sender.availability() == Availability::Available,
        "deadline Close route",
    );
    close_sender.attempt(
        "deadline-close",
        &Text {
            title: "held close".to_string(),
            body: "held close".to_string(),
        },
        &target,
    );
    wait(
        || root.join("request-38.json").exists(),
        "deadline Close notification held",
    );
    fdo_control(connection, "ReleaseNotify");
    wait(|| !counts().2, "deadline Close Notify reply released");
    for _ in 0..10 {
        pump();
    }
    fdo_control(connection, "HoldClose");
    close_sender.set_fdo_drain_budget_for_test(Duration::from_millis(200));
    close_sender.shutdown();
    close_sender.withdraw("deadline-close");
    wait(
        || root.join("close-held.json").exists(),
        "CloseNotification held by daemon",
    );
    assert!(
        !close_sender.is_quiescent(),
        "held Close was immediately reported quiescent"
    );
    let close_started = Instant::now();
    wait(
        || close_sender.is_quiescent(),
        "injected Close drain budget expires",
    );
    let close_elapsed = close_started.elapsed();
    assert!(
        close_elapsed >= Duration::from_millis(120),
        "Close deadline returned too early: {close_elapsed:?}"
    );
    assert!(
        close_elapsed < Duration::from_secs(2),
        "injected Close drain took {close_elapsed:?}"
    );
    assert!(
        counts().1.is_empty(),
        "expired Close drain reported a successful close"
    );
    fdo_control(connection, "ReleaseClose");
    publish_measurement(
        &root.join("early-withdrawal.json"),
        &serde_json::json!({
            "schema":1,
            "scope":"real private FDO method calls; shell display was not observed",
            "withdraw_trigger":"injected-deadline-expiry",
            "notify_elapsed_ms":notify_elapsed.as_millis(),
            "close_elapsed_ms":close_elapsed.as_millis(),
            "closed_ids":counts().1,
            "result":"deadline_expired_observed"
        }),
    );
}

fn shutdown_client(app: &gio::Application, connection: &gio::DBusConnection, root: &Path) {
    let counts = || {
        fdo_control(connection, "Counts")
            .get::<(u32, Vec<u32>, bool)>()
            .unwrap()
    };
    let mut sender = GioSender::for_application(Some(app.clone()));
    wait(
        || sender.availability() == Availability::Available,
        "shutdown FDO route",
    );
    let target = Target {
        instance: "shutdown".to_string(),
        session: 1,
        generation: 1,
        pane: 1,
    };
    sender.attempt(
        "shutdown",
        &Text {
            title: "first".to_string(),
            body: "first".to_string(),
        },
        &target,
    );
    wait(
        || root.join("request-37.json").exists(),
        "shutdown first Notify held",
    );
    sender.attempt(
        "shutdown",
        &Text {
            title: "second".to_string(),
            body: "second".to_string(),
        },
        &target,
    );
    fdo_control(connection, "ReleaseNotify");
    wait(
        || root.join("request-38.json").exists(),
        "shutdown follow-up held with provisional posted",
    );
    assert_eq!(counts(), (2, Vec::new(), true));
    sender.shutdown();
    sender.withdraw("shutdown");
    wait(
        || counts().1.contains(&37),
        "shutdown closes provisional visible id",
    );
    fdo_control(connection, "ReleaseNotify");
    wait(
        || counts().1.contains(&38),
        "shutdown closes follow-up reply id",
    );
    wait(|| sender.is_quiescent(), "shutdown drain quiescent");
    publish_measurement(
        &root.join("early-withdrawal.json"),
        &serde_json::json!({
            "schema":1,
            "scope":"real private FDO method calls; shell display was not observed",
            "withdraw_trigger":"shutdown-provisional-posted",
            "closed_ids":counts().1,
            "quiescent":sender.is_quiescent(),
            "result":"shutdown_provisional_observed"
        }),
    );
}

fn owner_swap_client(app: &gio::Application, connection: &gio::DBusConnection, root: &Path) {
    let a_root = root.join("a");
    let b_root = root.join("b");
    let _a = OwnedChild(spawn_delayed_daemon(&a_root));
    wait(
        || name_owner(connection, FDO).is_some(),
        "first delayed FDO owner",
    );
    let mut sender = GioSender::for_application(Some(app.clone()));
    wait(
        || sender.availability() == Availability::Available,
        "owner-swap FDO route",
    );
    let target = Target {
        instance: "swap".to_string(),
        session: 1,
        generation: 1,
        pane: 1,
    };
    sender.attempt(
        "swap",
        &Text {
            title: "first".to_string(),
            body: "first".to_string(),
        },
        &target,
    );
    wait(
        || a_root.join("request-37.json").exists(),
        "first owner-swap Notify held",
    );
    sender.attempt(
        "swap",
        &Text {
            title: "second".to_string(),
            body: "second".to_string(),
        },
        &target,
    );
    assert!(
        !a_root.join("request-38.json").exists(),
        "owner-swap follow-up was sent before the first reply"
    );
    fdo_control(connection, "ReleaseNotify");
    wait(
        || a_root.join("request-38.json").exists(),
        "owner-swap follow-up held",
    );
    let first_owner = name_owner(connection, FDO).expect("first owner disappeared");
    fdo_control(connection, "ReleaseName");
    let _b = OwnedChild(spawn_delayed_daemon(&b_root));
    wait(
        || name_owner(connection, FDO).is_some_and(|owner| owner != first_owner),
        "second delayed FDO owner",
    );
    fdo_control_to(connection, &first_owner, "ReleaseNotify");
    wait(
        || a_root.join("closed-37.json").exists() && a_root.join("closed-38.json").exists(),
        "old owner closed both visible ids",
    );
    let b_counts = fdo_control(connection, "Counts")
        .get::<(u32, Vec<u32>, bool)>()
        .unwrap();
    assert_eq!(
        b_counts,
        (0, Vec::new(), false),
        "the new owner received a Notify or Close from the old request"
    );
    publish_measurement(
        &root.join("early-withdrawal.json"),
        &serde_json::json!({
            "schema":1,
            "scope":"real private FDO method calls; shell display was not observed",
            "withdraw_trigger":"owner-swap",
            "first_owner":first_owner,
            "old_owner_closed":[37, 38],
            "new_owner_counts":b_counts,
            "result":"owner_swap_observed"
        }),
    );
}

fn delayed_fdo_client(root: &Path, scenario: &str) {
    let services = root.join("services");
    fs::create_dir(&services).unwrap();
    let executable = glib::shell_quote(std::env::current_exe().unwrap())
        .into_string()
        .unwrap();
    fs::write(
        services.join(format!("{FDO}.service")),
        format!("[D-BUS Service]\nName={FDO}\nExec=/usr/bin/env {ROLE}=delayed-daemon {executable} --exact {EARLY_TEST} --ignored --test-threads=1 --nocapture\n"),
    ).unwrap();
    let bus = gio::TestDBus::new(gio::TestDBusFlags::NONE);
    bus.add_service_dir(services.to_str().unwrap());
    bus.up();
    {
        let connection = gio::bus_get_sync(gio::BusType::Session, gio::Cancellable::NONE).unwrap();
        let app = gio::Application::builder()
            .application_id("com.github.among-terminal.delayed-fdo-test")
            .flags(gio::ApplicationFlags::NON_UNIQUE)
            .build();
        app.register(gio::Cancellable::NONE).unwrap();
        assert_eq!(app.dbus_connection().as_ref(), Some(&connection));
        let actions = Rc::new(Cell::new(0_u32));
        if scenario == "action"
            || scenario == "coalesce"
            || scenario == "same-id"
            || scenario == "retired-owner"
            || scenario == "notification-closed"
        {
            let action = gio::SimpleAction::new(
                "focus-pane",
                Some(&<(String, u64, u64, u64)>::static_variant_type()),
            );
            action.connect_activate({
                let actions = actions.clone();
                move |_, _| actions.set(actions.get() + 1)
            });
            app.add_action(&action);
        }
        if scenario == "gio-control" {
            gio_control(&app, &connection, root);
            drop(connection);
            drop(app);
            bus.down();
            return;
        }
        if scenario == "coalesce" {
            coalesce_client(&app, &connection, root, actions.clone());
            drop(connection);
            drop(app);
            bus.down();
            return;
        }
        if scenario == "shutdown" {
            shutdown_client(&app, &connection, root);
            drop(connection);
            drop(app);
            bus.down();
            return;
        }
        if scenario == "owner-swap" {
            owner_swap_client(&app, &connection, root);
            drop(connection);
            drop(app);
            bus.down();
            return;
        }
        if scenario == "errors" {
            errors_client(&app, &connection, root);
            drop(connection);
            drop(app);
            bus.down();
            return;
        }
        if scenario == "deadline" {
            deadline_client(&app, &connection, root);
            drop(connection);
            drop(app);
            bus.down();
            return;
        }
        if scenario == "retired-owner" {
            retired_owner_client(&app, &connection, root);
            drop(connection);
            drop(app);
            bus.down();
            return;
        }
        if scenario == "notification-closed" {
            notification_closed_client(&app, &connection, root);
            drop(connection);
            drop(app);
            bus.down();
            return;
        }
        if scenario == "same-id" {
            same_id_client(&app, &connection, root);
            drop(connection);
            drop(app);
            bus.down();
            return;
        }
        let id = Rc::new(RefCell::new(None));
        let withdrawals = Rc::new(Cell::new(0));
        let sender = ObservedSender {
            sender: GioSender::for_application(Some(app.clone())),
            id: id.clone(),
            withdrawals: withdrawals.clone(),
        };
        let mut host = Host::new().unwrap();
        let source = host.attach(1).unwrap();
        let mut runtime = Runtime::new(&mut host, Box::new(sender), "delayed-test".into()).unwrap();
        wait(
            || runtime.availability() == Availability::Available,
            "activatable FDO route",
        );
        let now = Instant::now();
        host.bell(source, Focus::default(), now).unwrap();
        runtime.ingress(&mut host, source, Focus::default());
        runtime.poll(&mut host, Focus::default(), now, |_| {
            Some(Text {
                title: "Early withdrawal".into(),
                body: "reply deliberately held".into(),
            })
        });
        wait(
            || root.join("request-37.json").exists(),
            "real Notify invocation held by daemon",
        );
        let request: serde_json::Value =
            serde_json::from_slice(&fs::read(root.join("request-37.json")).unwrap()).unwrap();
        assert_eq!(request["title"], "Early withdrawal");
        assert_eq!(request["body"], "reply deliberately held");
        assert_eq!(request["app_icon"], "");
        assert_eq!(request["expire_timeout"], -1);
        assert_eq!(request["replace_id"], 0);
        assert_eq!(request["actions"], serde_json::json!(["default", ""]));
        let hints = request["hint_names"].as_array().unwrap();
        for expected in ["desktop-entry", "urgency", "image-path"] {
            assert!(
                hints.iter().any(|hint| hint == expected),
                "raw Notify hint {expected} missing"
            );
        }
        let counts = || {
            fdo_control(&connection, "Counts")
                .get::<(u32, Vec<u32>, bool)>()
                .unwrap()
        };
        assert_eq!(counts(), (1, Vec::new(), true));
        if scenario == "action" {
            fdo_control(&connection, "ReleaseNotify");
            wait(|| !counts().2, "action notification reply released");
            for _ in 0..10 {
                pump();
            }
            fdo_control(&connection, "EmitToken");
            pump();
            fdo_control(&connection, "EmitAction");
            wait(
                || actions.get() == 1,
                "real focus-pane application handler ran",
            );
            publish_measurement(
                &root.join("early-withdrawal.json"),
                &serde_json::json!({
                    "schema":1,
                    "scope":"real private FDO method calls; shell display was not observed",
                    "withdraw_trigger":"action",
                    "notify_reply_was_held":true,
                    "action_handler_calls":actions.get(),
                    "result":"action_observed"
                }),
            );
            runtime.shutdown(&mut host);
            drop(runtime);
            drop(connection);
            drop(app);
            bus.down();
            return;
        }
        match scenario {
            "viewed" => runtime.observe(
                &mut host,
                Focus {
                    application_active: true,
                    viewed: Some(source),
                },
            ),
            "off" => runtime
                .configure(
                    &mut host,
                    among::notifications::Policy {
                        enabled: false,
                        eligibility: among::notifications::Eligibility::ApplicationBackground,
                    },
                )
                .unwrap(),
            other => panic!("unexpected withdrawal case {other}"),
        }
        let early_attempts = withdrawals.get();
        assert_eq!(
            early_attempts, 1,
            "runtime must forward the early withdrawal to the real FDO sender"
        );
        let before_reply = counts();
        assert_eq!(before_reply, (1, Vec::new(), true));
        fdo_control(&connection, "ReleaseNotify");
        // ReleaseNotify's reply follows the original Notify reply on the same
        // connection. Dispatch the client callback and its compensation Close.
        wait(|| !counts().2, "Notify reply released");
        wait(
            || counts().1.contains(&37),
            "automatic CloseNotification after the delayed reply",
        );
        let after_reply = counts();
        assert_eq!(after_reply.0, 1);
        assert!(!after_reply.2);
        let automatic_close = after_reply.1.contains(&37);
        assert!(
            automatic_close,
            "early withdrawal was not compensated after the delayed reply"
        );
        assert!(id.borrow().is_some(), "real sender attempt id absent");
        assert!(
            after_reply.1.iter().filter(|&&id| id == 37).count() == 1,
            "early withdrawal must close the original id exactly once"
        );

        // A fresh, fully acknowledged notification is the positive control for
        // CloseNotification, independent of the measured early-withdraw result.
        let notification = gio::Notification::new("Cleanup control");
        notification.set_body(Some("post-reply withdrawal control"));
        app.send_notification(Some("cleanup-control"), &notification);
        wait(
            || root.join("request-38.json").exists(),
            "positive-control Notify held",
        );
        fdo_control(&connection, "ReleaseNotify");
        pump();
        app.withdraw_notification("cleanup-control");
        wait(
            || counts().1.contains(&38),
            "positive-control CloseNotification reached daemon",
        );
        let final_counts = counts();
        assert_eq!(final_counts.0, 2);
        assert!(!final_counts.2);
        assert_eq!(final_counts.1.iter().filter(|&&id| id == 38).count(), 1);
        runtime.shutdown(&mut host);
        publish_measurement(
            &root.join("early-withdrawal.json"),
            &serde_json::json!({
                "schema":1,
                "scope":"real private FDO method calls; shell display was not observed",
                "observation_boundary":"ordered Notify/ReleaseNotify replies, main-context dispatch, then Counts roundtrip",
                "withdraw_trigger":scenario, "notify_reply_was_held":true,
                "runtime_withdraw_attempts_before_reply":early_attempts,
                "close_ids_before_reply":before_reply.1,
                "automatic_close_ids_after_reply":after_reply.1,
                "early_withdraw_satisfied":automatic_close,
                "positive_cleanup_control_closed_id":38,
                "final_close_ids":final_counts.1,
                "result":if automatic_close { "withdraw_observed" } else { "measured_gio_limit" }
            }),
        );
        drop(runtime);
        drop(app);
        for _ in 0..10 {
            pump();
        }
        println!("MEASURED {scenario}: automatic CloseNotification after delayed Notify reply={automatic_close}; positive cleanup control=true; shell display not observed");
    }
    bus.down();
}

#[test]
#[ignore = "measures early FDO withdrawal with an owned delayed-reply service; not product acceptance"]
fn real_fdo_early_withdrawal_is_measured_before_delayed_notify_reply_and_cleaned_up() {
    if let Ok(role) = std::env::var(ROLE) {
        let root = PathBuf::from(std::env::var_os(ROOT).unwrap());
        crate::test_isolation::assert_below_check_root(&root);
        match role.as_str() {
            "delayed-daemon" => delayed_fdo_daemon(&root),
            "delayed-client" => delayed_fdo_client(&root, &std::env::var(CASE).unwrap()),
            other => panic!("unexpected delayed-reply role {other}"),
        }
        return;
    }
    let base = crate::test_isolation::check_root()
        .join(format!("desktop-early-withdrawal-{}", std::process::id()));
    fs::create_dir(&base).unwrap();
    for scenario in [
        "viewed",
        "off",
        "gio-control",
        "coalesce",
        "owner-swap",
        "errors",
        "deadline",
        "retired-owner",
        "notification-closed",
        "same-id",
        "shutdown",
        "action",
    ] {
        let root = base.join(scenario);
        fs::create_dir(&root).unwrap();
        let log = fs::File::create(root.join("scenario.log")).unwrap();
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                EARLY_TEST,
                "--ignored",
                "--test-threads=1",
                "--nocapture",
            ])
            .env(ROLE, "delayed-client")
            .env(CASE, scenario)
            .env(ROOT, &root)
            .env_remove("GNOTIFICATION_BACKEND")
            .env_remove("GIO_USE_PORTALS")
            .env_remove("SNAP")
            .stdin(Stdio::null())
            .stdout(log.try_clone().unwrap())
            .stderr(log);
        for (key, directory) in [
            ("HOME", "home"),
            ("XDG_CONFIG_HOME", "config"),
            ("XDG_DATA_HOME", "data"),
            ("XDG_CACHE_HOME", "cache"),
            ("XDG_STATE_HOME", "state"),
        ] {
            let path = root.join(directory);
            fs::create_dir(&path).unwrap();
            command.env(key, path);
        }
        OwnedChild(command.spawn().unwrap()).finish();
        let report: serde_json::Value =
            serde_json::from_slice(&fs::read(root.join("early-withdrawal.json")).unwrap()).unwrap();
        println!(
            "MEASURED EARLY FDO WITHDRAWAL {scenario}: {}; evidence={}",
            report["result"],
            root.display()
        );
    }
}
