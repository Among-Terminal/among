use super::*;
use crate::plugin::install::source::tests::{normal, Reply, Server};
use std::os::fd::AsRawFd;
use std::os::unix::fs::PermissionsExt;

#[test]
#[ignore = "requires an isolated native display, real keyboard input and Rust HTTPS package pipeline"]
fn retained_settings_deletion_is_separate_stale_safe_and_identity_scoped() {
    use crate::plugin::install::source::tests::{install_at, package_route};
    use serde_json::json;
    const KEY: &str = "github:12/sample";
    const OTHER: &str = "github:13/sample";
    let root = crate::plugin::inventory::root().unwrap();
    let directory = crate::settings::directory().unwrap();
    let config = directory.join("managed-plugins.json");
    if std::env::var("AMONG_RETAINED_SETTINGS_PHASE").as_deref() == Ok("offline") {
        crate::test_isolation::assert_below_check_root(&config);
        let saved =
            crate::settings::load(&config, crate::settings::Document::ManagedPlugins).unwrap();
        assert!(saved["plugins"][KEY].get("settings").is_none());
        assert!(saved["plugins"][OTHER].get("settings").is_none());
        assert_eq!(saved["plugins"][KEY]["future"], "keep");
        let mut host = crate::plugin::host::PluginHost::new();
        for entry in crate::plugin::inventory::read(&root).unwrap() {
            let local = crate::plugin::inventory::local(&saved, &entry.key());
            host.register_managed(entry, local);
        }
        assert!(!host.desired_enabled(KEY));
        assert_eq!(host.supervised_count(), 0);
        println!("PASS fresh-process offline deletion persistence and disabled fresh-token reinstall; no repository access");
        return;
    }
    let (metadata, archive) = crate::plugin::install::test_package("sample");
    let server =
        Server::new(move |request| package_route(metadata.clone(), archive.clone(), request));
    let selection = server
        .inspect(true)
        .unwrap()
        .select("sample", "1.0")
        .unwrap();
    let first = install_at(&server, selection.clone(), &root)
        .unwrap()
        .receipt;
    std::fs::create_dir_all(&directory).unwrap();
    std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700)).unwrap();
    let original = json!({"version":1,"future":"preserve","plugins":{
        KEY:{"enabled":false,"installation":first.installation,"review":first.review(vec!["status".into()]),"future":"keep","settings":{"version":42,"values":{"opaque":"never echo this value"},"future":"delete only by consent"}},
        OTHER:{"enabled":true,"future":"other origin","settings":{"version":99,"values":{"opaque":"other origin value"}}}}});
    std::fs::write(&config, original.to_string()).unwrap();
    std::fs::set_permissions(&config, std::fs::Permissions::from_mode(0o600)).unwrap();
    let legacy = directory.join("plugins.json");
    let legacy_bytes = b"{\"plugins\":[],\"future\":\"legacy retained byte-for-byte\"}";
    std::fs::write(&legacy, legacy_bytes).unwrap();
    let app = application("com.github.among-terminal.retained-settings-test");
    let ctx = build_with_writer(&app, None, crate::session::Writer::new).unwrap();
    wait_for(|| ctx.state.borrow().session_ready, Duration::from_secs(5));
    assert_eq!(ctx.state.borrow().plugins.supervised_count(), 0);
    let terminal = ctx
        .state
        .borrow()
        .pane(PaneId(1))
        .unwrap()
        .terminal()
        .clone();
    let pty = terminal.pty().unwrap().fd().as_raw_fd();
    open_preferences(&ctx);
    let preferences = gtk::Window::list_toplevels()
        .into_iter()
        .filter_map(|window| window.downcast::<adw::PreferencesWindow>().ok())
        .find(|window| window.title().as_deref() == Some("Preferences"))
        .unwrap();
    let page = widgets(preferences.upcast_ref())
        .into_iter()
        .filter_map(|widget| widget.downcast::<adw::PreferencesPage>().ok())
        .find(|page| page.title() == "Plugins")
        .unwrap();
    preferences.set_visible_page(&page);
    let open_retained = || {
        focus(&preferences);
        press(&button(&preferences, "Retained settings…"));
        wait_for(
            || {
                gtk::Window::list_toplevels().iter().any(|window| {
                    window.downcast_ref::<gtk::Window>().is_some_and(|window| {
                        window.title().as_deref() == Some("Retained plugin settings")
                            && window.is_visible()
                    })
                })
            },
            Duration::from_secs(2),
        );
        let window = gtk::Window::list_toplevels()
            .into_iter()
            .filter_map(|window| window.downcast::<adw::PreferencesWindow>().ok())
            .find(|window| {
                window.title().as_deref() == Some("Retained plugin settings") && window.is_visible()
            })
            .unwrap();
        focus(&window);
        window
    };
    let retained_row = |window: &adw::PreferencesWindow, key: &str| {
        widgets(window.upcast_ref())
            .into_iter()
            .filter_map(|widget| widget.downcast::<adw::ActionRow>().ok())
            .find(|row| row.title() == key)
            .unwrap()
    };
    let confirm_delete = |window: &adw::PreferencesWindow, row: &adw::ActionRow| {
        press(&button(row, "Delete settings…"));
        wait_for(|| window.visible_dialog().is_some(), Duration::from_secs(2));
        assert!(has_label(
            window.visible_dialog().unwrap().upcast_ref(),
            "Delete retained settings?"
        ));
    };
    let retained = open_retained();
    assert!(
        !has_label(retained.upcast_ref(), KEY),
        "installed settings are not a deletion candidate"
    );
    retained_row(&retained, OTHER);
    retained.destroy();
    focus(&preferences);
    let plugin_row = widgets(preferences.upcast_ref())
        .into_iter()
        .filter_map(|widget| widget.downcast::<adw::ActionRow>().ok())
        .find(|row| {
            widgets(row.upcast_ref())
                .into_iter()
                .filter_map(|widget| widget.downcast::<gtk::Button>().ok())
                .any(|button| button.label().as_deref() == Some("Details…"))
        })
        .unwrap();
    press(&button(&plugin_row, "Details…"));
    wait_for(
        || {
            gtk::Window::list_toplevels().iter().any(|window| {
                window.downcast_ref::<gtk::Window>().is_some_and(|window| {
                    window.title().as_deref() == Some("Plugin details") && window.is_visible()
                })
            })
        },
        Duration::from_secs(2),
    );
    let details = gtk::Window::list_toplevels()
        .into_iter()
        .filter_map(|window| window.downcast::<adw::PreferencesWindow>().ok())
        .find(|window| window.title().as_deref() == Some("Plugin details") && window.is_visible())
        .unwrap();
    focus(&details);
    assert!(has_label(details.upcast_ref(), "Release tag"));
    assert!(has_label(details.upcast_ref(), "v1.0"));
    assert!(has_label(details.upcast_ref(), "Resolved release commit"));
    press(&button(&details, "Uninstall…"));
    wait_for(
        || details.visible_dialog().is_some(),
        Duration::from_secs(2),
    );
    press(&button(&details.visible_dialog().unwrap(), "Uninstall"));
    wait_for(
        || {
            details
                .visible_dialog()
                .is_some_and(|dialog| has_label(dialog.upcast_ref(), "Plugin uninstalled"))
        },
        Duration::from_secs(5),
    );
    assert_eq!(
        crate::settings::load(&config, crate::settings::Document::ManagedPlugins).unwrap(),
        original,
        "native uninstall has not deleted settings or grants"
    );
    press(&button(&details.visible_dialog().unwrap(), "Close"));
    details.destroy();

    let retained = open_retained();
    let row = retained_row(&retained, KEY);
    assert!(!has_label(retained.upcast_ref(), "never echo this value"));
    let before = std::fs::read(&config).unwrap();
    confirm_delete(&retained, &row);
    screenshot_dialog(
        &retained.visible_dialog().unwrap(),
        "retained-settings-separate-consent",
    );
    xdo(&["key", "--clearmodifiers", "Escape"]);
    wait_for(
        || retained.visible_dialog().is_none(),
        Duration::from_secs(2),
    );
    assert_eq!(
        std::fs::read(&config).unwrap(),
        before,
        "Escape is a real no-op, not deletion consent"
    );
    confirm_delete(&retained, &row);
    crate::settings::update(
        &config,
        crate::settings::Document::ManagedPlugins,
        |document| {
            document["plugins"][KEY]["settings"]["future"] = json!("independent change");
            Ok(())
        },
    )
    .unwrap();
    let changed = std::fs::read(&config).unwrap();
    press(&button(
        &retained.visible_dialog().unwrap(),
        "Delete settings",
    ));
    wait_for(
        || row.subtitle().is_some_and(|text| text.contains("changed")),
        Duration::from_secs(2),
    );
    assert_eq!(std::fs::read(&config).unwrap(), changed);
    retained.destroy();

    let retained = open_retained();
    let row = retained_row(&retained, KEY);
    confirm_delete(&retained, &row);
    let reinstalled = install_at(&server, selection.clone(), &root)
        .unwrap()
        .receipt;
    // intentionally do not reconcile the UI host: the authoritative store must
    // refuse this stale confirmation even before its observer catches up.
    press(&button(
        &retained.visible_dialog().unwrap(),
        "Delete settings",
    ));
    wait_for(
        || {
            row.subtitle()
                .is_some_and(|text| text.contains("installed"))
        },
        Duration::from_secs(2),
    );
    assert_eq!(std::fs::read(&config).unwrap(), changed);
    let removal = crate::plugin::install::mutation::Job::start(
        root.clone(),
        reinstalled,
        crate::plugin::install::mutation::Action::Uninstall,
        None,
    )
    .unwrap();
    removal.supervised_stopped();
    let removed = RefCell::new(None);
    wait_for(
        || {
            if let Some(outcome) = removal.poll() {
                removed.replace(Some(outcome));
                true
            } else {
                false
            }
        },
        Duration::from_secs(5),
    );
    removed.into_inner().unwrap().unwrap();
    retained.destroy();

    let retained = open_retained();
    let row = retained_row(&retained, KEY);
    let mut expected =
        crate::settings::load(&config, crate::settings::Document::ManagedPlugins).unwrap();
    expected["plugins"][KEY]
        .as_object_mut()
        .unwrap()
        .remove("settings");
    let selection_bytes = std::fs::read(root.join("installed.json")).unwrap();
    let settings_bytes = std::fs::read(&config).unwrap();
    assert_ne!(
        unsafe { libc::geteuid() },
        0,
        "the native permission-refusal observation needs an unprivileged profile"
    );
    std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o500)).unwrap();
    confirm_delete(&retained, &row);
    press(&button(
        &retained.visible_dialog().unwrap(),
        "Delete settings",
    ));
    wait_for(
        || {
            row.subtitle()
                .is_some_and(|text| text.contains("No settings deletion accepted"))
        },
        Duration::from_secs(2),
    );
    std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700)).unwrap();
    assert_eq!(std::fs::read(&config).unwrap(), settings_bytes);
    confirm_delete(&retained, &row);
    press(&button(
        &retained.visible_dialog().unwrap(),
        "Delete settings",
    ));
    wait_for(
        || row.subtitle().as_deref() == Some("Retained settings deleted locally."),
        Duration::from_secs(2),
    );
    assert_eq!(
        crate::settings::load(&config, crate::settings::Document::ManagedPlugins).unwrap(),
        expected
    );
    assert_eq!(
        std::fs::read(root.join("installed.json")).unwrap(),
        selection_bytes
    );
    assert!(!button(&row, "Delete settings…").is_sensitive());
    let other = retained_row(&retained, OTHER);
    ctx.state
        .borrow_mut()
        .settings
        .fail_next_managed_confirmation();
    confirm_delete(&retained, &other);
    press(&button(
        &retained.visible_dialog().unwrap(),
        "Delete settings",
    ));
    wait_for(
        || {
            other
                .subtitle()
                .is_some_and(|text| text.contains("durability uncertain"))
        },
        Duration::from_secs(2),
    );
    expected["plugins"][OTHER]
        .as_object_mut()
        .unwrap()
        .remove("settings");
    assert_eq!(
        crate::settings::load(&config, crate::settings::Document::ManagedPlugins).unwrap(),
        expected
    );
    screenshot(&retained, "retained-settings-result-and-uncertainty");
    assert_eq!(std::fs::read(&legacy).unwrap(), legacy_bytes);
    assert_eq!(terminal.pty().unwrap().fd().as_raw_fd(), pty);
    retained.destroy();
    let retained = open_retained();
    assert!(has_label(
        retained.upcast_ref(),
        "No retained settings to delete"
    ));
    retained.destroy();
    ctx.state
        .borrow_mut()
        .settings
        .managed_enable_changed(KEY, true, first.installation.as_deref())
        .unwrap();
    let fresh = install_at(&server, selection, &root).unwrap().receipt;
    reconcile_managed_installations(&ctx.state);
    assert_ne!(fresh.installation, first.installation);
    assert!(!ctx.state.borrow().plugins.desired_enabled(KEY));
    assert_eq!(ctx.state.borrow().plugins.supervised_count(), 0);
    preferences.destroy();
    ctx.window.destroy();
    let child = std::process::Command::new(std::env::current_exe().unwrap()).args(["--exact", "app::plugin_tests::installation::retained_settings_deletion_is_separate_stale_safe_and_identity_scoped", "--ignored", "--nocapture", "--test-threads=1"]).env("AMONG_RETAINED_SETTINGS_PHASE", "offline").output().unwrap();
    assert!(
        child.status.success(),
        "{} {}",
        String::from_utf8_lossy(&child.stdout),
        String::from_utf8_lossy(&child.stderr)
    );
    print!("{}", String::from_utf8_lossy(&child.stdout));
    println!("PASS real native keyboard uninstall retains settings, separate deletion Escape/cancel, changed-settings refusal, store-authoritative stale-reinstall refusal before host reconcile, actual permission refusal without accepted deletion, exact-payload-only deletion, truthful post-rename uncertainty, preserved unknown/grant/other-origin/legacy fields and unchanged terminal PTY. No downloaded process was executed; fresh reinstall remains OFF by token. This does not close the original modal terminal-input or combined-provider gates.");
}

