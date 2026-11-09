//! The Plugins page built from saved sources and installed plugins, in a real
//! window and offline: no client, runtime or server exists in these tests, so
//! anything shown comes from files and any request would be a failure.

use super::*;
use crate::plugin::install::listing::tests::{digest, inspection};
use crate::plugin::install::registry::{self, Request};
use crate::plugin::origin::Origin;
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};

const SAMPLE: &str = "github:12/sample";
const REMOVED: &str = "github:13/sample";

fn origin(repository_id: u64, publisher_id: u64, owner: &str, repository: &str) -> Origin {
    Origin {
        repository_id,
        publisher_id,
        owner: owner.into(),
        repository: repository.into(),
    }
}

/// Saves one source snapshot the way an explicit refresh would have.
fn save_source(root: &std::path::Path, source: crate::plugin::install::source::Inspection) {
    let job = registry::Job::start(root.to_path_buf(), Request::Add(Box::new(source))).unwrap();
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
}

/// Two installed plugins named Sample: one from a registered source that now
/// offers a newer version, one from a repository that is not registered.
fn install_fixtures(root: &std::path::Path) {
    let first = crate::plugin::inventory::tests::fixture_version(root, "1.0.0");
    let mut second = first.clone();
    let release = crate::plugin::inventory::tests::release_of(&mut second);
    release.repository_id = 13;
    release.owner = "another".into();
    let package = second.package_path(root);
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&package)
        .unwrap();
    for name in ["among-plugin.json", "plugin"] {
        std::fs::copy(first.package_path(root).join(name), package.join(name)).unwrap();
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
}

fn preferences_window(ctx: &Window) -> (adw::PreferencesWindow, adw::PreferencesPage) {
    open_preferences(ctx);
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
    let id = focus(&preferences);
    xdo(&["windowsize", "--sync", &id, "800", "1000"]);
    wait_for(|| preferences.height() >= 900, Duration::from_secs(3));
    (preferences, page)
}

