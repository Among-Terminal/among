use super::*;
use crate::plugin::install::source::tests::{normal, Reply, Server};
use std::os::fd::AsRawFd;
use std::os::unix::fs::PermissionsExt;

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
    println!("PASS actual source Add, fresh-view offline cache, numeric Refresh/rename, explicit transferred publisher approval, actual rate-limit/cancellation and Remove without changing selected installation, enablement or settings. Native modality unchanged; simultaneous terminal input remains open.");
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
        wait_for(|| server.requests().len() == 6, Duration::from_secs(5));
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
    assert_eq!(server.requests().len(), 6);
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
    println!("PASS actual HTTPS selected one of two catalogue plugins, verified/extracted/committed immutable files, hot native inventory disabled despite retained enabled=true, unchanged settings and terminal PTY, real input after modal close. Simultaneous modal input remains a separate unresolved acceptance conflict.");
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

fn screenshot(window: &impl IsA<gtk::Window>, name: &str) {
    let id = focus(window);
    let path = crate::test_isolation::check_root().join(format!("{name}.png"));
    assert!(std::process::Command::new("import")
        .args(["-window", &id])
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
        preferences.is_modal() && view.is_modal(),
        "this layout change must not alter modality"
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
    println!("PASS native repository source/search separation, labelled details, Save above fields and actual saved values; inline Inspect through real Tab/Enter, invalid/idle/loading/cancel/result states, dark narrow layout and inert address edits; modality unchanged. Simultaneous terminal input is covered by a separate unresolved test.");
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