#[test]
#[ignore = "requires isolated native keyboard input, Rust HTTPS and built managed processes"]
fn explicit_update_consent_restarts_only_selected_plugin_and_later_disable_wins() {
    use crate::plugin::install::{
        source::tests::{install_at, package_route},
        test_managed_package,
    };
    use serde_json::json;
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    };
    const KEY: &str = "github:12/sample";
    if std::env::var("AMONG_NATIVE_LIFECYCLE_PHASE").as_deref() == Ok("offline") {
        let root = crate::plugin::inventory::root().unwrap();
        let document: serde_json::Value = serde_json::from_slice(
            &std::fs::read(
                crate::settings::directory()
                    .unwrap()
                    .join("managed-plugins.json"),
            )
            .unwrap(),
        )
        .unwrap();
        let mut host = crate::plugin::host::PluginHost::new();
        for entry in crate::plugin::inventory::read(&root).unwrap() {
            let local = crate::plugin::inventory::local(&document, &entry.key());
            host.register_managed(entry, local);
        }
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while host.running_generation(KEY).is_none() {
            host.poll();
            assert!(std::time::Instant::now() < deadline, "{:?}", host.listing());
            std::thread::sleep(Duration::from_millis(2));
        }
        let receipt = host.managed_details(KEY).unwrap().0.receipt;
        host.request_status(KEY, "app_header", &Context::default());
        while !receipt
            .package_path(&root)
            .join("resource-observed")
            .exists()
        {
            host.poll();
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(
            std::fs::read_to_string(receipt.package_path(&root).join("resource-observed")).unwrap(),
            "4.0"
        );
        host.shutdown();
        while !host.is_stopped() {
            host.poll();
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(2));
        }
        println!("PASS fresh-process offline inventory and explicitly enabled exact installed generation with retained settings, no catalogue request or probe");
        return;
    }
    let root = crate::plugin::inventory::root().unwrap();
    let (first_meta, first_bytes) = test_managed_package("1.0", "managed-resource", |_| {});
    let first_server =
        Server::new(move |request| package_route(first_meta.clone(), first_bytes.clone(), request));
    let first = install_at(
        &first_server,
        first_server
            .inspect(true)
            .unwrap()
            .select("sample", "1.0")
            .unwrap(),
        &root,
    )
    .unwrap()
    .receipt;
    let config = crate::settings::directory().unwrap();
    std::fs::create_dir_all(&config).unwrap();
    std::fs::set_permissions(&config, std::fs::Permissions::from_mode(0o700)).unwrap();
    let local=json!({"version":1,"future":"preserved","plugins":{KEY:{"enabled":true,"installation":first.installation,"review":first.review(vec!["status".into(),"settings".into()]),"future":"keep","settings":{"version":1,"values":{"label":"retained","opaque":"preserve"}}}}}).to_string();
    let config = config.join("managed-plugins.json");
    std::fs::write(&config, &local).unwrap();
    std::fs::set_permissions(&config, std::fs::Permissions::from_mode(0o600)).unwrap();
    let app = application("com.github.among-terminal.explicit-update-test");
    let ctx = build_with_writer(&app, None, crate::session::Writer::new).unwrap();
    wait_for(
        || {
            ctx.state.borrow().session_ready
                && ctx.state.borrow().plugins.running_generation(KEY).is_some()
        },
        Duration::from_secs(5),
    );
    ctx.window.set_title(Some("Update fixture terminal"));
    let terminal = ctx
        .state
        .borrow()
        .pane(PaneId(1))
        .unwrap()
        .terminal()
        .clone();
    let pty = terminal.pty().unwrap().fd().as_raw_fd();
    let first_pid = std::fs::read_to_string(first.package_path(&root).join("executed")).unwrap();
    ctx.state.borrow_mut().plugins.register(PluginSpec {
        name: "sibling".into(),
        command: fixture("echo"),
        enabled: true,
    });
    wait_for(
        || {
            ctx.state
                .borrow()
                .plugins
                .listing()
                .iter()
                .any(|(key, state)| key == "sibling" && state == &PluginState::Running)
        },
        Duration::from_secs(5),
    );
    let sibling_pid = ctx
        .state
        .borrow()
        .plugins
        .supervised_pids()
        .into_iter()
        .find(|pid| pid.to_string() != first_pid)
        .unwrap();
    let release = Arc::new(AtomicBool::new(false));
    let (metadata, archive) = test_managed_package("2.0", "managed-resource", |_| {});
    let server = Rc::new(Server::new({
        let release = release.clone();
        move |request| {
            if request.contains("/github-production-release-asset/12/") {
                let deadline = std::time::Instant::now() + Duration::from_secs(10);
                while !release.load(Ordering::Acquire) {
                    assert!(std::time::Instant::now() < deadline);
                    std::thread::sleep(Duration::from_millis(1));
                }
            }
            package_route(metadata.clone(), archive.clone(), request)
        }
    }));
    crate::prefs::repositories::present_updates_for_test(
        ctx.window.upcast_ref(),
        Rc::new({
            let server = server.clone();
            move |repository| server.job(repository)
        }),
        Rc::new({
            let server = server.clone();
            let root = root.clone();
            move |selection| server.install(selection, root.clone())
        }),
        installation_observer(&ctx),
        Rc::new({
            let state = ctx.state.clone();
            move |key| state.borrow().plugins.managed_details(key)
        }),
    );
    let view = inspector();
    focus(&view);
    let address = widgets(view.upcast_ref())
        .into_iter()
        .find_map(|widget| widget.downcast::<adw::EntryRow>().ok())
        .unwrap();
    assert!(address.grab_focus());
    xdo(&["type", "--clearmodifiers", "--delay", "0", "example/sample"]);
    press(&button(&view, "Inspect"));
    wait_for(
        || has_label(view.upcast_ref(), "Metadata preview ready"),
        Duration::from_secs(5),
    );
    press(&button(&view, "Apply update…"));
    wait_for(|| view.visible_dialog().is_some(), Duration::from_secs(2));
    let dialog = view.visible_dialog().unwrap();
    assert!(widgets(dialog.upcast_ref())
        .iter()
        .filter_map(|widget| widget.downcast_ref::<gtk::Label>())
        .any(|label| label.text().contains("Only this plugin may restart")));
    press(&button(&dialog, "Cancel"));
    wait_for(|| view.visible_dialog().is_none(), Duration::from_secs(2));
    assert_eq!(server.requests().len(), 3);
    assert_eq!(
        crate::plugin::inventory::read(&root).unwrap()[0].receipt,
        first
    );
    press(&button(&view, "Apply update…"));
    wait_for(|| view.visible_dialog().is_some(), Duration::from_secs(2));
    screenshot_dialog(&view.visible_dialog().unwrap(), "update-consent");
    press(&button(&view.visible_dialog().unwrap(), "Apply update"));
    wait_for(|| server.requests().len() == 8, Duration::from_secs(5));
    let old_path = first.package_path(&root);
    std::fs::remove_file(old_path.join("resource-observed")).unwrap();
    wait_for(
        || old_path.join("resource-observed").exists(),
        Duration::from_secs(3),
    );
    assert_eq!(
        std::fs::read_to_string(old_path.join("resource-observed")).unwrap(),
        "1.0",
        "real old process opens its own immutable resources during pending download"
    );
    assert_eq!(
        crate::plugin::inventory::read(&root).unwrap()[0].receipt,
        first
    );
    release.store(true, Ordering::Release);
    wait_for(
        || {
            ctx.state.borrow().plugins.overview().iter().any(|info| {
                info.key == KEY
                    && info.version.as_deref() == Some("2.0")
                    && info.running_version.as_deref() == Some("2.0")
                    && info.state == PluginState::Running
            })
        },
        Duration::from_secs(8),
    );
    let second = crate::plugin::inventory::read(&root)
        .unwrap()
        .remove(0)
        .receipt;
    assert_eq!(second.installation, first.installation);
    assert_ne!(second.generation, first.generation);
    assert_ne!(
        std::fs::read_to_string(second.package_path(&root).join("executed")).unwrap(),
        first_pid
    );
    assert_eq!(std::fs::read(&config).unwrap(), local.as_bytes());
    assert_eq!(terminal.pty().unwrap().fd().as_raw_fd(), pty);
    assert!(ctx
        .state
        .borrow()
        .plugins
        .supervised_pids()
        .contains(&sibling_pid));
    assert!(old_path.join("version-resource").exists());
    screenshot(&view, "updated-selected-and-running");
    view.destroy();
    let (metadata, archive) = test_managed_package("3.0", "managed-wrong-version", |_| {});
    let release = Arc::new(AtomicBool::new(false));
    let server = Server::new({
        let release = release.clone();
        move |request| {
            if request.contains("/github-production-release-asset/12/") {
                let deadline = std::time::Instant::now() + Duration::from_secs(10);
                while !release.load(Ordering::Acquire) {
                    assert!(std::time::Instant::now() < deadline);
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
        .select("sample", "3.0")
        .unwrap()
        .for_update(&entry, stored.as_ref())
        .unwrap();
    let job = server.install(selected, root.clone()).unwrap();
    wait_for(|| server.requests().len() == 8, Duration::from_secs(5));
    ctx.state.borrow_mut().plugins.disable(KEY);
    ctx.state
        .borrow_mut()
        .settings
        .plugin_changed(KEY, false)
        .unwrap();
    release.store(true, Ordering::Release);
    let outcome = RefCell::new(None);
    wait_for(
        || {
            let result = job.poll();
            if result.is_some() {
                outcome.replace(result);
                true
            } else {
                false
            }
        },
        Duration::from_secs(5),
    );
    outcome.into_inner().unwrap().unwrap();
    reconcile_managed_installations(&ctx.state);
    wait_for(
        || {
            ctx.state
                .borrow()
                .plugins
                .listing()
                .iter()
                .any(|(key, state)| key == KEY && state == &PluginState::Disabled)
        },
        Duration::from_secs(5),
    );
    assert!(!ctx.state.borrow().plugins.desired_enabled(KEY));
    let third = crate::plugin::inventory::read(&root)
        .unwrap()
        .remove(0)
        .receipt;
    assert!(!third.package_path(&root).join("executed").exists());
    assert!(ctx
        .state
        .borrow()
        .plugins
        .supervised_pids()
        .contains(&sibling_pid));
    let saved: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&config).unwrap()).unwrap();
    assert_eq!(saved["plugins"][KEY]["future"], "keep");
    assert_eq!(
        saved["plugins"][KEY]["settings"]["values"]["opaque"],
        "preserve"
    );
    assert_eq!(saved["plugins"][KEY]["enabled"], false);
    assert_eq!(terminal.pty().unwrap().fd().as_raw_fd(), pty);
    ctx.state.borrow_mut().plugins.enable(KEY);
    ctx.state
        .borrow_mut()
        .settings
        .managed_enable_changed(KEY, true, third.installation.as_deref())
        .unwrap();
    wait_for(
        || {
            third.package_path(&root).join("executed").exists()
                && ctx.state.borrow().plugins.running_generation(KEY).is_none()
        },
        Duration::from_secs(5),
    );
    open_preferences(&ctx);
    let preferences = gtk::Window::list_toplevels()
        .into_iter()
        .filter_map(|window| window.downcast::<adw::PreferencesWindow>().ok())
        .find(|window| window.title().as_deref() == Some("Preferences"))
        .unwrap();
    let page = widgets(preferences.upcast_ref())
        .into_iter()
        .filter_map(|widget| widget.downcast::<adw::PreferencesPage>().ok())
        .find(|page| page.title() == "Plugins")
        .unwrap();
    preferences.set_visible_page(&page);
    let open_details = || {
        focus(&preferences);
        let row = widgets(preferences.upcast_ref())
            .into_iter()
            .filter_map(|widget| widget.downcast::<adw::ActionRow>().ok())
            .find(|row| row.title() == "Equal display name")
            .unwrap();
        press(&button(&row, "Details…"));
        wait_for(
            || {
                gtk::Window::list_toplevels().iter().any(|window| {
                    window
                        .downcast_ref::<adw::PreferencesWindow>()
                        .is_some_and(|window| {
                            window.title().as_deref() == Some("Plugin details")
                                && window.is_visible()
                        })
                })
            },
            Duration::from_secs(3),
        );
        let details = gtk::Window::list_toplevels()
            .into_iter()
            .filter_map(|window| window.downcast::<adw::PreferencesWindow>().ok())
            .find(|window| {
                window.title().as_deref() == Some("Plugin details") && window.is_visible()
            })
            .unwrap();
        focus(&details);
        details
    };
    let details = open_details();
    press(&button(&details, "Recover previous…"));
    wait_for(
        || details.visible_dialog().is_some(),
        Duration::from_secs(2),
    );
    screenshot_dialog(
        &details.visible_dialog().unwrap(),
        "recover-previous-consent",
    );
    press(&button(
        &details.visible_dialog().unwrap(),
        "Recover previous",
    ));
    wait_for(
        || {
            details
                .visible_dialog()
                .is_some_and(|dialog| has_label(dialog.upcast_ref(), "Previous package selected"))
        },
        Duration::from_secs(5),
    );
    wait_for(
        || {
            ctx.state.borrow().plugins.overview().iter().any(|info| {
                info.key == KEY
                    && info.version.as_deref() == Some("2.0")
                    && info.running_version.as_deref() == Some("2.0")
                    && info.state == PluginState::Running
            })
        },
        Duration::from_secs(5),
    );
    assert_eq!(
        crate::plugin::inventory::read(&root).unwrap()[0].receipt,
        second
    );
    assert!(
        third.package_path(&root).join("executed").exists(),
        "recovery does not claim to reverse independently written effects"
    );
    press(&button(&details.visible_dialog().unwrap(), "Close"));
    details.destroy();
    wait_for(
        || {
            ctx.state
                .borrow()
                .plugins
                .supervised_pids()
                .contains(&sibling_pid)
        },
        Duration::from_secs(2),
    );
    let (metadata, archive) = test_managed_package("4.0", "managed-resource", |_| {});
    let release = Arc::new(AtomicBool::new(false));
    let late_server = Server::new({
        let release = release.clone();
        move |request| {
            if request.contains("/github-production-release-asset/12/") {
                let deadline = std::time::Instant::now() + Duration::from_secs(15);
                while !release.load(Ordering::Acquire) {
                    assert!(std::time::Instant::now() < deadline);
                    std::thread::sleep(Duration::from_millis(1));
                }
            }
            package_route(metadata.clone(), archive.clone(), request)
        }
    });
    let (entry, stored) = ctx.state.borrow().plugins.managed_details(KEY).unwrap();
    let selected = late_server
        .inspect(true)
        .unwrap()
        .select("sample", "4.0")
        .unwrap();
    let late_job = late_server
        .install(
            selected
                .clone()
                .for_update(&entry, stored.as_ref())
                .unwrap(),
            root.clone(),
        )
        .unwrap();
    wait_for(|| late_server.requests().len() == 8, Duration::from_secs(5));
    let details = open_details();
    press(&button(&details, "Uninstall…"));
    wait_for(
        || details.visible_dialog().is_some(),
        Duration::from_secs(2),
    );
    screenshot_dialog(&details.visible_dialog().unwrap(), "uninstall-consent");
    press(&button(&details.visible_dialog().unwrap(), "Uninstall"));
    wait_for(
        || {
            details
                .visible_dialog()
                .is_some_and(|dialog| has_label(dialog.upcast_ref(), "Plugin uninstalled"))
        },
        Duration::from_secs(8),
    );
    assert!(crate::plugin::inventory::read(&root).unwrap().is_empty());
    assert!(ctx.state.borrow().plugins.managed_details(KEY).is_none());
    assert!(ctx
        .state
        .borrow()
        .plugins
        .supervised_pids()
        .contains(&sibling_pid));
    assert_eq!(terminal.pty().unwrap().fd().as_raw_fd(), pty);
    release.store(true, Ordering::Release);
    let late = RefCell::new(None);
    wait_for(
        || {
            let result = late_job.poll();
            if result.is_some() {
                late.replace(result);
                true
            } else {
                false
            }
        },
        Duration::from_secs(5),
    );
    assert!(late.into_inner().unwrap().is_err());
    reconcile_managed_installations(&ctx.state);
    assert!(crate::plugin::inventory::read(&root).unwrap().is_empty());
    assert!(ctx.state.borrow().plugins.managed_details(KEY).is_none());
    let retained: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&config).unwrap()).unwrap();
    assert_eq!(
        retained["plugins"][KEY]["settings"]["values"]["opaque"],
        "preserve"
    );
    assert_eq!(retained["plugins"][KEY]["enabled"], false);
    assert!(
        second.package_path(&root).exists(),
        "held inventory/resource lease defers generation cleanup after removed selection"
    );
    ctx.state
        .borrow_mut()
        .settings
        .managed_enable_changed(KEY, true, second.installation.as_deref())
        .unwrap();
    let fresh_job = late_server.install(selected, root.clone()).unwrap();
    let fresh = RefCell::new(None);
    wait_for(
        || {
            let result = fresh_job.poll();
            if result.is_some() {
                fresh.replace(result);
                true
            } else {
                false
            }
        },
        Duration::from_secs(5),
    );
    let fresh = fresh.into_inner().unwrap().unwrap().receipt;
    reconcile_managed_installations(&ctx.state);
    assert_ne!(fresh.installation, second.installation);
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
        "preserve"
    );
    screenshot(&details, "removed-then-disabled-reinstall");
    details.destroy();
    preferences.destroy();
    ctx.window.destroy();
    ctx.state
        .borrow_mut()
        .settings
        .managed_enable_changed(KEY, true, fresh.installation.as_deref())
        .unwrap();
    let child=std::process::Command::new(std::env::current_exe().unwrap()).args(["--exact","app::plugin_tests::installation::explicit_update_consent_restarts_only_selected_plugin_and_later_disable_wins","--ignored","--nocapture","--test-threads=1"]).env("AMONG_NATIVE_LIFECYCLE_PHASE","offline").output().unwrap();
    assert!(
        child.status.success(),
        "{} {}",
        String::from_utf8_lossy(&child.stdout),
        String::from_utf8_lossy(&child.stderr)
    );
    print!("{}", String::from_utf8_lossy(&child.stdout));
    println!("PASS native update consent/cancel, target-only restart and live old resource opens; later disable wins; real failed API admission recovers previous through explicit native action; uninstall invalidates pending HTTPS update, waits for supervised stop and commits removed selection; late callback cannot resurrect; leases defer cleanup and reinstall has fresh disabled token despite retained enabled preference/settings. Sibling and terminal PTY unchanged. Modal input and combined providers remain separate open gates.");
}