/// Lets the main loop run so timers and idle callbacks get their turn.
fn pump_for(duration: Duration) {
    let end = std::time::Instant::now() + duration;
    while std::time::Instant::now() < end {
        glib::MainContext::default().iteration(false);
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn row_by_identity(root: &impl IsA<gtk::Widget>, identity: &str) -> adw::ActionRow {
    widgets(root.as_ref())
        .into_iter()
        .filter_map(|widget| widget.downcast::<adw::ActionRow>().ok())
        .find(|row| row.widget_name() == identity)
        .unwrap_or_else(|| panic!("no row for {identity}"))
}

fn shown_buttons(row: &adw::ActionRow) -> Vec<String> {
    widgets(row.upcast_ref())
        .into_iter()
        .filter_map(|widget| widget.downcast::<gtk::Button>().ok())
        .filter(|button| button.is_mapped())
        .filter_map(|button| button.label().map(|label| label.to_string()))
        .collect()
}

#[test]
#[ignore = "requires an isolated display, xdotool and ImageMagick"]
fn plugin_list_is_built_offline_from_saved_sources_with_one_state_control_per_row() {
    let root = crate::plugin::inventory::root().unwrap();
    crate::test_isolation::assert_below_check_root(&root);
    install_fixtures(&root);
    let sources = registry::root().unwrap();
    // the official source, a source that offers the installed Sample again in
    // a newer version, and a user source whose Notes shares nothing installed.
    save_source(
        &sources,
        inspection(
            origin(
                1_431_055_733,
                339_567_100,
                "Among-Terminal",
                "among-plugins",
            ),
            &[("system-status", "System status", "0.2.0", &digest('b'))],
        ),
    );
    save_source(
        &sources,
        inspection(
            origin(12, 34, "example", "sample"),
            &[("sample", "Sample", "1.1.0", &digest('b'))],
        ),
    );
    save_source(
        &sources,
        inspection(
            origin(77, 34, "acme", "plugins"),
            &[("notes", "Notes", "0.4.0", &digest('b'))],
        ),
    );
    let app = application("com.github.among-terminal.plugin-list-offline-test");
    let ctx = build_with_writer(&app, None, crate::session::Writer::new).unwrap();
    wait_for(|| ctx.state.borrow().session_ready, Duration::from_secs(5));
    let (preferences, page) = preferences_window(&ctx);
    wait_for(
        || has_list_row(&preferences, "notes"),
        Duration::from_secs(5),
    );

    // Installed: the first Sample has an update waiting, the second names its
    // missing source; each row shows exactly one of Install, Update, Uninstall.
    let updated = row_by_identity(&preferences, SAMPLE);
    assert_eq!(shown_buttons(&updated), ["Update"]);
    let removed = row_by_identity(&preferences, REMOVED);
    assert_eq!(shown_buttons(&removed), ["Uninstall"]);
    assert!(has_label(removed.upcast_ref(), "source removed"));
    // Available: official plugins are there without any request, and a user
    // source's plugin names its repository.
    let status = row_by_identity(&preferences, "github:1431055733/system-status");
    assert_eq!(shown_buttons(&status), ["Install"]);
    assert!(
        !has_label(status.upcast_ref(), "Among-Terminal/among-plugins"),
        "the official source needs no label: its shield says so"
    );
    let notes = row_by_identity(&preferences, "github:77/notes");
    assert_eq!(shown_buttons(&notes), ["Install"]);
    assert!(has_label(notes.upcast_ref(), "acme/plugins"));
    // The shields tell the three origins apart in words, not colour alone.
    let tooltip = |row: &adw::ActionRow| named_shield(row).tooltip_text().unwrap().to_string();
    assert!(tooltip(&status).starts_with("Official plugin\n"));
    assert!(tooltip(&notes).starts_with("Plugin from another publisher\n"));
    assert!(tooltip(&updated).starts_with("Plugin from another publisher\n"));
    // An ordinary run has neither the channel switch nor a local catalogue.
    assert!(!banner_says(&page, "Plugin development mode"));
    assert!(widgets(page.upcast_ref())
        .iter()
        .all(|widget| !widget.is::<gtk::ToggleButton>()));
    assert!(!has_label(
        page.upcast_ref(),
        "Install from a local catalogue"
    ));
    screenshot(&preferences, "plugins-list");

    // Catalogue entries have working info pages before installation. Their
    // protection is unobserved and no settings or enable switch is offered.
    press(&named_button(&status, "plugin-info"));
    wait_for(
        || has_label(preferences.upcast_ref(), "Checked when the plugin starts."),
        Duration::from_secs(3),
    );
    assert!(has_label(preferences.upcast_ref(), "Can read"));
    assert!(button(&preferences, "Install").is_mapped());
    assert!(!widgets(preferences.upcast_ref())
        .iter()
        .any(|widget| widget.is::<adw::SwitchRow>() && widget.is_mapped()));
    screenshot(&preferences, "plugin-available-page");
    focus(&preferences);
    xdo(&["key", "--clearmodifiers", "Escape"]);
    settle();

    // The title and description area of the row opens the same page as the
    // info icon.
    adw::prelude::ActionRowExt::activate(&status);
    wait_for(
        || has_label(preferences.upcast_ref(), "Checked when the plugin starts."),
        Duration::from_secs(3),
    );
    focus(&preferences);
    xdo(&["key", "--clearmodifiers", "Escape"]);
    settle();

    // The plugin page: a subpage of the same window, what it is and what it
    // may read first, package facts folded away.
    press(&named_button(&updated, "plugin-info"));
    wait_for(
        || {
            widgets(preferences.upcast_ref())
                .iter()
                .any(|widget| widget.is::<adw::ExpanderRow>() && widget.is_mapped())
        },
        Duration::from_secs(3),
    );
    assert!(has_label(preferences.upcast_ref(), "Can read"));
    wait_for(
        || has_label(preferences.upcast_ref(), "Version 1.1.0 is available."),
        Duration::from_secs(3),
    );
    assert!(!has_label(preferences.upcast_ref(), "Manifest SHA-256"));
    screenshot(&preferences, "plugin-page");
    // Uninstall asks first and says what happens; declining changes nothing.
    focus(&preferences);
    xdo(&["key", "--clearmodifiers", "Escape"]);
    wait_for(
        || {
            !widgets(preferences.upcast_ref())
                .iter()
                .any(|widget| widget.is::<adw::ExpanderRow>() && widget.is_mapped())
        },
        Duration::from_secs(3),
    );
    press(&button(&removed, "Uninstall"));
    wait_for(
        || preferences.visible_dialog().is_some(),
        Duration::from_secs(3),
    );
    assert!(has_label(
        preferences.visible_dialog().unwrap().upcast_ref(),
        "It stops now if it is running. Your settings are kept."
    ));
    screenshot_dialog(&preferences.visible_dialog().unwrap(), "plugins-uninstall");
    press(&button(&preferences.visible_dialog().unwrap(), "Cancel"));
    wait_for(
        || preferences.visible_dialog().is_none(),
        Duration::from_secs(3),
    );
    assert_eq!(crate::plugin::inventory::read(&root).unwrap().len(), 2);
    assert_eq!(shown_buttons(&removed), ["Uninstall"]);
    preferences.destroy();
    ctx.window.destroy();
    println!("PASS Plugins page from saved snapshots offline: Update, source removed, source labels, shields, one control per row, ordinary run without development controls");
}

#[test]
#[ignore = "requires an isolated display, xdotool and ImageMagick"]
fn an_empty_source_cache_is_fetched_once_on_first_open_and_load_tries_again() {
    let root = crate::plugin::inventory::root().unwrap();
    crate::test_isolation::assert_below_check_root(&root);
    let empty = crate::test_isolation::check_root().join("empty-sources");
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&empty)
        .unwrap();
    let app = application("com.github.among-terminal.plugin-list-empty-test");
    let ctx = build_with_writer(&app, None, crate::session::Writer::new).unwrap();
    wait_for(|| ctx.state.borrow().session_ready, Duration::from_secs(5));
    let (preferences, _) = preferences_window(&ctx);
    let requests = Rc::new(Cell::new(0));
    // Only this page fetches by itself; the default Preferences page behind it
    // has another source directory and stays offline.
    let _fetch = crate::prefs::enable_first_open_fetch_for_test(empty.clone());
    let view = list_window(
        &ctx,
        &preferences,
        crate::prefs::PluginServices {
            root: Some(empty),
            refresh: Rc::new({
                let requests = requests.clone();
                move |_| {
                    requests.set(requests.get() + 1);
                    Err(crate::plugin::data::Problem("this test has no network"))
                }
            }),
            prerelease: Rc::new(|_| Err(crate::plugin::data::Problem("not a development run"))),
            install: Rc::new(|_| Err(crate::plugin::data::Problem("nothing is installed here"))),
        },
    );
    // No button is pressed: with nothing saved, the page asks for the official
    // list by itself, once, and shows why it could not get it.
    wait_for(
        || {
            has_label(
                view.upcast_ref(),
                "Could not load the list: this test has no network",
            )
        },
        Duration::from_secs(5),
    );
    pump_for(Duration::from_millis(600));
    assert_eq!(
        requests.get(),
        1,
        "the first open fetched once and did not keep retrying"
    );
    screenshot(&view, "plugins-status");
    press(&button(&view, "Load"));
    wait_for(|| requests.get() == 2, Duration::from_secs(3));
    view.destroy();
    preferences.destroy();
    ctx.window.destroy();
    println!(
        "PASS empty cache: one automatic request on first open, the failure shown, Load tries again"
    );
}

#[test]
#[ignore = "requires an isolated display, xdotool and ImageMagick"]
fn development_mode_shows_the_banner_the_channel_switch_and_the_local_catalogue() {
    assert!(crate::plugin::development::fix(true));
    let app = application("com.github.among-terminal.plugin-list-development-test");
    let ctx = build_with_writer(&app, None, crate::session::Writer::new).unwrap();
    wait_for(|| ctx.state.borrow().session_ready, Duration::from_secs(5));
    let (preferences, page) = preferences_window(&ctx);
    assert!(banner_says(&page, "This is not saved"));
    assert!(channel_active(&page, "Pre-release"));
    assert!(!channel_active(&page, "Stable"));
    assert!(has_label(
        page.upcast_ref(),
        "Install from a local catalogue"
    ));
    assert!(widgets(page.upcast_ref()).iter().all(|widget| {
        widget
            .downcast_ref::<gtk::Button>()
            .is_none_or(|button| button.label().as_deref() != Some("Use latest prerelease"))
    }));
    screenshot(&preferences, "plugins-development");
    preferences.destroy();
    ctx.window.destroy();
    println!("PASS development run: banner, Stable/Pre-release switch on Pre-release, local catalogue row, no separate prerelease action");
}

#[test]
#[ignore = "requires an isolated display, xdotool and ImageMagick"]
fn a_refused_install_is_named_in_its_row_and_leaves_install_available() {
    let root = crate::plugin::inventory::root().unwrap();
    crate::test_isolation::assert_below_check_root(&root);
    let sources = registry::root().unwrap();
    save_source(
        &sources,
        inspection(
            origin(
                1_431_055_733,
                339_567_100,
                "Among-Terminal",
                "among-plugins",
            ),
            &[("system-status", "System status", "0.2.0", &digest('b'))],
        ),
    );
    let app = application("com.github.among-terminal.plugin-list-install-refusal-test");
    let ctx = build_with_writer(&app, None, crate::session::Writer::new).unwrap();
    wait_for(|| ctx.state.borrow().session_ready, Duration::from_secs(5));
    let (preferences, _) = preferences_window(&ctx);
    let installs = Rc::new(Cell::new(0));
    let view = list_window(
        &ctx,
        &preferences,
        crate::prefs::PluginServices {
            root: Some(sources),
            refresh: Rc::new(|_| Err(crate::plugin::data::Problem("no refresh here"))),
            prerelease: Rc::new(|_| Err(crate::plugin::data::Problem("not a development run"))),
            install: Rc::new({
                let installs = installs.clone();
                move |_| {
                    installs.set(installs.get() + 1);
                    Err(crate::plugin::data::Problem(
                        "two repository operations are still active",
                    ))
                }
            }),
        },
    );
    wait_for(
        || has_list_row(&view, "system status"),
        Duration::from_secs(5),
    );
    focus(&view);
    let row = row_by_identity(&view, "github:1431055733/system-status");
    press(&button(&row, "Install"));
    wait_for(
        || {
            has_label(
                row.upcast_ref(),
                "two repository operations are still active",
            )
        },
        Duration::from_secs(3),
    );
    assert_eq!(installs.get(), 1);
    assert_eq!(shown_buttons(&row), ["Install"]);
    assert!(crate::plugin::inventory::read(&root).unwrap().is_empty());
    press(&named_button(&row, "plugin-info"));
    wait_for(
        || has_label(view.upcast_ref(), "Checked when the plugin starts."),
        Duration::from_secs(3),
    );
    press(&button(&view, "Install"));
    wait_for(
        || banner_says(&view, "two repository operations are still active"),
        Duration::from_secs(3),
    );
    assert_eq!(installs.get(), 2);
    assert!(crate::plugin::inventory::read(&root).unwrap().is_empty());
    view.destroy();
    preferences.destroy();
    ctx.window.destroy();
    println!("PASS a refused install names its reason in the row, offers Install again and installs nothing");
}

#[test]
#[ignore = "requires an isolated display, xdotool and ImageMagick"]
fn old_package_controls_refuse_a_replaced_receipt_before_the_page_refreshes() {
    fn page_switch(view: &adw::PreferencesWindow) -> adw::SwitchRow {
        let page = widgets(view.upcast_ref())
            .into_iter()
            .find(|widget| widget.widget_name() == format!("plugin-page:{SAMPLE}"))
            .unwrap();
        widgets(&page)
            .into_iter()
            .filter_map(|widget| widget.downcast::<adw::SwitchRow>().ok())
            .find(|row| row.title() == "Enabled")
            .unwrap()
    }
    let root = crate::plugin::inventory::root().unwrap();
    install_fixtures(&root);
    let app = application("com.github.among-terminal.plugin-stale-controls-test");
    let ctx = build_with_writer(&app, None, crate::session::Writer::new).unwrap();
    wait_for(|| ctx.state.borrow().session_ready, Duration::from_secs(5));
    let enable_calls = Rc::new(Cell::new(0));
    let disable_calls = Rc::new(Cell::new(0));
    let mut context = preferences_context(&ctx);
    context.enable_plugin = Rc::new({
        let actual = context.enable_plugin.clone();
        let calls = enable_calls.clone();
        move |key, parent| {
            calls.set(calls.get() + 1);
            actual(key, parent);
        }
    });
    context.disable_plugin = Rc::new({
        let actual = context.disable_plugin.clone();
        let calls = disable_calls.clone();
        move |key| {
            calls.set(calls.get() + 1);
            actual(key);
        }
    });
    let page = crate::prefs::plugins_page_for_test(
        &context,
        crate::prefs::production_plugin_services_for_test(),
    );
    let preferences = adw::PreferencesWindow::builder()
        .transient_for(&ctx.window)
        .title("Receipt controls")
        .search_enabled(false)
        .default_width(800)
        .default_height(1000)
        .build();
    preferences.add(&page);
    preferences.present();
    focus(&preferences);
    let row = row_by_identity(&preferences, "github:12/sample");
    let old_row_switch = widgets(row.upcast_ref())
        .into_iter()
        .find_map(|widget| widget.downcast::<gtk::Switch>().ok())
        .unwrap();
    assert!(!old_row_switch.is_active());
    let old_row_uninstall = button(&row, "Uninstall");
    press(&named_button(&row, "plugin-info"));
    wait_for(
        || has_label(preferences.upcast_ref(), "Technical details"),
        Duration::from_secs(3),
    );
    let old_page_uninstall = plugin_page_button(&preferences, "github:12/sample", "Uninstall");
    let old_page_switch = page_switch(&preferences);
    assert!(!old_page_switch.is_active());
    let mut entries = crate::plugin::inventory::read(&root).unwrap();
    let selected = entries
        .iter_mut()
        .find(|entry| entry.key() == "github:12/sample")
        .unwrap();
    // Reinstalling the same bytes can retain the generation value while
    // changing installation authority. The full receipt must bind controls.
    let original = selected.receipt.clone();
    selected.receipt.installation = Some("a".repeat(64));
    assert_eq!(selected.receipt.generation, original.generation);
    assert_ne!(selected.receipt.installation, original.installation);
    let receipts: Vec<_> = entries.into_iter().map(|entry| entry.receipt).collect();
    std::fs::write(
        root.join("installed.json"),
        serde_json::json!({"version":1,"installations":receipts}).to_string(),
    )
    .unwrap();
    reconcile_managed_installations(&ctx.state);
    let before = std::fs::read(root.join("installed.json")).unwrap();
    let config = crate::settings::directory()
        .unwrap()
        .join("managed-plugins.json");
    let preferences_before = std::fs::read(&config).ok();
    // Do not iterate GTK between selection and clicks: this exercises the
    // interval before either page's observation timer can rebuild controls.
    old_page_uninstall.emit_clicked();
    old_row_uninstall.emit_clicked();
    old_row_switch.set_active(true);
    old_row_switch.set_active(false);
    old_page_switch.set_active(true);
    old_page_switch.set_active(false);
    assert_eq!(
        (enable_calls.get(), disable_calls.get()),
        (0, 0),
        "old row/page switches invoked the application's current receipt callbacks"
    );
    assert!(
        preferences.visible_dialog().is_none(),
        "a stale control opened a current-generation confirmation"
    );
    assert_eq!(std::fs::read(root.join("installed.json")).unwrap(), before);
    assert_eq!(std::fs::read(&config).ok(), preferences_before);
    assert!(!ctx.state.borrow().plugins.desired_enabled(SAMPLE));

    // The next observation must replace controls even though package bytes
    // and generation did not change. A permanent stale row is unusable too.
    wait_for(
        || {
            row_by_identity(&preferences, SAMPLE) != row
                && page_switch(&preferences) != old_page_switch
        },
        Duration::from_secs(3),
    );
    old_row_uninstall.emit_clicked();
    old_page_uninstall.emit_clicked();
    old_row_switch.set_active(true);
    old_row_switch.set_active(false);
    old_page_switch.set_active(true);
    old_page_switch.set_active(false);
    assert_eq!((enable_calls.get(), disable_calls.get()), (0, 0));
    assert!(preferences.visible_dialog().is_none());
    assert_eq!(std::fs::read(&config).ok(), preferences_before);

    let current_page_switch = page_switch(&preferences);
    current_page_switch.set_active(true);
    assert_eq!((enable_calls.get(), disable_calls.get()), (1, 0));
    wait_for(
        || preferences.visible_dialog().is_some(),
        Duration::from_secs(2),
    );
    let review = preferences.visible_dialog().unwrap();
    press(&button(&review, "Cancel"));
    wait_for(
        || preferences.visible_dialog().is_none() && !review.is_mapped(),
        Duration::from_secs(2),
    );
    xdo(&["key", "--clearmodifiers", "Escape"]);
    wait_for(|| !current_page_switch.is_mapped(), Duration::from_secs(2));
    let current_row = row_by_identity(&preferences, SAMPLE);
    let current_uninstall = button(&current_row, "Uninstall");
    press(&current_uninstall);
    wait_for(
        || preferences.visible_dialog().is_some(),
        Duration::from_secs(2),
    );
    let confirmation = preferences.visible_dialog().unwrap();
    press(&button(&confirmation, "Cancel"));
    wait_for(
        || preferences.visible_dialog().is_none() && !confirmation.is_mapped(),
        Duration::from_secs(2),
    );
    assert_eq!(std::fs::read(root.join("installed.json")).unwrap(), before);
    preferences.destroy();
    ctx.window.destroy();
}

#[test]
#[ignore = "requires an isolated display, xdotool and ImageMagick"]
fn an_uninstall_write_failure_keeps_the_page_and_the_selected_package() {
    let root = crate::plugin::inventory::root().unwrap();
    install_fixtures(&root);
    let app = application("com.github.among-terminal.plugin-uninstall-failure-test");
    let ctx = build_with_writer(&app, None, crate::session::Writer::new).unwrap();
    wait_for(|| ctx.state.borrow().session_ready, Duration::from_secs(5));
    let (preferences, _) = preferences_window(&ctx);
    let row = row_by_identity(&preferences, "github:12/sample");
    press(&named_button(&row, "plugin-info"));
    settle();
    let before = std::fs::read(root.join("installed.json")).unwrap();
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o500)).unwrap();
    press(&plugin_page_button(
        &preferences,
        "github:12/sample",
        "Uninstall",
    ));
    wait_for(
        || preferences.visible_dialog().is_some(),
        Duration::from_secs(3),
    );
    press(&button(&preferences.visible_dialog().unwrap(), "Uninstall"));
    wait_for(
        || {
            widgets(preferences.upcast_ref()).iter().any(|widget| {
                widget
                    .downcast_ref::<adw::Banner>()
                    .is_some_and(|banner| banner.is_mapped() && banner.is_revealed())
            })
        },
        Duration::from_secs(5),
    );
    assert_eq!(std::fs::read(root.join("installed.json")).unwrap(), before);
    assert!(plugin_page_button(&preferences, "github:12/sample", "Uninstall").is_mapped());
    screenshot(&preferences, "plugin-uninstall-failed");
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
    preferences.destroy();
    ctx.window.destroy();
}

