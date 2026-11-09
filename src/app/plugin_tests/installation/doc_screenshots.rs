//! One window walked through every Preferences tab, for the screenshots in the
//! documentation and the release notes. The scene is built from plausible
//! official plugins and one third-party source so the pictures show what a
//! user sees, and every capture first asserts the content it is meant to show.
//! The raw captures are written below `AMONG_CHECK_ROOT/preferences`;
//! `among-tools prepare-preferences-screenshots` crops and optimizes them.

use super::list::{origin, preferences_window, save_source};
use super::*;
use crate::plugin::install::{
    listing::tests::{digest, document},
    source::Inspection,
};
use crate::plugin::inventory::tests::{fixture_plugin, release_of, source_receipt, Identity};
use crate::plugin::origin::Origin;
use serde_json::json;

const OFFICIAL: (u64, &str, &str) = (1_431_055_733, "Among-Terminal", "among-plugins");
const OFFICIAL_PUBLISHER: u64 = 339_567_100;

/// A catalogue document whose plugins carry the descriptions a real source
/// would publish, instead of the generic fixture text.
fn described(source: Origin, plugins: &[(&str, &str, &str, &str)]) -> Inspection {
    let specs: Vec<_> = plugins
        .iter()
        .map(|(id, name, version, _)| (*id, *name, *version, digest('b')))
        .collect();
    let specs: Vec<_> = specs
        .iter()
        .map(|(id, name, version, sum)| (*id, *name, *version, sum.as_str()))
        .collect();
    let mut catalogue = document(&specs);
    for (index, (_, _, _, description)) in plugins.iter().enumerate() {
        let version = &mut catalogue["plugins"][index]["versions"][0];
        version["manifest"]["description"] = json!(description);
        // The list shows the download size; the generic fixture's 123 bytes
        // would look like a mistake in a picture meant to show a real source.
        version["artifacts"][0]["size"] = json!(262_144);
    }
    Inspection::from_bytes(source, "a".repeat(40), catalogue.to_string().into_bytes()).unwrap()
}

fn shoot(window: &impl IsA<gtk::Window>, name: &str) {
    settle();
    let id = focus(window);
    let directory = crate::test_isolation::check_root().join("preferences");
    std::fs::create_dir_all(&directory).unwrap();
    let path = directory.join(format!("{name}.png"));
    assert!(std::process::Command::new("import")
        .args(["-screen", "-window", &id])
        .arg(&path)
        .status()
        .unwrap()
        .success());
    println!("Screenshot: {}", path.display());
}

fn show_tab(preferences: &adw::PreferencesWindow, title: &str) {
    let page = widgets(preferences.upcast_ref())
        .into_iter()
        .filter_map(|widget| widget.downcast::<adw::PreferencesPage>().ok())
        .find(|page| page.title() == title)
        .unwrap_or_else(|| panic!("no tab {title}"));
    preferences.set_visible_page(&page);
    wait_for(|| page.is_mapped(), Duration::from_secs(3));
}

/// Picks a dropdown entry by its label through the row's own selection, which
/// runs the same handler a click on the entry would.
fn choose(preferences: &adw::PreferencesWindow, name: &str, label: &str) {
    let row = widgets(preferences.upcast_ref())
        .into_iter()
        .filter_map(|widget| widget.downcast::<adw::ComboRow>().ok())
        .find(|row| row.widget_name() == name)
        .unwrap_or_else(|| panic!("no dropdown named {name}"));
    let model = row
        .model()
        .and_downcast::<gtk::StringList>()
        .expect("a string model");
    let position = (0..model.n_items())
        .find(|index| model.string(*index).as_deref() == Some(label))
        .unwrap_or_else(|| panic!("{name} has no entry {label}"));
    row.set_selected(position);
    assert_eq!(row.selected(), position);
}

/// Makes the application's own icon findable: the container has no installed
/// package, and the About page shows the themed icon by name.
fn install_application_icon() {
    const ICON: &[u8] = include_bytes!("../../../../data/com.github.among-terminal.svg");
    let base = crate::test_isolation::check_root().join("icons");
    let directory = base.join("hicolor/scalable/apps");
    std::fs::create_dir_all(&directory).unwrap();
    std::fs::write(directory.join("com.github.among-terminal.svg"), ICON).unwrap();
    gtk::IconTheme::for_display(&gtk::gdk::Display::default().unwrap()).add_search_path(&base);
}