#[test]
#[ignore = "requires an isolated display and actual Rust HTTPS installation worker"]
fn active_installer_completion_targets_replacement_window_not_the_retiring_host() {
    use crate::plugin::install::source::tests::package_route;
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    };
    let app = application("com.github.among-terminal.installer-window-replacement-test");
    let first = build_with_writer(&app, None, crate::session::Writer::new).unwrap();
    wait_for(
        || first.state.borrow().session_ready,
        Duration::from_secs(5),
    );
    let observer = installation_observer(&first);
    let root = crate::plugin::inventory::root().unwrap();
    let (metadata, archive) = crate::plugin::install::test_package("sample");
    let release = Arc::new(AtomicBool::new(false));
    let server = Server::new({
        let release = release.clone();
        move |request| {
            if request.starts_with("GET /repositories/12/releases/assets/11 ") {
                let deadline = std::time::Instant::now() + Duration::from_secs(10);
                while !release.load(Ordering::Acquire) {
                    assert!(std::time::Instant::now() < deadline);
                    std::thread::sleep(Duration::from_millis(1));
                }
            }
            package_route(metadata.clone(), archive.clone(), request)
        }
    });
    let selected = server
        .inspect(true)
        .unwrap()
        .select("sample", "1.0")
        .unwrap();
    let job = server.install(selected, root.clone()).unwrap();
    wait_for(
        || {
            server
                .requests()
                .iter()
                .any(|request| request.starts_with("GET /repositories/12/releases/assets/11 "))
        },
        Duration::from_secs(5),
    );
    first.window.destroy();
    assert!(first.state.borrow().exiting);
    let second = build_with_writer(&app, None, crate::session::Writer::new).unwrap();
    wait_for(
        || second.state.borrow().session_ready,
        Duration::from_secs(5),
    );
    assert!(second.state.borrow().plugins.listing().is_empty());
    release.store(true, Ordering::Release);
    let outcome = RefCell::new(None);
    wait_for(
        || {
            let result = job.poll();
            if result.is_some() {
                outcome.replace(result);
                true
            } else {
                false
            }
        },
        Duration::from_secs(5),
    );
    assert!(outcome.into_inner().unwrap().is_ok());
    observer();
    assert!(
        first.state.borrow().plugins.listing().is_empty(),
        "a retiring host cannot receive a late installation"
    );
    assert_eq!(second.state.borrow().plugins.listing().len(), 1);
    assert!(!second
        .state
        .borrow()
        .plugins
        .desired_enabled("github:12/sample"));
    assert_eq!(second.state.borrow().plugins.supervised_count(), 0);
    assert_eq!(crate::plugin::inventory::read(&root).unwrap().len(), 1);
    second.window.destroy();
    println!("PASS a real pending HTTPS installation survives owned window replacement; its committed outcome reconciles only the current live host, disabled, without registering into the retiring host.");
}

