//! source-bound real GTK resource output with synthetic local receipt metadata;
//! this is not authenticated Release installation or an AT-SPI screen-reader run.

use super::*;
use glib::translate::{from_glib_full, ToGlibPtr};
use serde_json::json;
use std::io::Read;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

const KEY: &str = "github:1431055733/system-status";
const CLIENT: &str = "276e1d29b0ec912e4a34b44a07f7c1b195d02374";
const PHASE: &str = "AMONG_RESOURCE_GUI_PHASE";
const LEGACY: &[u8] = b"{\"plugins\":[],\"future\":\"retain these legacy bytes\"}\n";

struct PhaseChild(std::process::Child);

impl Drop for PhaseChild {
    fn drop(&mut self) {
        if self.0.try_wait().ok().flatten().is_none() {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
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

fn bars(widget: &gtk::Widget) -> Vec<gtk::ProgressBar> {
    widgets(widget)
        .into_iter()
        .filter_map(|widget| widget.downcast::<gtk::ProgressBar>().ok())
        .filter(|bar| bar.has_css_class("status-meter"))
        .collect()
}

fn focus(window: &impl IsA<gtk::Window>) -> String {
    wait_for(|| window.as_ref().is_mapped(), Duration::from_secs(2));
    let ids = xdo(&[
        "search",
        "--all",
        "--onlyvisible",
        "--pid",
        &std::process::id().to_string(),
        "--name",
        &format!("^{}$", window.as_ref().title().unwrap()),
    ]);
    let id = ids.lines().last().unwrap().to_string();
    xdo(&["windowactivate", "--sync", &id]);
    xdo(&["windowfocus", "--sync", &id]);
    wait_for(|| window.as_ref().is_active(), Duration::from_secs(2));
    id
}

fn press(button: &gtk::Button) {
    wait_for(|| button.is_mapped(), Duration::from_secs(2));
    assert!(button.grab_focus());
    xdo(&["key", "--clearmodifiers", "space"]);
}

fn prepare_source_bound_fixture(root: &Path) {
    let binary = PathBuf::from(std::env::var_os("AMONG_MANAGED_SYSTEM_BINARY").unwrap());
    let metadata = std::fs::symlink_metadata(&binary).unwrap();
    assert!(binary.is_absolute() && metadata.is_file() && metadata.mode() & 0o111 != 0);
    let mut bytes = Vec::new();
    std::fs::File::open(binary)
        .unwrap()
        .take(32 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)
        .unwrap();
    assert!(bytes.len() <= 32 * 1024 * 1024);
    let digest = crate::plugin::inventory::checksum(&bytes).unwrap();
    assert_eq!(
        digest,
        std::env::var("AMONG_MANAGED_SYSTEM_SHA256").unwrap()
    );
    let manifest = json!({"schema_version":1,"id":"system-status","version":"source-bound-test",
        "name":"System status source-bound UI fixture","description":"Real executable, synthetic local package metadata; not authenticated installation",
        "entrypoint":"plugin","api":{"major":1,"minor":2},"required_features":["status"]})
        .to_string();
    let receipt = crate::plugin::inventory::Receipt {
        repository_id: 1431055733,
        owner: "Among-Terminal".into(),
        repository: "among-plugins".into(),
        plugin_id: "system-status".into(),
        revision: CLIENT.into(),
        manifest_sha256: crate::plugin::inventory::checksum(manifest.as_bytes()).unwrap(),
        package_sha256: digest.clone(),
        release_id: 1,
        asset_id: 2,
        generation: "a".repeat(64),
        installation: None,
        source: None,
    };
    let package = receipt.package_path(root);
    std::fs::create_dir_all(&package).unwrap();
    let mut path = package.as_path();
    while path.starts_with(root) {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
        if path == root {
            break;
        }
        path = path.parent().unwrap();
    }
    let executable = package.join("plugin");
    std::fs::write(&executable, &bytes).unwrap();
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
    assert_eq!(std::fs::metadata(&executable).unwrap().nlink(), 1);
    std::fs::write(package.join("among-plugin.json"), manifest).unwrap();
    std::fs::set_permissions(
        package.join("among-plugin.json"),
        std::fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    std::fs::write(
        root.join("installed.json"),
        json!({"version":1,"installations":[receipt]}).to_string(),
    )
    .unwrap();
    std::fs::set_permissions(
        root.join("installed.json"),
        std::fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    let entries = crate::plugin::inventory::read(root).unwrap();
    assert_eq!(entries.len(), 1);
    assert!(entries[0].inspected.is_ok());
    assert_eq!(entries[0].key(), KEY);
    println!("resource_gui_client={CLIENT} binary_sha256={digest} synthetic_receipt=true");
}

fn settle_session(ctx: &Window) {
    wait_for(
        || ctx.state.borrow().session_ready || ctx.window.visible_dialog().is_some(),
        Duration::from_secs(5),
    );
    if !ctx.state.borrow().session_ready {
        let dialog = ctx.window.visible_dialog().unwrap();
        let fresh = widgets(dialog.upcast_ref())
            .into_iter()
            .filter_map(|widget| widget.downcast::<gtk::Button>().ok())
            .find(|button| button.label().as_deref() == Some("Start new"))
            .unwrap();
        focus(&ctx.window);
        press(&fresh);
        wait_for(|| ctx.state.borrow().session_ready, Duration::from_secs(5));
    }
}

fn input_proof(ctx: &Window, terminal: &vte::Terminal, pty: &vte::Pty, name: &str) {
    assert_eq!(
        ctx.state.borrow().pane(PaneId(1)).unwrap().terminal(),
        terminal
    );
    assert_eq!(terminal.pty().as_ref(), Some(pty));
    focus(&ctx.window);
    apply_focus(ctx);
    wait_for(|| terminal.has_focus(), Duration::from_secs(2));
    let home = PathBuf::from(std::env::var_os("HOME").unwrap());
    crate::test_isolation::assert_below_check_root(&home);
    let path = home.join(name);
    assert!(!path.exists());
    xdo(&[
        "type",
        "--clearmodifiers",
        "--delay",
        "0",
        &format!("printf '{name}' > \"$HOME/{name}\""),
    ]);
    xdo(&["key", "--clearmodifiers", "Return"]);
    wait_for(
        || std::fs::read_to_string(&path).ok().as_deref() == Some(name),
        Duration::from_secs(3),
    );
    assert_eq!(terminal.pty().as_ref(), Some(pty));
    println!("resource_gui_real_input={name} same_terminal=true same_pty_object=true explicit_focus=true");
}

fn screenshot(window: &impl IsA<gtk::Window>, state: &str) {
    let directory = PathBuf::from(std::env::var_os("AMONG_RESOURCE_GUI_SCREENSHOTS").unwrap());
    crate::test_isolation::assert_below_check_root(&directory);
    assert!(directory.is_dir());
    let scheme = std::env::var("AMONG_COLOR_SCHEME").unwrap();
    assert!(matches!(scheme.as_str(), "light" | "dark"));
    let path = directory.join(format!("resource-{scheme}-{state}.png"));
    assert!(!path.exists());
    let id = focus(window);
    let output = std::process::Command::new("/usr/bin/import")
        .args(["-window", &id, path.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let bytes = std::fs::read(&path).unwrap();
    assert!(bytes.len() > 8 && bytes.starts_with(b"\x89PNG\r\n\x1a\n"));
    println!("resource_gui_screenshot={}", path.display());
}

fn visible_bounds(widget: &gtk::Widget, ancestor: &gtk::Widget) -> gtk::graphene::Rect {
    assert!(widget.is_mapped() && widget.width() > 0 && widget.height() > 0);
    assert!(ancestor.is_mapped() && ancestor.width() > 0 && ancestor.height() > 0);
    let bounds = widget.compute_bounds(ancestor).unwrap();
    let viewport =
        gtk::graphene::Rect::new(0.0, 0.0, ancestor.width() as f32, ancestor.height() as f32);
    assert!(bounds.width() > 0.0 && bounds.height() > 0.0 && viewport.contains_rect(&bounds));
    bounds
}

fn saved_managed(path: &Path, enabled: bool) -> (Vec<u8>, serde_json::Value) {
    let bytes = std::fs::read(path).unwrap();
    let document = crate::settings::load(path, crate::settings::Document::ManagedPlugins).unwrap();
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&bytes).unwrap(),
        document
    );
    assert_eq!(document["version"], 1);
    assert!(document["plugins"][KEY].is_object());
    assert_eq!(document["plugins"][KEY]["enabled"], enabled);
    assert_eq!(
        document["plugins"][KEY]["review"]["owner"],
        "Among-Terminal"
    );
    assert_eq!(
        document["plugins"][KEY]["review"]["repository"],
        "among-plugins"
    );
    assert_eq!(
        document["plugins"][KEY]["review"]["features"],
        json!(["status"])
    );
    (bytes, document)
}

fn disabled_record() -> PathBuf {
    let directory = PathBuf::from(std::env::var_os("AMONG_RESOURCE_GUI_SCREENSHOTS").unwrap());
    crate::test_isolation::assert_below_check_root(&directory);
    directory.join("post-disable-managed.json")
}

fn snapshot_meters(header: &gtk::Widget, window: &gtk::Widget) {
    visible_bounds(header, window);
    let meters = bars(header);
    assert_eq!(meters.len(), 3);
    for (bar, label) in meters.iter().zip(["CPU", "MEM", "DISK"]) {
        let bar_bounds = visible_bounds(bar.upcast_ref(), header);
        visible_bounds(bar.upcast_ref(), window);
        assert!(!bar.is_focusable());
        let parent = bar.parent().unwrap();
        let label_widget = widgets(&parent)
            .into_iter()
            .filter_map(|widget| widget.downcast::<gtk::Label>().ok())
            .find(|text| text.text().starts_with(&format!("{label} ")))
            .unwrap();
        let label_bounds = visible_bounds(label_widget.upcast_ref(), header);
        visible_bounds(label_widget.upcast_ref(), window);
        let text = label_widget.text();
        let value = text
            .strip_prefix(&format!("{label} "))
            .unwrap()
            .strip_suffix('%')
            .unwrap()
            .parse::<u8>()
            .unwrap();
        assert!(value <= 100);
        assert_eq!(bar.fraction(), f64::from(value) / 100.0);
        assert!(gtk::test_accessible_has_role(
            bar,
            gtk::AccessibleRole::ProgressBar
        ));
        assert!(gtk::test_accessible_has_property(
            bar,
            gtk::AccessibleProperty::Label
        ));
        let expected = std::ffi::CString::new(text.as_str()).unwrap();
        // GTK's test accessibility context inspects the actual rendered widget's
        // stored label, rather than inferring its accessible name from a sibling.
        let mismatch = unsafe {
            gtk::ffi::gtk_test_accessible_check_property(
                bar.upcast_ref::<gtk::Accessible>().to_glib_none().0,
                gtk::ffi::GTK_ACCESSIBLE_PROPERTY_LABEL,
                expected.as_ptr(),
            )
        };
        if !mismatch.is_null() {
            let message: glib::GString = unsafe { from_glib_full(mismatch) };
            panic!("resource meter accessibility label mismatch: {message}");
        }
        println!(
            "resource_gui_meter text={text:?} fraction={} allocation={}x{} role=ProgressBar accessible_label_exact=true focusable=false bar_header_bounds=[{},{},{},{}] label_header_bounds=[{},{},{},{}] within_window=true",
            bar.fraction(), bar.width(), bar.height(), bar_bounds.x(), bar_bounds.y(), bar_bounds.width(), bar_bounds.height(), label_bounds.x(), label_bounds.y(), label_bounds.width(), label_bounds.height()
        );
    }
}

#[test]
#[ignore = "requires exact companion binary, owned native display/keyboard and GTK test accessibility"]
fn real_resource_meters_review_keyboard_input_and_disabled_restart() {
    if std::env::var_os(PHASE).is_none() {
        crate::test_isolation::assert_below_check_root(&PathBuf::from(
            std::env::var_os("HOME").unwrap(),
        ));
        let directory = PathBuf::from(std::env::var_os("AMONG_RESOURCE_GUI_SCREENSHOTS").unwrap());
        crate::test_isolation::assert_below_check_root(&directory);
        let scheme = std::env::var("AMONG_COLOR_SCHEME").unwrap();
        assert!(matches!(scheme.as_str(), "light" | "dark"));
        for phase in ["keyboard-and-output", "fresh-disabled"] {
            let stdout = directory.join(format!("resource-{scheme}-{phase}.stdout.log"));
            let stderr = directory.join(format!("resource-{scheme}-{phase}.stderr.log"));
            let create = |path: &Path| {
                std::fs::OpenOptions::new()
                    .create_new(true)
                    .write(true)
                    .open(path)
                    .unwrap()
            };
            let mut child = PhaseChild(std::process::Command::new(std::env::current_exe().unwrap())
                .args(["app::plugin_tests::resources::real_resource_meters_review_keyboard_input_and_disabled_restart", "--exact", "--ignored", "--test-threads=1", "--nocapture"])
                .env(PHASE, phase)
                .stdin(std::process::Stdio::null())
                .stdout(create(&stdout))
                .stderr(create(&stderr))
                .spawn()
                .unwrap());
            let deadline = std::time::Instant::now() + Duration::from_secs(90);
            let status = loop {
                let oversized = [&stdout, &stderr]
                    .iter()
                    .any(|path| std::fs::metadata(path).unwrap().len() > 4 * 1024 * 1024);
                if oversized || std::time::Instant::now() >= deadline {
                    let _ = child.0.kill();
                    let _ = child.0.wait();
                    panic!("resource GUI phase {phase} exceeded time/output budget; original logs retained at {stdout:?} and {stderr:?}");
                }
                if let Some(status) = child.0.try_wait().unwrap() {
                    break status;
                }
                std::thread::sleep(Duration::from_millis(20));
            };
            assert!(
                [&stdout, &stderr]
                    .iter()
                    .all(|path| std::fs::metadata(path).unwrap().len() <= 4 * 1024 * 1024),
                "resource GUI phase output budget exceeded; logs retained"
            );
            let output = std::fs::read(&stdout).unwrap();
            let errors = std::fs::read(&stderr).unwrap();
            // diagnostics survive a failed child assertion or external timeout;
            // successful child output is still checked and never fabricated.
            print!("{}", String::from_utf8_lossy(&output));
            eprint!("{}", String::from_utf8_lossy(&errors));
            assert!(
                status.success(),
                "resource GUI phase {phase} exited {status}; original phase logs retained"
            );
            assert!(
                String::from_utf8_lossy(&output).contains(if phase == "fresh-disabled" {
                    "PASS resource_gui fresh_disabled=true"
                } else {
                    "PASS resource_gui real_keyboard_review_cancel_accept_disable=true"
                })
            );
        }
        return;
    }
    assert_eq!(std::env::var("GTK_A11Y").as_deref(), Ok("test"));
    crate::test_isolation::assert_below_check_root(&PathBuf::from(
        std::env::var_os("HOME").unwrap(),
    ));
    let phase = std::env::var(PHASE).unwrap();
    assert!(matches!(
        phase.as_str(),
        "keyboard-and-output" | "fresh-disabled"
    ));
    let root = crate::plugin::inventory::root().unwrap();
    crate::test_isolation::assert_below_check_root(&root);
    let directory = crate::settings::directory().unwrap();
    crate::test_isolation::assert_below_check_root(&directory);
    let config = directory.join("managed-plugins.json");
    if phase == "keyboard-and-output" {
        prepare_source_bound_fixture(&root);
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(directory.join("plugins.json"), LEGACY).unwrap();
    }
    let before = std::fs::read(&config).ok();
    let expected_disabled = if phase == "fresh-disabled" {
        let (bytes, _) = saved_managed(&config, false);
        let retained = std::fs::read(disabled_record()).unwrap();
        assert_eq!(bytes, retained);
        let entries = crate::plugin::inventory::read(&root).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].key(), KEY);
        assert_eq!(entries[0].receipt.revision, CLIENT);
        assert!(entries[0].inspected.is_ok());
        Some(retained)
    } else {
        None
    };
    let app = application("com.github.among-terminal.managed-resource-ui-test");
    let ctx = build_with_writer(&app, None, crate::session::Writer::new).unwrap();
    settle_session(&ctx);
    assert_eq!(ctx.state.borrow().plugins.supervised_count(), 0);
    assert!(bars(ctx.app_header.upcast_ref()).is_empty());
    if phase == "fresh-disabled" {
        let listing = ctx.state.borrow().plugins.listing();
        let entries: Vec<_> = listing.iter().filter(|(key, _)| key == KEY).collect();
        assert_eq!(entries.len(), 1);
        assert!(matches!(&entries[0].1, PluginState::Disabled));
        assert!(!ctx.state.borrow().plugins.desired_enabled(KEY));
        let (bytes, _) = saved_managed(&config, false);
        assert_eq!(Some(bytes), expected_disabled);
        assert_eq!(
            std::fs::read(directory.join("plugins.json")).unwrap(),
            LEGACY
        );
        screenshot(&ctx.window, "fresh-disabled");
        println!("PASS resource_gui fresh_disabled=true no_spawn=true no_bars=true saved_bytes_unchanged=true");
        ctx.window.destroy();
        return;
    }
    let terminal = ctx
        .state
        .borrow()
        .pane(PaneId(1))
        .unwrap()
        .terminal()
        .clone();
    let pty = terminal.pty().unwrap();
    input_proof(&ctx, &terminal, &pty, "resource-input-before-review");
    open_preferences(&ctx);
    let preferences = gtk::Window::list_toplevels()
        .into_iter()
        .find_map(|window| window.downcast::<adw::PreferencesWindow>().ok())
        .unwrap();
    preferences.set_title(Some("Resource Preferences"));
    let row = widgets(preferences.upcast_ref())
        .into_iter()
        .filter_map(|widget| widget.downcast::<adw::ActionRow>().ok())
        .find(|row| row.title() == "System status source-bound UI fixture")
        .unwrap();
    let page = row
        .ancestor(adw::PreferencesPage::static_type())
        .unwrap()
        .downcast::<adw::PreferencesPage>()
        .unwrap();
    preferences.set_visible_page(&page);
    let toggle = widgets(row.upcast_ref())
        .into_iter()
        .find_map(|widget| widget.downcast::<gtk::Switch>().ok())
        .unwrap();
    focus(&preferences);
    wait_for(|| toggle.is_mapped(), Duration::from_secs(2));
    assert!(toggle.grab_focus());
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
    assert_eq!(ctx.state.borrow().plugins.supervised_count(), 0);
    assert_eq!(std::fs::read(&config).ok(), before);
    xdo(&["key", "--clearmodifiers", "Escape"]);
    wait_for(
        || preferences.visible_dialog().is_none() && !toggle.is_active(),
        Duration::from_secs(2),
    );
    assert_eq!(ctx.state.borrow().plugins.supervised_count(), 0);
    assert_eq!(std::fs::read(&config).ok(), before);
    assert!(toggle.grab_focus());
    xdo(&["key", "--clearmodifiers", "space"]);
    wait_for(
        || preferences.visible_dialog().is_some(),
        Duration::from_secs(2),
    );
    let dialog = preferences.visible_dialog().unwrap();
    let accept = widgets(dialog.upcast_ref())
        .into_iter()
        .filter_map(|widget| widget.downcast::<gtk::Button>().ok())
        .find(|button| button.label().as_deref() == Some("Accept and enable"))
        .unwrap();
    press(&accept);
    wait_for(
        || bars(ctx.app_header.upcast_ref()).len() == 3,
        Duration::from_secs(8),
    );
    assert_eq!(ctx.state.borrow().plugins.supervised_count(), 1);
    let (_, accepted) = saved_managed(&config, true);
    input_proof(&ctx, &terminal, &pty, "resource-input-while-running");
    wait_for(
        || {
            let meters = bars(ctx.app_header.upcast_ref());
            meters.len() == 3
                && meters
                    .iter()
                    .all(|bar| bar.is_mapped() && bar.width() > 0 && bar.height() > 0)
        },
        Duration::from_secs(2),
    );
    snapshot_meters(ctx.app_header.upcast_ref(), ctx.window.upcast_ref());
    screenshot(&ctx.window, "enabled");
    focus(&preferences);
    assert!(toggle.grab_focus());
    xdo(&["key", "--clearmodifiers", "space"]);
    wait_for(
        || {
            !toggle.is_active()
                && bars(ctx.app_header.upcast_ref()).is_empty()
                && ctx.state.borrow().plugins.supervised_count() == 0
        },
        Duration::from_secs(5),
    );
    assert!(ctx.state.borrow().store.is_empty(Scope::AppHeader));
    assert_eq!(terminal.pty().as_ref(), Some(&pty));
    let (disabled, retained) = saved_managed(&config, false);
    assert_eq!(
        retained["plugins"][KEY]["review"],
        accepted["plugins"][KEY]["review"]
    );
    let mut record = std::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(disabled_record())
        .unwrap();
    std::io::Write::write_all(&mut record, &disabled).unwrap();
    record.sync_all().unwrap();
    input_proof(&ctx, &terminal, &pty, "resource-input-after-disable");
    screenshot(&ctx.window, "disabled");
    assert_eq!(
        std::fs::read(directory.join("plugins.json")).unwrap(),
        LEGACY
    );
    preferences.destroy();
    ctx.window.destroy();
    println!("PASS resource_gui real_keyboard_review_cancel_accept_disable=true three_actual_meters=true same_terminal_pty=true legacy_bytes_unchanged=true synthetic_receipt=true");
}