fn resize(preferences: &adw::PreferencesWindow, height: u32) {
    let id = focus(preferences);
    xdo(&["windowsize", "--sync", &id, "820", &height.to_string()]);
    // A window cannot shrink below what its content needs, so the final size
    // is reported rather than required; the captures are then judged by eye.
    let end = std::time::Instant::now() + Duration::from_millis(1500);
    while std::time::Instant::now() < end {
        glib::MainContext::default().iteration(false);
        std::thread::sleep(Duration::from_millis(5));
    }
    eprintln!("window {}x{}", preferences.width(), preferences.height());
}

fn back(preferences: &adw::PreferencesWindow) {
    focus(preferences);
    xdo(&["key", "--clearmodifiers", "Escape"]);
    settle();
}

fn row_titled(preferences: &adw::PreferencesWindow, title: &str) -> adw::ActionRow {
    wait_for(
        || {
            widgets(preferences.upcast_ref())
                .iter()
                .any(|widget| widget.is::<adw::ActionRow>() && widget.is_mapped())
        },
        Duration::from_secs(3),
    );
    widgets(preferences.upcast_ref())
        .into_iter()
        .filter_map(|widget| widget.downcast::<adw::ActionRow>().ok())
        .find(|row| row.title() == title && row.is_mapped())
        .unwrap_or_else(|| panic!("no shown row titled {title}"))
}