#[test]
#[ignore = "requires isolated native windows, xdotool and controlled Rust HTTPS"]
fn registered_sources_cache_refresh_trust_and_removal_are_real_and_separate_from_installation() {
    use serde_json::json;
    use std::sync::{
        atomic::{AtomicU8, Ordering},
        Arc,
    };
    let installed = crate::plugin::inventory::root().unwrap();
    crate::plugin::inventory::tests::fixture(&installed);
    let selected = std::fs::read(installed.join("installed.json")).unwrap();
    let source_root = crate::plugin::install::registry::root().unwrap();
    let app = application("com.github.among-terminal.source-cache-test");
    let ctx = build_with_writer(&app, None, crate::session::Writer::new).unwrap();
    wait_for(|| ctx.state.borrow().session_ready, Duration::from_secs(5));
    let directory = crate::settings::directory().unwrap();
    let preferences_before = ["managed-plugins.json", "plugins.json", "shortcuts.json"]
        .map(|name| std::fs::read(directory.join(name)).ok());
    let mode = Arc::new(AtomicU8::new(0));
    let server = Rc::new(Server::new({
        let mode = mode.clone();
        move |request| {
            let path = request.split_whitespace().nth(1).unwrap();
            let mode = mode.load(Ordering::Acquire);
            if path == "/repos/example/sample" || path == "/repositories/12" {
                if mode == 3 {
                    return Reply::rate_limited();
                }
                if mode == 4 {
                    return Reply::held();
                }
                Reply::json(
                    json!({"id":12,"owner":{"login":if mode==2 {"new-publisher"}else{"example"},"id":if mode==2 {99}else{34}},"name":if mode==0 {"sample"}else{"renamed"},"private":false,"default_branch":"main"}),
                )
            } else if path == "/repositories/12/commits/main" {
                Reply::json(json!({"sha":"a".repeat(40)}))
            } else if path.ends_with("/among-plugins.json") {
                Reply::json(crate::plugin::install::test_package("sample").0)
            } else {
                panic!("unexpected source fixture path: {path}");
            }
        }
    }));
    let present = || {
        crate::prefs::repositories::present_sources_for_test(
            ctx.window.upcast_ref(),
            Rc::new({
                let server = server.clone();
                move |repository| server.job(repository)
            }),
            Rc::new({
                let server = server.clone();
                move |id| server.refresh(id)
            }),
            Rc::new({
                let server = server.clone();
                let installed = installed.clone();
                move |selection| server.install(selection, installed.clone())
            }),
            Rc::new({
                let state = ctx.state.clone();
                move || reconcile_managed_installations(&state)
            }),
            source_root.clone(),
        )
    };
    present();
    let view = inspector();
    focus(&view);
    let entry = widgets(view.upcast_ref())
        .into_iter()
        .find_map(|widget| widget.downcast::<adw::EntryRow>().ok())
        .unwrap();
    assert!(entry.grab_focus());
    xdo(&["type", "--clearmodifiers", "--delay", "0", "example/sample"]);
    press(&button(&view, "Inspect"));
    wait_for(
        || has_label(view.upcast_ref(), "Metadata preview ready"),
        Duration::from_secs(5),
    );
    assert!(
        !source_root.join("sources.json").exists(),
        "opening/editing/inspection must not register a source"
    );
    press(&button(&view, "Add source"));
    wait_for(
        || has_label(view.upcast_ref(), "Source change saved"),
        Duration::from_secs(5),
    );
    assert_eq!(server.requests().len(), 3);
    let source = || {
        serde_json::from_slice::<serde_json::Value>(
            &std::fs::read(source_root.join("sources.json")).unwrap(),
        )
        .unwrap()
    };
    let initial = source();
    assert_eq!(initial["registrations"][0]["origin"]["repository_id"], 12);
    assert_eq!(initial["registrations"][0]["origin"]["publisher_id"], 34);
    assert!(
        initial["registrations"][0]["snapshots"][0]["retrieved_at"]
            .as_u64()
            .unwrap()
            > 0
    );
    screenshot(&view, "source-registered");
    view.destroy();
    present();
    let view = inspector();
    focus(&view);
    wait_for(
        || has_label(view.upcast_ref(), "Cached source registrations loaded"),
        Duration::from_secs(5),
    );
    press(&button(&view, "Cached"));
    wait_for(
        || has_label(view.upcast_ref(), "Cached metadata; not an online check"),
        Duration::from_secs(5),
    );
    assert_eq!(
        server.requests().len(),
        3,
        "cache reopen must make no network request"
    );
    let mut stale = source();
    stale["registrations"][0]["snapshots"][0]["retrieved_at"] =
        json!(crate::plugin::install::registry::now().unwrap() - 24 * 60 * 60);
    std::fs::write(source_root.join("sources.json"), stale.to_string()).unwrap();
    view.destroy();
    present();
    let view = inspector();
    focus(&view);
    wait_for(
        || has_label(view.upcast_ref(), "Cached source registrations loaded"),
        Duration::from_secs(5),
    );
    press(&button(&view, "Cached"));
    wait_for(
        || {
            has_label(
                view.upcast_ref(),
                "Cached metadata is stale or its clock is uncertain",
            )
        },
        Duration::from_secs(5),
    );
    assert_eq!(
        server.requests().len(),
        3,
        "stale cached inspection is still offline and does not imply a successful refresh"
    );
    screenshot(&view, "source-stale-offline");
    mode.store(1, Ordering::Release);
    press(&button(&view, "Refresh"));
    wait_for(
        || {
            source()["registrations"][0]["origin"]["repository"] == "renamed"
                && has_label(view.upcast_ref(), "Source change saved")
        },
        Duration::from_secs(5),
    );
    assert_eq!(
        source()["registrations"][0]["origin"]["repository"],
        "renamed"
    );
    assert_eq!(
        source()["registrations"][0]["token"],
        initial["registrations"][0]["token"]
    );
    assert!(
        server.requests()[3].starts_with("GET /repositories/12 "),
        "refresh must target the registered numeric ID, not a reused repository name"
    );
    let renamed = std::fs::read(source_root.join("sources.json")).unwrap();
    mode.store(2, Ordering::Release);
    press(&button(&view, "Refresh"));
    wait_for(
        || has_label(view.upcast_ref(), "Source approval was not changed"),
        Duration::from_secs(5),
    );
    screenshot(&view, "source-transfer-review");
    assert_eq!(
        std::fs::read(source_root.join("sources.json")).unwrap(),
        renamed,
        "publisher transfer is not implicitly approved by refresh"
    );
    press(&button(&view, "Approve publisher change"));
    wait_for(
        || {
            source()["registrations"][0]["origin"]["publisher_id"] == 99
                && has_label(view.upcast_ref(), "Source change saved")
        },
        Duration::from_secs(5),
    );
    assert_eq!(source()["registrations"][0]["origin"]["publisher_id"], 99);
    assert_ne!(
        source()["registrations"][0]["token"],
        initial["registrations"][0]["token"]
    );
    let transferred = std::fs::read(source_root.join("sources.json")).unwrap();
    mode.store(3, Ordering::Release);
    press(&button(&view, "Refresh"));
    wait_for(
        || {
            has_label(
                view.upcast_ref(),
                "Refresh failed; retained metadata remains available offline",
            )
        },
        Duration::from_secs(5),
    );
    assert_eq!(
        std::fs::read(source_root.join("sources.json")).unwrap(),
        transferred
    );
    press(&button(&view, "Cached"));
    wait_for(
        || has_label(view.upcast_ref(), "Cached metadata; not an online check"),
        Duration::from_secs(5),
    );
    mode.store(4, Ordering::Release);
    let before = server.requests().len();
    press(&button(&view, "Refresh"));
    wait_for(|| server.requests().len() > before, Duration::from_secs(5));
    assert!(!button(&view, "Cached").is_sensitive());
    assert!(!button(&view, "Refresh").is_sensitive());
    assert!(!button(&view, "Remove source").is_sensitive());
    screenshot(&view, "source-refresh-cancellable");
    press(&button(&view, "Cancel source request"));
    wait_for(
        || {
            has_label(
                view.upcast_ref(),
                "Refresh failed; retained metadata remains available offline",
            )
        },
        Duration::from_secs(5),
    );
    assert_eq!(
        std::fs::read(source_root.join("sources.json")).unwrap(),
        transferred
    );
    press(&button(&view, "Remove source"));
    wait_for(
        || {
            source()["registrations"].as_array().unwrap().is_empty()
                && has_label(view.upcast_ref(), "Source change saved")
        },
        Duration::from_secs(5),
    );
    assert!(source()["registrations"].as_array().unwrap().is_empty());
    assert_eq!(
        std::fs::read(installed.join("installed.json")).unwrap(),
        selected
    );
    assert_eq!(
        ["managed-plugins.json", "plugins.json", "shortcuts.json"]
            .map(|name| std::fs::read(directory.join(name)).ok()),
        preferences_before
    );
    assert_eq!(ctx.state.borrow().plugins.supervised_count(), 0);
    assert!(!installed.join("executed").exists());
    screenshot(&view, "source-removed-installed-preserved");
    view.destroy();
    ctx.window.destroy();
    println!("PASS actual source Add, fresh-view offline cache, numeric Refresh/rename, explicit transferred publisher approval, actual rate-limit/cancellation and Remove without changing selected installation, enablement or settings. Native window hierarchy retained; simultaneous terminal input is exercised separately.");
}

