use super::*;
use among::notifications::desktop::Runtime;
use among::notifications::{Focus, Host};
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
fn activatable_gtk_alone_is_not_a_gio_route_and_discovery_stays_explicit() {
    let mut services = Services {
        names: vec!["org.gtk.Notifications", FDO],
        owners: vec![None, None],
        activatable: None,
    };
    assert_eq!(services.availability(), Availability::Checking);
    services.owners.fill(Some(false));
    assert_eq!(services.availability(), Availability::Checking);
    services.activatable = Some(vec!["org.gtk.Notifications".into(), PORTAL.into()]);
    assert_eq!(services.availability(), Availability::Unavailable);
    services.activatable = Some(vec![FDO.into()]);
    assert_eq!(services.availability(), Availability::Available);
    services.names = vec![PORTAL];
    services.owners = vec![Some(false)];
    assert_eq!(services.availability(), Availability::Unavailable);
    services.activatable = Some(vec![PORTAL.into()]);
    assert_eq!(services.availability(), Availability::Available);
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