#[test]
#[ignore = "requires an isolated display, xdotool and ImageMagick"]
fn every_preferences_tab_is_captured_for_the_documentation() {
    let root = crate::plugin::inventory::root().unwrap();
    crate::test_isolation::assert_below_check_root(&root);
    let clock = fixture_plugin(
        &root,
        Identity {
            id: "clock",
            name: "Clock",
            description: "Local time in the header",
            repository: OFFICIAL,
        },
        "0.2.0",
    );
    let status = fixture_plugin(
        &root,
        Identity {
            id: "system-status",
            name: "System status",
            description: "CPU, memory and disk bars",
            repository: OFFICIAL,
        },
        "0.2.0",
    );
    // An installed official plugin carries the publisher of the official
    // source; without it the page would call the plugin unreviewed.
    let published_officially = |mut receipt| {
        let mut source = source_receipt(&receipt);
        source.publisher_id = OFFICIAL_PUBLISHER;
        release_of(&mut receipt).source = Some(source);
        receipt
    };
    let installed = [published_officially(clock), published_officially(status)];
    std::fs::write(
        root.join("installed.json"),
        json!({"version":1,"installations":installed}).to_string(),
    )
    .unwrap();
    let sources = crate::plugin::install::registry::root().unwrap();
    let official = origin(OFFICIAL.0, OFFICIAL_PUBLISHER, OFFICIAL.1, OFFICIAL.2);
    save_source(
        &sources,
        described(
            official,
            &[
                ("clock", "Clock", "0.2.0", "Local time in the header"),
                (
                    "system-status",
                    "System status",
                    "0.2.0",
                    "CPU, memory and disk bars",
                ),
                (
                    "pane-pwd",
                    "Pane directory",
                    "0.2.0",
                    "Working directory per pane",
                ),
            ],
        ),
    );
    let third_party = origin(77, 34, "example", "among-plugins");
    save_source(
        &sources,
        described(
            third_party,
            &[("notes", "Notes", "0.4.0", "A small note pad")],
        ),
    );

    // A recorded notification service on the private bus, so the Notifications
    // tab shows what a desktop with a service shows instead of the note that
    // none is available.
    let _service = crate::app::desktop_notification_tests::Server::new();
    let app = application(crate::app::desktop_notification_tests::APP_ID);
    let ctx = build_with_writer(&app, None, crate::session::Writer::new).unwrap();
    wait_for(|| ctx.state.borrow().session_ready, Duration::from_secs(5));
    let bells = ctx.state.borrow().bells.clone().unwrap();
    wait_for(|| bells.desktop_report().is_none(), Duration::from_secs(5));
    adw::StyleManager::default().set_color_scheme(adw::ColorScheme::ForceDark);
    install_application_icon();
    let (preferences, _) = preferences_window(&ctx);
    wait_for(
        || has_label(preferences.upcast_ref(), "example/among-plugins"),
        Duration::from_secs(5),
    );

    resize(&preferences, 640);
    assert!(has_label(preferences.upcast_ref(), "Installed"));
    assert!(has_label(preferences.upcast_ref(), "Available"));
    assert!(has_label(preferences.upcast_ref(), "Pane directory"));
    shoot(&preferences, "plugins");

    // The info page of an installed plugin.
    press(&named_button(
        &row_titled(&preferences, "System status"),
        "plugin-info",
    ));
    wait_for(
        || has_label(preferences.upcast_ref(), "Can read"),
        Duration::from_secs(3),
    );
    resize(&preferences, 720);
    shoot(&preferences, "plugin-info");
    back(&preferences);

    // Switching a plugin on asks for a review of what it may read.
    resize(&preferences, 640);
    let clock_row = row_titled(&preferences, "Clock");
    let switch = widgets(clock_row.upcast_ref())
        .into_iter()
        .find_map(|widget| widget.downcast::<gtk::Switch>().ok())
        .unwrap();
    focus(&preferences);
    assert!(switch.is_sensitive() && switch.grab_focus());
    xdo(&["key", "--clearmodifiers", "space"]);
    wait_for(
        || preferences.visible_dialog().is_some(),
        Duration::from_secs(3),
    );
    let dialog = preferences.visible_dialog().unwrap();
    assert!(has_label(dialog.upcast_ref(), "Review plugin access"));
    resize(&preferences, 720);
    shoot(&preferences, "plugin-access-review");
    press(&button(&dialog, "Cancel"));
    settle();

    // The Sources page.
    resize(&preferences, 640);
    let sources_row = row_titled(&preferences, "Sources");
    focus(&preferences);
    adw::prelude::ActionRowExt::activate(&sources_row);
    wait_for(
        || has_label(preferences.upcast_ref(), "example/among-plugins"),
        Duration::from_secs(3),
    );
    shoot(&preferences, "sources");
    back(&preferences);

    // Tall enough for the whole page, including the Command group at the end.
    resize(&preferences, 1100);
    show_tab(&preferences, "Notifications");
    assert!(has_label(preferences.upcast_ref(), "Command"));
    shoot(&preferences, "notifications");
    resize(&preferences, 820);
    show_tab(&preferences, "Keyboard");
    shoot(&preferences, "keyboard");
    resize(&preferences, 420);
    show_tab(&preferences, "Terminal");
    choose(&preferences, "terminal-text-background", "Solarized dark");
    choose(&preferences, "terminal-ansi-palette", "Solarized");
    shoot(&preferences, "terminal");
    // The dropdown open. The high-contrast scheme is not captured here: it is
    // chosen by the environment before the process starts, not by a call.
    resize(&preferences, 640);
    adw::prelude::ActionRowExt::activate(&first_of::<adw::ComboRow>(&preferences));
    settle();
    shoot(&preferences, "terminal-dropdown");
    focus(&preferences);
    xdo(&["key", "--clearmodifiers", "Escape"]);
    settle();
    resize(&preferences, 680);
    show_tab(&preferences, "About");
    shoot(&preferences, "about");

    // Search in the plugin list.
    resize(&preferences, 640);
    show_tab(&preferences, "Plugins");
    let search: gtk::SearchEntry = first_of(&preferences);
    focus(&preferences);
    assert!(search.grab_focus());
    xdo(&["type", "--clearmodifiers", "clock"]);
    settle();
    shoot(&preferences, "plugins-search");
    xdo(&["key", "--clearmodifiers", "ctrl+a", "BackSpace"]);

    // The add-source dialog, then the command arguments one per line.
    focus(&preferences);
    adw::prelude::ActionRowExt::activate(&row_titled(&preferences, "Sources"));
    wait_for(
        || has_label(preferences.upcast_ref(), "Add source"),
        Duration::from_secs(3),
    );
    adw::prelude::ActionRowExt::activate(&row_titled(&preferences, "Add source"));
    wait_for(
        || preferences.visible_dialog().is_some(),
        Duration::from_secs(3),
    );
    shoot(&preferences, "sources-add");
    focus(&preferences);
    xdo(&["key", "--clearmodifiers", "Escape"]);
    settle();
    back(&preferences);
    resize(&preferences, 1100);
    show_tab(&preferences, "Notifications");
    let arguments: gtk::TextView = first_of(&preferences);
    arguments
        .buffer()
        .set_text("--urgency=low\nAmong Terminal\nA bell rang");
    settle();
    shoot(&preferences, "notifications-command");

    // The same Plugins and Terminal tabs in the light scheme.
    adw::StyleManager::default().set_color_scheme(adw::ColorScheme::ForceLight);
    settle();
    resize(&preferences, 420);
    show_tab(&preferences, "Terminal");
    shoot(&preferences, "terminal-light");
    show_tab(&preferences, "Plugins");
    resize(&preferences, 640);
    shoot(&preferences, "plugins-light");
    preferences.destroy();
    adw::StyleManager::default().set_color_scheme(adw::ColorScheme::ForceDark);

    // The main window: two panes, the sidebar, and the attention pulse of the
    // pane that rang the bell while the other one has focus.
    let id = focus(&ctx.window);
    xdo(&["windowsize", "--sync", &id, "1100", "700"]);
    let terminals = || {
        widgets(ctx.window.upcast_ref())
            .into_iter()
            .filter_map(|widget| widget.downcast::<vte::Terminal>().ok())
            .collect::<Vec<_>>()
    };
    xdo(&["key", "--clearmodifiers", "ctrl+shift+n"]);
    wait_for(|| terminals().len() == 2, Duration::from_secs(5));
    settle();
    for terminal in terminals() {
        terminal.feed_child(b"ls --color=always /usr; echo\n");
    }
    settle();
    shoot(&ctx.window, "main-window");
    let ringing = terminals()
        .into_iter()
        .find(|terminal| !terminal.has_focus())
        .expect("one pane is unfocused");
    ringing.feed_child(b"printf '\\a'\n");
    shoot_now(&ctx.window, "main-window-bell");
    ctx.window.destroy();
}