#[test]
#[ignore = "requires an isolated display, xdotool and ImageMagick"]
fn plugin_controls_remain_named_and_operable_in_high_contrast_without_animation() {
    std::env::set_var("GTK_A11Y", "test");
    let root = crate::plugin::inventory::root().unwrap();
    install_fixtures(&root);
    let app = application("com.github.among-terminal.plugin-accessibility-test");
    let ctx = build_with_writer(&app, None, crate::session::Writer::new).unwrap();
    let contrast = gio::Settings::new("org.gnome.desktop.a11y.interface");
    contrast.set_boolean("high-contrast", true).unwrap();
    gtk::Settings::default()
        .unwrap()
        .set_gtk_enable_animations(false);
    pump_for(Duration::from_millis(200));
    assert!(adw::StyleManager::default().is_high_contrast());
    let (preferences, _) = preferences_window(&ctx);
    let row = row_by_identity(&preferences, "github:12/sample");
    let shield = named_shield(&row);
    crate::app::notification_ui_tests::accessible_label(&shield, "Plugin from another publisher");
    assert!(shield
        .tooltip_text()
        .is_some_and(|text| text.starts_with("Plugin from another publisher")));
    let info = named_button(&row, "plugin-info");
    crate::app::notification_ui_tests::accessible_label(&info, "Details of Sample");
    assert!(info.grab_focus());
    xdo(&["key", "--clearmodifiers", "Return"]);
    wait_for(
        || plugin_page_button(&preferences, "github:12/sample", "Uninstall").is_mapped(),
        Duration::from_secs(3),
    );
    screenshot(&preferences, "plugin-high-contrast-static");
    assert!(!gtk::Settings::default().unwrap().is_gtk_enable_animations());
    preferences.destroy();
    ctx.window.destroy();
}