#[test]
#[ignore = "requires an isolated display, xdotool, ImageMagick and real Rust HTTPS fixtures"]
fn selected_ready_built_package_commits_disabled_into_the_live_native_manager() {
    native_install(false);
}

#[test]
#[ignore = "requires an isolated display, xdotool, ImageMagick and real Rust HTTPS fixtures"]
fn committed_install_reconciles_even_after_its_native_view_is_destroyed() {
    native_install(true);
}

fn native_install(close_after_commit: bool) {
    use crate::plugin::install::source::tests::package_route;
    use crate::plugin::install::test_package as fixture_for;
    use serde_json::json;
    let root = crate::plugin::inventory::root().unwrap();
    let directory = crate::settings::directory().unwrap();
    std::fs::create_dir_all(&directory).unwrap();
    std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700)).unwrap();
    let retained = json!({"version":1,"future":"preserved","plugins":{"github:12/sample":{
        "enabled":true,"installation":"0".repeat(64),"custom":42,
        "settings":{"version":1,"values":{"label":"retained value","opaque":"keep"}}}}})
    .to_string();
    std::fs::write(directory.join("managed-plugins.json"), &retained).unwrap();
    std::fs::set_permissions(
        directory.join("managed-plugins.json"),
        std::fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    let app = application("com.github.among-terminal.installation-test");
    let ctx = build_with_writer(&app, None, crate::session::Writer::new).unwrap();
    wait_for(|| ctx.state.borrow().session_ready, Duration::from_secs(5));
    ctx.window.set_title(Some("Installer fixture terminal"));
    focus(&ctx.window);
    let terminal = ctx
        .state
        .borrow()
        .pane(PaneId(1))
        .unwrap()
        .terminal()
        .clone();
    let initial_pty = terminal.pty().unwrap().fd().as_raw_fd();
    assert!(ctx.state.borrow().plugins.listing().is_empty());
    let (mut first, archive) = fixture_for("sample");
    let (mut second, _) = fixture_for("second");
    first.as_object_mut().unwrap().remove("schema_version");
    second.as_object_mut().unwrap().remove("schema_version");
    second["artifacts"][0]["asset_id"] = json!(22);
    let catalogue = json!({"schema_version":1,"plugins":[{"id":"sample","versions":[first.clone()]},{"id":"second","versions":[second]}]});
    let release = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(!close_after_commit));
    let worker_release = release.clone();
    let server = Rc::new(Server::new(move |request| {
        if request.contains("/github-production-release-asset/12/") {
            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            while !worker_release.load(std::sync::atomic::Ordering::Acquire) {
                assert!(std::time::Instant::now() < deadline);
                std::thread::sleep(Duration::from_millis(1));
            }
        }
        if request
            .split_whitespace()
            .nth(1)
            .unwrap()
            .ends_with("/among-plugins.json")
        {
            // the catalogue is retrieved by the actual revision-pinned client.
            let mut reply = package_route(first.clone(), archive.clone(), request);
            reply.set_json(catalogue.clone());
            reply
        } else {
            package_route(first.clone(), archive.clone(), request)
        }
    }));
    open_preferences(&ctx);
    let preferences = gtk::Window::list_toplevels()
        .into_iter()
        .filter_map(|window| window.downcast::<adw::PreferencesWindow>().ok())
        .find(|window| window.title().as_deref() == Some("Preferences"))
        .unwrap();
    let page = widgets(preferences.upcast_ref())
        .into_iter()
        .filter_map(|widget| widget.downcast::<adw::PreferencesPage>().ok())
        .find(|page| page.title() == "Plugins")
        .unwrap();
    preferences.set_visible_page(&page);
    crate::prefs::repositories::present_installer_for_test(
        preferences.upcast_ref(),
        Rc::new({
            let server = server.clone();
            move |repository| server.job(repository)
        }),
        Rc::new({
            let server = server.clone();
            let root = root.clone();
            move |selection| server.install(selection, root.clone())
        }),
        Rc::new({
            let state = ctx.state.clone();
            move || reconcile_managed_installations(&state)
        }),
    );
    let view = inspector();
    focus(&view);
    let entry = widgets(view.upcast_ref())
        .into_iter()
        .find_map(|widget| widget.downcast::<adw::EntryRow>().ok())
        .unwrap();
    assert!(entry.grab_focus());
    xdo(&["type", "--clearmodifiers", "--delay", "0", "example/sample"]);
    press(&button(&view, "Inspect"));
    wait_for(
        || has_label(view.upcast_ref(), "Metadata preview ready"),
        Duration::from_secs(5),
    );
    let installs: Vec<_> = widgets(view.upcast_ref())
        .into_iter()
        .filter_map(|widget| widget.downcast::<gtk::Button>().ok())
        .filter(|button| button.label().as_deref() == Some("Install disabled"))
        .collect();
    assert_eq!(
        installs.len(),
        2,
        "multi-plugin catalogue must expose independent actions"
    );
    assert!(!root.join("installed.json").exists());
    assert_eq!(server.requests().len(), 3);
    press(&installs[0]);
    if close_after_commit {
        wait_for(|| server.requests().len() == 8, Duration::from_secs(5));
        release.store(true, std::sync::atomic::Ordering::Release);
        // hold the GUI loop while the worker commits, then destroy the view
        // before its polling callback has any chance to consume the outcome.
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while !root.join("installed.json").exists() {
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(ctx.state.borrow().plugins.listing().is_empty());
        view.destroy();
        wait_for(
            || ctx.state.borrow().plugins.listing().len() == 1,
            Duration::from_secs(5),
        );
    } else {
        wait_for(
            || {
                has_label(
                    view.upcast_ref(),
                    "Installed github:12/sample disabled. Enable separately in Your plugins.",
                )
            },
            Duration::from_secs(5),
        );
    }
    assert_eq!(ctx.state.borrow().plugins.listing().len(), 1);
    assert!(!ctx
        .state
        .borrow()
        .plugins
        .desired_enabled("github:12/sample"));
    assert_eq!(ctx.state.borrow().plugins.supervised_count(), 0);
    assert_eq!(
        ctx.state
            .borrow()
            .plugins
            .managed_details("github:12/sample")
            .unwrap()
            .1
            .unwrap()["values"]["label"],
        "retained value"
    );
    assert_eq!(
        std::fs::read_to_string(directory.join("managed-plugins.json")).unwrap(),
        retained
    );
    assert_eq!(crate::plugin::inventory::read(&root).unwrap().len(), 1);
    assert_eq!(server.requests().len(), 11);
    assert!(!server
        .requests()
        .iter()
        .any(|request| request.contains("/assets/22")));
    if !close_after_commit {
        screenshot(&view, "installed-disabled");
        view.destroy();
    }
    focus(&preferences);
    wait_for(
        || {
            widgets(page.upcast_ref())
                .iter()
                .any(|widget| widget.is::<gtk::Switch>())
        },
        Duration::from_secs(2),
    );
    screenshot(&preferences, "hot-manager-disabled");
    preferences.destroy();
    focus(&ctx.window);
    apply_focus(&ctx);
    wait_for(|| terminal.has_focus(), Duration::from_secs(2));
    xdo(&[
        "type",
        "--clearmodifiers",
        "--delay",
        "0",
        "printf installer-alive > \"$HOME/installer-input-proof\"",
    ]);
    xdo(&["key", "--clearmodifiers", "Return"]);
    let proof =
        std::path::PathBuf::from(std::env::var_os("HOME").unwrap()).join("installer-input-proof");
    wait_for(|| proof.exists(), Duration::from_secs(2));
    assert_eq!(std::fs::read_to_string(proof).unwrap(), "installer-alive");
    assert_eq!(terminal.pty().unwrap().fd().as_raw_fd(), initial_pty);
    ctx.window.destroy();
    println!("PASS actual HTTPS selected one of two catalogue plugins, verified/extracted/committed immutable files, hot native inventory disabled despite retained enabled=true, unchanged settings and terminal PTY, real input after closing its views. Input with the views open is exercised separately.");
}