/// Like `shoot`, but without the settling pause: for a state that fades, such as
/// the attention pulse of a pane.
fn shoot_now(window: &impl IsA<gtk::Window>, name: &str) {
    let id = focus(window);
    let directory = crate::test_isolation::check_root().join("preferences");
    std::fs::create_dir_all(&directory).unwrap();
    assert!(std::process::Command::new("import")
        .args(["-screen", "-window", &id])
        .arg(directory.join(format!("{name}.png")))
        .status()
        .unwrap()
        .success());
}

fn first_of<T: IsA<gtk::Widget>>(root: &impl IsA<gtk::Widget>) -> T {
    widgets(root.as_ref())
        .into_iter()
        .filter_map(|widget| widget.downcast::<T>().ok())
        .find(|widget| widget.is_mapped())
        .unwrap_or_else(|| panic!("no shown {}", std::any::type_name::<T>()))
}

/// What a user sees the first time Preferences opens with no saved sources and
/// no network: the loading state and then the failure.
#[test]
#[ignore = "requires an isolated display, xdotool and ImageMagick"]
fn the_first_open_without_a_network_is_captured_for_the_documentation() {
    let app = application("com.github.among-terminal.preferences-first-open-test");
    let ctx = build_with_writer(&app, None, crate::session::Writer::new).unwrap();
    wait_for(|| ctx.state.borrow().session_ready, Duration::from_secs(5));
    adw::StyleManager::default().set_color_scheme(adw::ColorScheme::ForceDark);
    let (preferences, _) = preferences_window(&ctx);
    resize(&preferences, 640);
    shoot_now(&preferences, "plugins-loading");
    let end = std::time::Instant::now() + Duration::from_secs(8);
    while std::time::Instant::now() < end {
        glib::MainContext::default().iteration(false);
        std::thread::sleep(Duration::from_millis(5));
    }
    shoot(&preferences, "plugins-offline");
    preferences.destroy();
    ctx.window.destroy();
}