fn widgets(root: &gtk::Widget) -> Vec<gtk::Widget> {
    let mut result = vec![root.clone()];
    let mut child = root.first_child();
    while let Some(widget) = child {
        result.extend(widgets(&widget));
        child = widget.next_sibling();
    }
    result
}

fn focus(window: &impl IsA<gtk::Window>) -> String {
    wait_for(|| window.as_ref().is_mapped(), Duration::from_secs(2));
    let name = format!("^{}$", window.as_ref().title().unwrap());
    let ids = xdo(&[
        "search",
        "--all",
        "--onlyvisible",
        "--pid",
        &std::process::id().to_string(),
        "--name",
        &name,
    ]);
    assert_eq!(
        ids.lines().count(),
        1,
        "PID and exact title must select one owned visible window"
    );
    let id = ids.lines().last().unwrap();
    xdo(&["windowactivate", "--sync", id]);
    xdo(&["windowfocus", "--sync", id]);
    eprintln!(
        "repository focus target={} id={id} mapped={} active={}",
        window.as_ref().title().unwrap(),
        window.as_ref().is_mapped(),
        window.as_ref().is_active()
    );
    wait_for(|| window.as_ref().is_active(), Duration::from_secs(2));
    id.to_string()
}

fn button(root: &impl IsA<gtk::Widget>, label: &str) -> gtk::Button {
    widgets(root.as_ref())
        .into_iter()
        .filter_map(|widget| widget.downcast::<gtk::Button>().ok())
        .find(|button| button.label().as_deref() == Some(label))
        .unwrap()
}

fn press(button: &gtk::Button) {
    wait_for(
        || button.is_mapped() && button.is_sensitive(),
        Duration::from_secs(3),
    );
    assert!(button.grab_focus());
    wait_for(|| button.has_focus(), Duration::from_secs(2));
    xdo(&["key", "--clearmodifiers", "Return"]);
}

fn inspector() -> adw::PreferencesWindow {
    wait_for(
        || {
            gtk::Window::list_toplevels().iter().any(|window| {
                window
                    .downcast_ref::<adw::PreferencesWindow>()
                    .is_some_and(|window| {
                        window.title().as_deref() == Some("Inspect public GitHub repository")
                    })
            })
        },
        Duration::from_secs(2),
    );
    gtk::Window::list_toplevels()
        .into_iter()
        .filter_map(|window| window.downcast::<adw::PreferencesWindow>().ok())
        .find(|window| window.title().as_deref() == Some("Inspect public GitHub repository"))
        .unwrap()
}

fn screenshot_dialog(dialog: &adw::Dialog, name: &str) {
    wait_for(
        || dialog.is_mapped() && dialog.width() > 0,
        Duration::from_secs(2),
    );
    let settled = std::time::Instant::now();
    wait_for(
        || settled.elapsed() >= Duration::from_millis(350),
        Duration::from_secs(2),
    );
    let path = crate::test_isolation::check_root().join(format!("{name}.png"));
    assert!(std::process::Command::new("import")
        .args(["-window", "root"])
        .arg(&path)
        .status()
        .unwrap()
        .success());
    println!("Native dialog scene screenshot: {}", path.display());
}

fn screenshot(window: &impl IsA<gtk::Window>, name: &str) {
    let id = focus(window);
    let path = crate::test_isolation::check_root().join(format!("{name}.png"));
    assert!(std::process::Command::new("import")
        .args(["-screen", "-window", &id])
        .arg(&path)
        .status()
        .unwrap()
        .success());
    println!("Screenshot: {}", path.display());
}

fn contained(
    widget: &impl IsA<gtk::Widget>,
    window: &impl IsA<gtk::Window>,
) -> gtk::graphene::Rect {
    let bounds = widget.as_ref().compute_bounds(window.as_ref()).unwrap();
    assert!(bounds.x() >= 0.0 && bounds.y() >= 0.0);
    assert!(bounds.x() + bounds.width() <= window.as_ref().width() as f32);
    assert!(bounds.y() + bounds.height() <= window.as_ref().height() as f32);
    assert!(bounds.width() >= 24.0 && bounds.height() >= 24.0);
    bounds
}

#[test]
#[ignore = "requires an isolated display, xdotool and ImageMagick"]
fn repository_and_plugin_windows_have_clear_native_layout_and_keyboard_actions() {
    let root = crate::plugin::inventory::root().unwrap();
    crate::plugin::inventory::tests::fixture(&root);
    let config = crate::settings::directory()
        .unwrap()
        .join("managed-plugins.json");
    assert!(!config.exists());
    let app = application("com.github.among-terminal.plugin-layout-test");
    let ctx = build_with_writer(&app, None, crate::session::Writer::new).unwrap();
    wait_for(|| ctx.state.borrow().session_ready, Duration::from_secs(5));
    open_preferences(&ctx);
    let preferences = gtk::Window::list_toplevels()
        .into_iter()
        .filter_map(|window| window.downcast::<adw::PreferencesWindow>().ok())
        .find(|window| window.title().as_deref() == Some("Preferences"))
        .unwrap();
    let page = widgets(preferences.upcast_ref())
        .into_iter()
        .filter_map(|widget| widget.downcast::<adw::PreferencesPage>().ok())
        .find(|page| page.title() == "Plugins")
        .unwrap();
    preferences.set_visible_page(&page);
    screenshot(&preferences, "plugins");
    let search = widgets(page.upcast_ref())
        .into_iter()
        .find_map(|widget| widget.downcast::<gtk::SearchEntry>().ok())
        .unwrap();
    let sources = button(&preferences, "Inspect repository…");
    let source_bounds = contained(&sources, &preferences);
    let search_bounds = contained(&search, &preferences);
    assert!(
        source_bounds.y() + source_bounds.height() < search_bounds.y(),
        "repository action belongs to its own source section, not under plugin search"
    );
    let details = widgets(page.upcast_ref())
        .into_iter()
        .filter_map(|widget| widget.downcast::<gtk::Button>().ok())
        .find(|button| button.label().as_deref() == Some("Details…"))
        .unwrap();
    contained(&details, &preferences);
    press(&details);
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
    screenshot(&details, "plugin-details");
    xdo(&["key", "--clearmodifiers", "Escape"]);
    wait_for(|| !details.is_visible(), Duration::from_secs(2));
    focus(&preferences);
    press(&button(&preferences, "Settings…"));
    wait_for(
        || {
            gtk::Window::list_toplevels().iter().any(|window| {
                window.downcast_ref::<gtk::Window>().is_some_and(|window| {
                    window.title().as_deref() == Some("Plugin settings") && window.is_mapped()
                })
            })
        },
        Duration::from_secs(2),
    );
    let editor = gtk::Window::list_toplevels()
        .into_iter()
        .find_map(|window| {
            window
                .downcast::<adw::PreferencesWindow>()
                .ok()
                .filter(|window| window.title().as_deref() == Some("Plugin settings"))
        })
        .unwrap();
    screenshot(&editor, "plugin-settings");
    let field = widgets(editor.upcast_ref())
        .into_iter()
        .find_map(|widget| widget.downcast::<adw::EntryRow>().ok())
        .unwrap();
    let save = button(&editor, "Save");
    assert!(contained(&save, &editor).y() < contained(&field, &editor).y());
    assert!(!config.exists(), "inspection must not save defaults");
    assert!(field.grab_focus());
    xdo(&["key", "--clearmodifiers", "ctrl+a"]);
    xdo(&["type", "--clearmodifiers", "--delay", "0", "layout edit"]);
    wait_for(|| field.text() == "layout edit", Duration::from_secs(2));
    press(&save);
    wait_for(
        || has_label(editor.upcast_ref(), "Saved locally"),
        Duration::from_secs(2),
    );
    let saved: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&config).unwrap()).unwrap();
    assert_eq!(
        saved["plugins"]["github:12/sample"]["settings"]["values"]["label"],
        "layout edit"
    );
    let saved = std::fs::read(&config).unwrap();
    xdo(&["key", "--clearmodifiers", "alt+F4"]);
    wait_for(|| !editor.is_visible(), Duration::from_secs(2));
    focus(&preferences);
    press(&button(&preferences, "Inspect repository…"));
    let view = inspector();
    screenshot(&view, "repository-empty");
    assert!(
        !preferences.is_modal() && !view.is_modal(),
        "Preferences and inspection must leave the terminal available"
    );
    assert!(!button(&view, "Inspect").is_sensitive());
    assert!(!button(&view, "Cancel request").is_visible());
    let output = widgets(view.upcast_ref())
        .into_iter()
        .find_map(|widget| widget.downcast::<gtk::TextView>().ok())
        .unwrap();
    assert!(
        !output.is_mapped(),
        "idle state must not contain a blank result panel"
    );
    xdo(&["key", "--clearmodifiers", "Escape"]);
    wait_for(|| !view.is_visible(), Duration::from_secs(2));
    let held = Rc::new(Server::new(|_| Reply::held()));
    let ready = Rc::new(Server::new(normal));
    let requests = Rc::new(Cell::new(0));
    crate::prefs::repositories::present_for_test(
        preferences.upcast_ref(),
        Rc::new({
            let held = held.clone();
            let ready = ready.clone();
            let requests = requests.clone();
            move |repository| {
                let count = requests.get();
                requests.set(count + 1);
                if count == 0 {
                    held.job(repository)
                } else {
                    ready.job(repository)
                }
            }
        }),
    );
    let view = inspector();
    focus(&view);
    let entry = widgets(view.upcast_ref())
        .into_iter()
        .find_map(|widget| widget.downcast::<adw::EntryRow>().ok())
        .unwrap();
    let inspect = button(&view, "Inspect");
    assert!(entry.grab_focus());
    xdo(&["type", "--clearmodifiers", "--delay", "0", "invalid"]);
    wait_for(|| entry.text() == "invalid", Duration::from_secs(2));
    xdo(&["key", "--clearmodifiers", "Return"]);
    wait_for(
        || has_label(view.upcast_ref(), "Check the repository address"),
        Duration::from_secs(2),
    );
    assert!(entry.has_css_class("error"));
    assert_eq!(requests.get(), 0);
    screenshot(&view, "repository-invalid");
    xdo(&["key", "--clearmodifiers", "ctrl+a"]);
    xdo(&["type", "--clearmodifiers", "--delay", "0", "example/sample"]);
    wait_for(|| entry.text() == "example/sample", Duration::from_secs(2));
    assert!(!entry.has_css_class("error"));
    let field_bounds = contained(&entry, &view);
    let inspect_bounds = contained(&inspect, &view);
    assert!(
        inspect_bounds.y() >= field_bounds.y()
            && inspect_bounds.y() + inspect_bounds.height()
                <= field_bounds.y() + field_bounds.height(),
        "Inspect must sit alongside the address, not below it"
    );
    xdo(&["key", "--clearmodifiers", "Tab"]);
    wait_for(|| inspect.has_focus(), Duration::from_secs(2));
    xdo(&["key", "--clearmodifiers", "Return"]);
    wait_for(|| !held.requests().is_empty(), Duration::from_secs(5));
    let cancel = button(&view, "Cancel request");
    assert!(cancel.is_mapped() && cancel.is_sensitive() && !entry.is_sensitive());
    contained(&cancel, &view);
    screenshot(&view, "repository-loading");
    press(&cancel);
    wait_for(
        || has_label(view.upcast_ref(), "repository inspection cancelled"),
        Duration::from_secs(2),
    );
    assert!(!cancel.is_visible() && entry.is_sensitive());
    screenshot(&view, "repository-cancelled");
    assert!(entry.grab_focus());
    xdo(&["key", "--clearmodifiers", "Return"]);
    wait_for(
        || has_label(view.upcast_ref(), "Metadata preview ready"),
        Duration::from_secs(6),
    );
    let output = widgets(view.upcast_ref())
        .into_iter()
        .find_map(|widget| widget.downcast::<gtk::TextView>().ok())
        .unwrap();
    assert!(output.is_mapped());
    assert!(output
        .buffer()
        .text(
            &output.buffer().start_iter(),
            &output.buffer().end_iter(),
            false
        )
        .contains("example/sample (repository 12, publisher 34)"));
    assert_eq!(requests.get(), 2);
    assert_eq!(ready.requests().len(), 3);
    screenshot(&view, "repository-result");
    let id = focus(&view);
    xdo(&["windowsize", "--sync", &id, "480", "620"]);
    wait_for(|| view.width() <= 480, Duration::from_secs(2));
    adw::StyleManager::default().set_color_scheme(adw::ColorScheme::ForceDark);
    contained(&inspect, &view);
    screenshot(&view, "repository-dark-narrow");
    assert!(entry.grab_focus());
    xdo(&["key", "--clearmodifiers", "ctrl+a"]);
    xdo(&[
        "type",
        "--clearmodifiers",
        "--delay",
        "0",
        "another/repository",
    ]);
    wait_for(|| !output.is_mapped(), Duration::from_secs(2));
    assert_eq!(
        requests.get(),
        2,
        "editing an address is inert and hides the old result"
    );
    assert_eq!(std::fs::read(&config).unwrap(), saved);
    assert_eq!(ctx.state.borrow().plugins.supervised_count(), 0);
    assert!(!root.join("executed").exists());
    view.destroy();
    preferences.destroy();
    ctx.window.destroy();
    println!("PASS native repository source/search separation, labelled details, Save above fields and actual saved values; inline Inspect through real Tab/Enter, invalid/idle/loading/cancel/result states, dark narrow layout and inert address edits; Preferences and inspection are nonmodal. Simultaneous terminal input is exercised separately.");
}

#[test]
#[ignore = "requires an isolated display, xdotool, actual terminal and controlled Rust HTTPS fixture"]
fn repository_inspection_is_inert_then_cancellable_while_terminal_input_continues() {
    let app = application("com.github.among-terminal.repository-inspection-test");
    let ctx = build_with_writer(&app, None, crate::session::Writer::new).unwrap();
    wait_for(|| ctx.state.borrow().session_ready, Duration::from_secs(5));
    ctx.window.set_title(Some("Repository fixture terminal"));
    let terminal = ctx
        .state
        .borrow()
        .pane(PaneId(1))
        .unwrap()
        .terminal()
        .clone();
    let directory = crate::settings::directory().unwrap();
    let before = ["plugins.json", "managed-plugins.json", "shortcuts.json"]
        .map(|name| std::fs::read(directory.join(name)).ok());
    assert_eq!(ctx.state.borrow().plugins.supervised_count(), 0);
    focus(&ctx.window);
    open_preferences(&ctx);
    let preferences = gtk::Window::list_toplevels()
        .into_iter()
        .filter_map(|window| window.downcast::<adw::PreferencesWindow>().ok())
        .find(|window| window.title().as_deref() == Some("Preferences"))
        .unwrap();
    let page = widgets(preferences.upcast_ref())
        .into_iter()
        .filter_map(|widget| widget.downcast::<adw::PreferencesPage>().ok())
        .find(|page| page.title() == "Plugins")
        .unwrap();
    preferences.set_visible_page(&page);
    focus(&preferences);
    press(&button(&preferences, "Inspect repository…"));
    wait_for(
        || {
            gtk::Window::list_toplevels()
                .iter()
                .filter_map(|widget| widget.downcast_ref::<gtk::Window>())
                .any(|window| window.title().as_deref() == Some("Inspect public GitHub repository"))
        },
        Duration::from_secs(2),
    );
    let first = inspector();
    assert!(has_label(first.upcast_ref(), "No request has been sent."));
    assert_eq!(ctx.state.borrow().plugins.supervised_count(), 0);
    focus(&first);
    xdo(&["key", "--clearmodifiers", "alt+F4"]);
    wait_for(|| !first.is_visible(), Duration::from_secs(2));
    drop(first);

    let held = Rc::new(Server::new(|_| Reply::held()));
    let ready = Rc::new(Server::new(normal));
    let requests = Rc::new(Cell::new(0));
    crate::prefs::repositories::present_for_test(
        preferences.upcast_ref(),
        Rc::new({
            let held = held.clone();
            let ready = ready.clone();
            let requests = requests.clone();
            move |repository| {
                let current = requests.get();
                requests.set(current + 1);
                if current == 0 {
                    held.job(repository)
                } else {
                    ready.job(repository)
                }
            }
        }),
    );
    let view = inspector();
    let entry = widgets(view.upcast_ref())
        .into_iter()
        .find_map(|widget| widget.downcast::<adw::EntryRow>().ok())
        .unwrap();
    let output = widgets(view.upcast_ref())
        .into_iter()
        .find_map(|widget| widget.downcast::<gtk::TextView>().ok())
        .unwrap();
    let text = || {
        output
            .buffer()
            .text(
                &output.buffer().start_iter(),
                &output.buffer().end_iter(),
                false,
            )
            .to_string()
    };
    focus(&view);
    wait_for(|| entry.is_mapped(), Duration::from_secs(2));
    assert!(entry.grab_focus());
    xdo(&["type", "--clearmodifiers", "--delay", "0", "example/sample"]);
    wait_for(|| entry.text() == "example/sample", Duration::from_secs(2));
    assert!(held.requests().is_empty() && ready.requests().is_empty());
    press(&button(&view, "Inspect"));
    wait_for(|| !held.requests().is_empty(), Duration::from_secs(5));
    assert!(has_label(
        view.upcast_ref(),
        "Inspecting public repository metadata…"
    ));
    assert!(!entry.is_sensitive());
    eprintln!(
        "repository modal state: preferences={} inspection={}",
        preferences.is_modal(),
        view.is_modal()
    );
    focus(&ctx.window);
    apply_focus(&ctx);
    wait_for(|| terminal.has_focus(), Duration::from_secs(2));
    xdo(&[
        "type",
        "--clearmodifiers",
        "--delay",
        "0",
        "printf inspected > \"$HOME/repository-input-proof\"",
    ]);
    xdo(&["key", "--clearmodifiers", "Return"]);
    let proof =
        std::path::PathBuf::from(std::env::var_os("HOME").unwrap()).join("repository-input-proof");
    let input_started = std::time::Instant::now();
    let diagnosed = Cell::new(false);
    wait_for(
        || {
            if input_started.elapsed() >= Duration::from_secs(1) && !diagnosed.replace(true) {
                eprintln!(
                    "repository input diagnostic: terminal={:?} inspection={:?} xfocus={}",
                    terminal.text_format(vte::Format::Text),
                    text(),
                    xdo(&["getwindowfocus"])
                );
            }
            proof.exists()
        },
        Duration::from_secs(2),
    );
    assert_eq!(std::fs::read_to_string(proof).unwrap(), "inspected");
    assert!(has_label(
        view.upcast_ref(),
        "Inspecting public repository metadata…"
    ));
    focus(&view);
    press(&button(&view, "Cancel request"));
    wait_for(
        || has_label(view.upcast_ref(), "repository inspection cancelled"),
        Duration::from_secs(2),
    );
    assert_eq!(requests.get(), 1);
    assert!(text().is_empty());
    press(&button(&view, "Inspect"));
    wait_for(
        || {
            has_label(
                view.upcast_ref(),
                "Metadata inspected. No installation, registration or activation was performed.",
            )
        },
        Duration::from_secs(6),
    );
    assert!(text().contains("example/sample (repository 12, publisher 34)"));
    assert!(text().contains("inspection does not install or enable plugins"));
    assert_eq!(ready.requests().len(), 3);
    assert!(ready.requests()[2].starts_with(&format!(
        "GET /example/sample/{}/among-plugins.json ",
        "a".repeat(40)
    )));
    assert_eq!(ctx.state.borrow().plugins.supervised_count(), 0);
    assert_eq!(
        ctx.state.borrow().pane(PaneId(1)).unwrap().terminal(),
        &terminal
    );
    let after = ["plugins.json", "managed-plugins.json", "shortcuts.json"]
        .map(|name| std::fs::read(directory.join(name)).ok());
    assert_eq!(after, before);
    focus(&view);
    xdo(&["key", "--clearmodifiers", "alt+F4"]);
    wait_for(|| !view.is_visible(), Duration::from_secs(2));
    preferences.destroy();
    ctx.window.destroy();
    println!("PASS production inspection opens inertly; the same native controls with test-only TLS roots/loopback DNS drive actual HTTPS, cancellation and revision-pinned preview while real terminal input works; no plugin starts or preference writes");
}
