//! The preferences window: plugins, notifications, keyboard shortcuts, terminal
//! appearance and About.
//!
//! Both features have to be inspectable and changeable *by the user*, not just by
//! editing configuration. Putting them in one window is also what makes the
//! capability discoverable: a binding registry that can only be read from a
//! debug log is not user-facing in any meaningful sense.

use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;
use gtk::gdk;

use crate::plugin::host::{Configuration, PluginState};
use crate::shortcuts::{self, Action, Bindings};

mod about;
pub(crate) mod general;
pub(crate) mod local_catalogue;
pub(crate) mod notification_role;
mod plugin_manager;
mod plugin_settings;
pub(crate) mod repositories;
mod repository_sources;
mod retained_settings;
pub(crate) mod terminal;

pub(crate) type ManagedDetails = (crate::plugin::inventory::Entry, Option<serde_json::Value>);
pub(crate) type PluginDetailsQuery = dyn Fn(&str) -> Option<ManagedDetails>;
type ConfigurationQuery = dyn Fn(&str) -> Configuration;
type EnablePlugin = dyn Fn(&str, Option<gtk::Window>);
type AllowProtection = dyn Fn(&str, &str) -> Result<(), String>;
type RestartPlugin = dyn Fn(&str, &str, Option<gtk::Window>);
type PackageAction = dyn Fn(
    crate::plugin::inventory::Receipt,
    crate::plugin::install::mutation::Action,
    Option<gtk::Window>,
);
type SavePluginSettings = dyn Fn(
    &str,
    &str,
    &serde_json::Map<String, serde_json::Value>,
) -> Result<crate::settings::Durability, String>;
type RetainedSettingsQuery = dyn Fn() -> Result<Vec<crate::settings::RetainedSettings>, String>;
type DeleteRetainedSettings =
    dyn Fn(&crate::settings::RetainedSettings) -> Result<crate::settings::Durability, String>;

/// What the preferences window needs from the running application.
///
/// Handed over as closures rather than a reference to the window so this module
/// does not need to know how the application stores its state.
#[derive(Clone)]
pub struct PreferencesContext {
    pub general: Option<general::Context>,
    pub on_closed: Rc<dyn Fn()>,
    pub storage_status: gtk::StringList,
    pub bindings: Rc<RefCell<Bindings>>,
    pub plugin_inventory: Rc<dyn Fn() -> Vec<crate::plugin::host::Overview>>,
    pub plugin_details: Rc<PluginDetailsQuery>,
    pub save_plugin_settings: Rc<SavePluginSettings>,
    pub plugin_configuration: Rc<ConfigurationQuery>,
    pub restart_plugin: Rc<RestartPlugin>,
    pub enable_plugin: Rc<EnablePlugin>,
    pub disable_plugin: Rc<dyn Fn(&str)>,
    /// Records the user's consent to start a plugin at a reduced protection
    /// level (identified by the level's token) and tries the start again.
    pub allow_reduced_protection: Rc<AllowProtection>,
    pub installations_changed: Rc<dyn Fn()>,
    pub package_action: Rc<PackageAction>,
    pub retained_settings: Rc<RetainedSettingsQuery>,
    pub delete_retained_settings: Rc<DeleteRetainedSettings>,
    /// Rebuilds the window's shortcut controller after a rebind.
    pub bindings_changed: Rc<dyn Fn()>,
    pub terminal: terminal::Context,
}

/// Open the preferences window, parented to the main window.
pub fn present(parent: &impl IsA<gtk::Window>, ctx: PreferencesContext) {
    let window = adw::PreferencesWindow::builder()
        .transient_for(parent)
        .modal(false)
        .search_enabled(false)
        // Wide enough for five full labels plus the native switcher padding.
        .default_width(820)
        .build();
    window.set_title(Some("Preferences"));
    let on_closed = ctx.on_closed.clone();
    window.connect_destroy(move |_| on_closed());

    let plugins = plugins_page(&ctx);
    let notifications = general::page(ctx.general.as_ref());
    add_storage_status(&notifications, &ctx);
    let terminal = terminal::page(&ctx);
    add_storage_status(&terminal, &ctx);
    window.add(&plugins);
    window.add(&notifications);
    window.add(&shortcuts_page(&ctx));
    window.add(&terminal);
    window.add(&about::page());
    window.set_visible_page(&plugins);
    equalize_tabs(&window);
    window.present();
}

/// Give every tab of the page switcher the same width.
///
/// The stock switcher sizes each tab to its own label, so tabs of different
/// length leave uneven gaps. The tab buttons are internal to the switcher, so
/// they are found by their type name; if a libadwaita release renames them the
/// tabs keep their natural widths, which is only a cosmetic loss.
fn equalize_tabs(window: &adw::PreferencesWindow) {
    fn collect(widget: &gtk::Widget, found: &mut Vec<gtk::Widget>) {
        if widget.type_().name() == "AdwViewSwitcherButton" {
            found.push(widget.clone());
        }
        let mut child = widget.first_child();
        while let Some(widget) = child {
            collect(&widget, found);
            child = widget.next_sibling();
        }
    }
    let mut buttons = Vec::new();
    collect(window.upcast_ref(), &mut buttons);
    let group = gtk::SizeGroup::new(gtk::SizeGroupMode::Horizontal);
    for button in &buttons {
        group.add_widget(button);
    }
}

fn shortcuts_page(ctx: &PreferencesContext) -> adw::PreferencesPage {
    let page = adw::PreferencesPage::new();
    page.set_title("Keyboard");
    page.set_name(Some("keyboard"));
    page.set_icon_name(Some("preferences-desktop-keyboard-symbolic"));

    let group = adw::PreferencesGroup::new();
    group.set_title("Shortcuts");
    group.set_description(Some(
        "Select a shortcut to change it. Select it again to cancel, or press \
         Escape to remove the binding.",
    ));
    page.add(&group);
    add_storage_status(&page, ctx);

    for action in Bindings::default_actions() {
        group.add(&shortcut_row(ctx, action));
    }
    page
}

fn add_storage_status(page: &adw::PreferencesPage, ctx: &PreferencesContext) {
    let group = adw::PreferencesGroup::new();
    group.set_title("Settings storage");
    let row = adw::ActionRow::new();
    row.set_title("Some preferences could not be saved or loaded");
    let status = ctx.storage_status.clone();
    let text = status.string(0).unwrap_or_default();
    row.set_subtitle(&escape_display(&text));
    group.set_visible(!text.is_empty());
    group.add(&row);
    let weak_group = group.downgrade();
    let weak_row = row.downgrade();
    status.connect_items_changed(move |status, _, _, _| {
        if let (Some(group), Some(row)) = (weak_group.upgrade(), weak_row.upgrade()) {
            let text = status.string(0).unwrap_or_default();
            row.set_subtitle(&escape_display(&text));
            group.set_visible(!text.is_empty());
        }
    });
    page.add(&group);
}

/// One row per action, showing the current binding and able to rebind it.
/// Escape an accelerator for display in a Pango-markup label.
///
/// An accelerator looks like `<Alt>Up`, which markup reads as an unclosed tag;
/// left as is, the row renders nothing at all.
fn escape_display(accel: &str) -> String {
    accel
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('\'', "&apos;")
        .replace('"', "&quot;")
}

fn shortcut_row(ctx: &PreferencesContext, action: Action) -> adw::ActionRow {
    let row = adw::ActionRow::new();
    row.set_title(&action.label());
    row.set_activatable(true);

    let bound = ctx
        .bindings
        .borrow()
        .accel_for(action)
        .unwrap_or("")
        .to_string();
    row.set_subtitle(&shortcuts::describe(&escape_display(&bound)));

    // Set while the row is waiting for the next key press.
    let capturing = Rc::new(RefCell::new(false));
    let generation = Rc::new(std::cell::Cell::new(0u64));
    let bindings = ctx.bindings.clone();
    let changed = ctx.bindings_changed.clone();

    row.connect_activated(move |row| {
        generation.set(generation.get().wrapping_add(1));
        if *capturing.borrow() {
            // Second activation cancels.
            *capturing.borrow_mut() = false;
            row.remove_css_class("capture");
            let current = bindings
                .borrow()
                .accel_for(action)
                .unwrap_or("")
                .to_string();
            row.set_subtitle(&shortcuts::describe(&escape_display(&current)));
            return;
        }
        *capturing.borrow_mut() = true;
        row.add_css_class("capture");
        row.set_subtitle("Press a key combination…");

        // `root()` is the toplevel, which is where a capture controller must
        // live to see the key before any focused widget consumes it.
        let window = row
            .root()
            .and_then(|root| root.downcast::<gtk::Window>().ok());

        let row_for_controller = row.clone();
        let capturing = capturing.clone();
        let bindings = bindings.clone();
        let changed = changed.clone();
        let action = action;
        let finish = move |sequence: Option<String>| {
            *capturing.borrow_mut() = false;
            row_for_controller.remove_css_class("capture");
            {
                let mut borrowed = bindings.borrow_mut();
                match sequence {
                    // A sequence the desktop would swallow is refused rather
                    // than accepted, because the action would then never fire.
                    Some(seq) if shortcuts::accel_is_valid(&seq) => {
                        borrowed.rebind(action, &seq);
                    }
                    // An unusable sequence leaves the binding alone instead of
                    // silently disabling the action.
                    Some(_) => {}
                    None => borrowed.unbind(action),
                }
            }
            let current = bindings
                .borrow()
                .accel_for(action)
                .unwrap_or("")
                .to_string();
            row_for_controller.set_subtitle(&shortcuts::describe(&escape_display(&current)));
            changed();
        };

        match window {
            Some(window) => {
                let current = generation.get();
                let generation = generation.clone();
                attach_capture(
                    &window,
                    Rc::new(finish),
                    Rc::new(move || generation.get() == current),
                );
            }
            None => finish(None),
        }
    });

    row
}

/// Listen for one key press in capture phase and stop it reaching the terminal.
///
/// Capture phase matters: the sequence being recorded must not first arrive at
/// the focused widget, or rebinding a shortcut would type into the shell.
fn attach_capture(
    window: &gtk::Window,
    on_key: Rc<dyn Fn(Option<String>)>,
    active: Rc<dyn Fn() -> bool>,
) {
    let controller = gtk::EventControllerKey::new();
    controller.set_propagation_phase(gtk::PropagationPhase::Capture);
    let fired = Rc::new(RefCell::new(false));

    controller.connect_key_pressed({
        let fired = fired.clone();
        let on_key = on_key.clone();
        move |_, key, _code, modifiers| {
            if !active() {
                return glib::Propagation::Proceed;
            }
            if *fired.borrow() {
                return glib::Propagation::Proceed;
            }
            *fired.borrow_mut() = true;
            // Escape means "remove this binding"; it must not be recorded as a
            // shortcut in its own right.
            if key == gdk::Key::Escape {
                on_key(None);
            } else {
                // GTK renders the accelerator from the key and the modifiers
                // held with it, which is exactly the form `Bindings` stores.
                let accel = gtk::accelerator_name(key, modifiers);
                on_key(Some(accel.to_string()));
            }
            glib::Propagation::Stop
        }
    });

    window.add_controller(controller);
}

fn plugins_page(ctx: &PreferencesContext) -> adw::PreferencesPage {
    let page = plugin_manager::page(ctx);
    page.set_name(Some("plugins"));
    page
}

fn describe_state(state: &PluginState) -> String {
    match state {
        PluginState::Unavailable { reason } => format!("Unavailable ({reason})"),
        PluginState::Waiting => "Waiting for previous plugins to stop".into(),
        PluginState::Starting => "Starting".into(),
        PluginState::Stopping { reason, .. } => format!("Stopping ({reason})"),
        PluginState::Running => "Running".into(),
        PluginState::Disabled => "Disabled".into(),
        PluginState::Missing { reason } => format!("Not installed ({reason})"),
        PluginState::Faulted { reason } => format!("Stopped ({reason})"),
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_accelerator_survives_being_shown_as_markup() {
        // `<Alt>Up` read as markup is an unclosed tag, and the row would render
        // nothing at all.
        let shown = escape_display("<Alt>Up");
        assert_eq!(shown, "&lt;Alt&gt;Up");
        assert!(!shown.contains('<'));
    }

    #[test]
    fn every_default_accelerator_is_escaped_safely() {
        for (_, accel) in Bindings::defaults().listing() {
            let escaped = escape_display(&accel);
            assert!(!escaped.contains('<'), "{accel} still contains a raw '<'");
            assert!(!escaped.contains('>'), "{accel} still contains a raw '>'");
        }
    }

    #[test]
    fn an_unbound_action_reads_as_disabled() {
        assert_eq!(shortcuts::describe(""), "Disabled");
        assert_eq!(shortcuts::describe("<Alt>Up"), "<Alt>Up");
    }

    #[test]
    fn every_default_action_is_listed_even_if_unbound() {
        let actions = Bindings::default_actions();
        assert!(actions.contains(&Action::OpenPreferences));
        assert!(actions.contains(&Action::SplitPane));
        // A registry that only listed bound actions would hide what could be
        // bound, which is half the point of the page.
        assert!(!actions.is_empty());
    }

    #[test]
    #[ignore = "requires an isolated GTK display"]
    fn the_card_marks_reduced_protection_and_offers_consent_only_while_a_start_waits() {
        use crate::plugin::sandbox::level::{consent_message, Protection};
        adw::init().expect("isolated GTK display");
        fn context(overview: crate::plugin::host::Overview) -> PreferencesContext {
            PreferencesContext {
                general: None,
                package_action: Rc::new(|_, _, _| {}),
                retained_settings: Rc::new(|| Ok(Vec::new())),
                delete_retained_settings: Rc::new(|_| Err("no retained settings".into())),
                installations_changed: Rc::new(|| {}),
                on_closed: Rc::new(|| {}),
                storage_status: gtk::StringList::new(&[]),
                bindings: Rc::new(RefCell::new(Bindings::defaults())),
                plugin_inventory: Rc::new(move || vec![overview.clone()]),
                plugin_details: Rc::new(|_| None),
                save_plugin_settings: Rc::new(|_, _, _| Err("no settings in this fixture".into())),
                plugin_configuration: Rc::new(|_| Configuration::NotApplied),
                restart_plugin: Rc::new(|_, _, _| {}),
                enable_plugin: Rc::new(|_, _| {}),
                disable_plugin: Rc::new(|_| {}),
                allow_reduced_protection: Rc::new(|_, _| Ok(())),
                bindings_changed: Rc::new(|| {}),
                terminal: crate::prefs::terminal::test_context(),
            }
        }
        fn collect<T: IsA<gtk::Widget> + gtk::glib::object::IsClass>(
            widget: &gtk::Widget,
            found: &mut Vec<T>,
        ) {
            if let Ok(item) = widget.clone().downcast::<T>() {
                found.push(item);
            }
            let mut child = widget.first_child();
            while let Some(widget) = child {
                collect::<T>(&widget, found);
                child = widget.next_sibling();
            }
        }
        // the card of one plugin: its subtitle and its consent button, if any.
        let card = |overview: crate::plugin::host::Overview| {
            let page = plugins_page(&context(overview));
            let mut rows = Vec::new();
            collect::<adw::ActionRow>(page.upcast_ref(), &mut rows);
            let row = rows
                .into_iter()
                .find(|row| row.title() == "sample")
                .expect("the plugin's card");
            let mut buttons = Vec::new();
            collect::<gtk::Button>(row.upcast_ref(), &mut buttons);
            let consent = buttons
                .into_iter()
                .find(|button| button.label().as_deref() == Some("Allow reduced protection…"))
                .expect("the card always carries the consent button; only its visibility changes");
            (
                row.subtitle().unwrap_or_default().to_string(),
                consent.is_visible(),
            )
        };

        let offered = Protection::Reduced { abi: 2 };
        let mut refused = crate::plugin::host::Overview::fixture(
            "sample",
            PluginState::Unavailable {
                reason: consent_message(offered, None),
            },
            true,
        );
        refused.protection = Some(format!("Not started: {}", consent_message(offered, None)));
        refused.consent_offer = Some(offered);
        let (subtitle, button) = card(refused);
        assert!(subtitle.contains(
            "the kernel supports only Landlock ABI 2 (required: ABI 3, kernel 6.2 or newer); \
             the plugin was not started."
        ));
        assert!(button, "a refused start offers the consent action");

        let mut full = crate::plugin::host::Overview::fixture("sample", PluginState::Running, true);
        full.protection = Some("Protection: full (Landlock ABI 6 and seccomp)".into());
        let (subtitle, button) = card(full);
        assert!(subtitle.contains("Protection: full (Landlock ABI 6 and seccomp)"));
        assert!(!subtitle.contains("reduced"));
        assert!(!button, "full protection needs no consent");

        let mut reduced =
            crate::plugin::host::Overview::fixture("sample", PluginState::Running, true);
        reduced.protection = Some(format!(
            "Protection: {} · running with reduced protection",
            offered.label()
        ));
        let (subtitle, button) = card(reduced);
        assert!(subtitle.contains("running with reduced protection"));
        assert!(
            !button,
            "an allowed plugin shows the permanent mark, not the action"
        );
    }

    #[test]
    #[ignore = "requires an attached X11 display and a real Preferences window"]
    fn actual_preferences_measures_five_tabs() {
        adw::init().expect("attached X11 display");
        use crate::plugin::host::Overview;
        use crate::plugin::install::TestOwnedRoot;
        use crate::plugin::inventory;

        let root = TestOwnedRoot::new();
        inventory::tests::fixture(&root.0);
        let entry = inventory::read(&root.0).unwrap().remove(0);
        let key = entry.key();

        fn context(
            key: &str,
            entry: &crate::plugin::inventory::Entry,
            overview: Overview,
        ) -> PreferencesContext {
            let details_key = key.to_owned();
            let details_entry = entry.clone();
            PreferencesContext {
                general: None,
                package_action: Rc::new(|_, _, _| {}),
                retained_settings: Rc::new(|| Ok(Vec::new())),
                delete_retained_settings: Rc::new(|_| Err("no retained settings".into())),
                installations_changed: Rc::new(|| {}),
                on_closed: Rc::new(|| {}),
                storage_status: gtk::StringList::new(&[]),
                bindings: Rc::new(RefCell::new(Bindings::defaults())),
                plugin_inventory: Rc::new(move || vec![overview.clone()]),
                plugin_details: Rc::new(move |asked| {
                    (asked == details_key.as_str()).then(|| (details_entry.clone(), None))
                }),
                save_plugin_settings: Rc::new(|_, _, _| Err("no settings in this fixture".into())),
                plugin_configuration: Rc::new(|_| Configuration::NotApplied),
                restart_plugin: Rc::new(|_, _, _| {}),
                enable_plugin: Rc::new(|_, _| {}),
                disable_plugin: Rc::new(|_| {}),
                allow_reduced_protection: Rc::new(|_, _| Ok(())),
                bindings_changed: Rc::new(|| {}),
                terminal: crate::prefs::terminal::test_context(),
            }
        }
        fn pump_until(condition: &dyn Fn() -> bool, what: &str) {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
            while !condition() {
                assert!(
                    std::time::Instant::now() < deadline,
                    "timed out waiting for {what}"
                );
                gtk::glib::MainContext::default().iteration(false);
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
        }
        fn collect(widget: &gtk::Widget, found: &mut Vec<gtk::Widget>) {
            found.push(widget.clone());
            let mut child = widget.first_child();
            while let Some(node) = child {
                child = node.next_sibling();
                collect(&node, found);
            }
        }
        fn toolbars(widget: &gtk::Widget, found: &mut Vec<adw::ToolbarView>) {
            if let Some(toolbar) = widget.downcast_ref::<adw::ToolbarView>() {
                found.push(toolbar.clone());
            }
            let mut child = widget.first_child();
            while let Some(node) = child {
                child = node.next_sibling();
                toolbars(&node, found);
            }
        }
        let mut overview = Overview::fixture(&key, PluginState::Running, true);
        overview.name = "Sample".into();
        overview.description = "No probe".into();
        overview.protection = Some(
            "Protection: full (Landlock ABI 6 and seccomp) · running with reduced protection"
                .into(),
        );
        let parent = gtk::Window::new();
        present(&parent, context(&key, &entry, overview));
        let prefs = gtk::Window::list_toplevels()
            .into_iter()
            .find_map(|window| window.downcast::<adw::PreferencesWindow>().ok())
            .expect("Preferences window");
        pump_until(
            &|| prefs.is_mapped() && prefs.width() > 0,
            "Preferences allocation",
        );
        assert_eq!(prefs.default_width(), 820, "production default width");
        let widgets = {
            let mut widgets = Vec::new();
            collect(prefs.upcast_ref(), &mut widgets);
            widgets
        };
        let tabs: Vec<_> = widgets
            .iter()
            .filter(|widget| widget.type_().name() == "AdwViewSwitcherButton" && widget.is_mapped())
            .cloned()
            .collect();
        assert_eq!(tabs.len(), 5, "five mapped page tabs");
        let widths: Vec<i32> = tabs.iter().map(|tab| tab.width()).collect();
        let minimum = widths.iter().copied().min().unwrap_or(0);
        let maximum = widths.iter().copied().max().unwrap_or(0);
        assert!(
            minimum > 0 && maximum - minimum <= 1,
            "five homogeneous tabs must be mapped with at most one physical pixel of integer-allocation rounding: {widths:?}"
        );
        let expected_labels = ["Plugins", "Notifications", "Keyboard", "Terminal", "About"];
        let mut seen_labels = Vec::new();
        for (tab_index, tab) in tabs.iter().enumerate() {
            let expected = expected_labels[tab_index];
            let mut descendants = Vec::new();
            collect(tab, &mut descendants);
            let labels: Vec<gtk::Label> = descendants
                .into_iter()
                .filter_map(|widget| widget.downcast::<gtk::Label>().ok())
                .filter(|label| !label.text().is_empty() && label.is_mapped())
                .collect();
            let before = seen_labels.len();
            for label in labels {
                let logical = label.layout().pixel_extents().1;
                let text_width = logical.width();
                let allocation = label.width().max(1);
                let parent_width = tab
                    .compute_bounds(prefs.upcast_ref::<gtk::Widget>())
                    .map(|rect| rect.width() as i32)
                    .unwrap_or(0);
                let natural = label.measure(gtk::Orientation::Horizontal, -1).1;
                let parent_measure = label
                    .parent()
                    .map(|parent| parent.measure(gtk::Orientation::Horizontal, -1));
                eprintln!(
                    "tab {tab_index} expected {expected:?}: text={:?} mapped={} ellipsized={} text_width={} natural={} allocation={} parent_width={} max_width_chars={} width_chars={} width_request={} halign={:?} hexpands={} parent_measure={parent_measure:?}",
                    label.text(),
                    label.is_mapped(),
                    label.layout().is_ellipsized(),
                    text_width,
                    natural,
                    allocation,
                    parent_width,
                    label.max_width_chars(),
                    label.width_chars(),
                    label.width_request(),
                    label.halign(),
                    label.hexpands()
                );
                assert!(
                    !label.layout().is_ellipsized(),
                    "tab label is not ellipsized: {:?}",
                    label.text()
                );
                assert!(
                    label.is_mapped() && label.width() > 0,
                    "tab label is mapped with an allocation: {:?}",
                    label.text()
                );
                assert!(
                    text_width > 0 && text_width <= allocation && allocation <= parent_width,
                    "tab label full text fits its mapped allocation: {:?} text {} allocation {} parent {}",
                    label.text(),
                    text_width,
                    allocation,
                    parent_width
                );
                seen_labels.push(label.text().to_string());
            }
            assert!(
                seen_labels[before..].iter().any(|label| label == expected),
                "tab {tab_index} must show {expected:?}; saw {:?}",
                &seen_labels[before..]
            );
        }

        let measure_mapped_toolbars = |what: &str| {
            pump_until(
                &|| {
                    let mut found = Vec::new();
                    toolbars(prefs.upcast_ref(), &mut found);
                    found
                        .iter()
                        .any(|toolbar| toolbar.is_mapped() && toolbar.width() > 0)
                },
                what,
            );
            let mut found = Vec::new();
            toolbars(prefs.upcast_ref(), &mut found);
            for toolbar in found {
                if !toolbar.is_mapped() || toolbar.width() <= 0 {
                    continue;
                }
                let minimum = toolbar.measure(gtk::Orientation::Horizontal, -1).0;
                eprintln!(
                    "{what}: toolbar min={minimum} allocation={} mapped={}",
                    toolbar.width(),
                    toolbar.is_mapped()
                );
                assert!(
                    minimum <= toolbar.width(),
                    "toolbar minimum {minimum} exceeds allocation {}",
                    toolbar.width()
                );
            }
        };
        measure_mapped_toolbars("Preferences toolbar allocation");
        prefs.destroy();
    }

    #[test]
    fn plugin_states_are_described_for_the_user() {
        assert!(describe_state(&PluginState::Running).contains("Running"));
        assert!(describe_state(&PluginState::Disabled).contains("Disabled"));
        assert!(describe_state(&PluginState::Missing { reason: "x".into() }).contains("x"));
        let incompatible = describe_state(&PluginState::Unavailable {
            reason: "managed API minimum 9.1 is unsupported".into(),
        });
        assert!(incompatible.contains('9') && incompatible.contains('1'));
        assert!(describe_state(&PluginState::Faulted {
            reason: "crashed".into()
        })
        .contains("crashed"));
    }

    #[test]
    #[ignore = "requires an isolated GTK display"]
    fn plugin_row_observes_toggles_and_async_faults_without_replacing_focus_widget() {
        adw::init().expect("isolated GTK display");
        let state = Rc::new(RefCell::new(PluginState::Running));
        let observed = state.clone();
        let enabled = state.clone();
        let disabled = state.clone();
        let desired = Rc::new(std::cell::Cell::new(true));
        let page = plugins_page(&PreferencesContext {
            general: None,
            package_action: Rc::new(|_, _, _| {}),
            retained_settings: Rc::new(|| Ok(Vec::new())),
            delete_retained_settings: Rc::new(|_| {
                Err("no retained settings in this fixture".into())
            }),
            installations_changed: Rc::new(|| {}),
            on_closed: Rc::new(|| {}),
            storage_status: gtk::StringList::new(&[]),
            bindings: Rc::new(RefCell::new(Bindings::defaults())),
            plugin_inventory: Rc::new({
                let desired = desired.clone();
                move || {
                    vec![crate::plugin::host::Overview::fixture(
                        "clock",
                        observed.borrow().clone(),
                        desired.get(),
                    )]
                }
            }),
            plugin_details: Rc::new(|_| None),
            save_plugin_settings: Rc::new(|_, _, _| Err("no settings in this fixture".into())),
            plugin_configuration: Rc::new(|_| Configuration::NotApplied),
            restart_plugin: Rc::new(|_, _, _| {}),
            enable_plugin: Rc::new({
                let desired = desired.clone();
                move |_, _| {
                    desired.set(true);
                    *enabled.borrow_mut() = PluginState::Running;
                }
            }),
            disable_plugin: Rc::new(move |_| {
                desired.set(false);
                *disabled.borrow_mut() = PluginState::Disabled;
            }),
            allow_reduced_protection: Rc::new(|_, _| Err("no kernel in this fixture".into())),
            bindings_changed: Rc::new(|| {}),
            terminal: crate::prefs::terminal::test_context(),
        });
        fn find<T: IsA<gtk::Widget> + gtk::glib::object::IsClass>(
            widget: &gtk::Widget,
        ) -> Option<T> {
            if let Ok(found) = widget.clone().downcast::<T>() {
                return Some(found);
            }
            let mut child = widget.first_child();
            while let Some(widget) = child {
                if let Some(found) = find::<T>(&widget) {
                    return Some(found);
                }
                child = widget.next_sibling();
            }
            None
        }
        let switch = find::<gtk::Switch>(page.upcast_ref()).expect("native plugin switch");
        let row = switch
            .ancestor(adw::ActionRow::static_type())
            .and_then(|widget| widget.downcast::<adw::ActionRow>().ok())
            .expect("live plugin row containing the switch");
        assert!(row
            .subtitle()
            .is_some_and(|text| text.contains("Running") && text.contains("Local test fixture")));
        switch.set_active(false);
        assert!(row.subtitle().is_some_and(|text| text.contains("Disabled")));
        switch.set_active(true);
        assert!(row.subtitle().is_some_and(|text| text.contains("Running")));
        // force inventory refresh throughout a real keyboard animation. an
        // unchanged set_active(true) used to cancel Space before notify fired.
        gtk::Settings::default()
            .unwrap()
            .set_gtk_enable_animations(true);
        let window = gtk::Window::builder()
            .title("Among plugin refresh regression")
            .child(&page)
            .build();
        window.present();
        let pump_until = |condition: &dyn Fn() -> bool| {
            let limit = std::time::Instant::now() + std::time::Duration::from_secs(3);
            while !condition() {
                assert!(std::time::Instant::now() < limit);
                glib::MainContext::default().iteration(false);
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
        };
        pump_until(&|| switch.is_mapped());
        let xdo = |args: &[&str]| {
            let output = std::process::Command::new("xdotool")
                .args(args)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            String::from_utf8(output.stdout).unwrap()
        };
        let ids = xdo(&[
            "search",
            "--onlyvisible",
            "--pid",
            &std::process::id().to_string(),
            "--name",
            "^Among plugin refresh regression$",
        ]);
        let id = ids.lines().last().unwrap();
        xdo(&["windowactivate", "--sync", id]);
        xdo(&["windowfocus", "--sync", id]);
        assert!(switch.grab_focus());
        pump_until(&|| switch.has_focus());
        xdo(&["key", "--clearmodifiers", "space"]);
        let search = find::<gtk::SearchEntry>(page.upcast_ref()).unwrap();
        let until = std::time::Instant::now() + std::time::Duration::from_millis(400);
        while std::time::Instant::now() < until {
            while glib::MainContext::default().pending() {
                glib::MainContext::default().iteration(false);
            }
            search.emit_by_name::<()>("search-changed", &[]);
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(
            !switch.is_active(),
            "refresh must not cancel a real pending keyboard toggle"
        );
        assert!(row.subtitle().is_some_and(|text| text.contains("Disabled")));
        println!("PASS real Space completes despite overlapping inventory refreshes during GTK animation");
        window.set_child(None::<&gtk::Widget>);
        window.destroy();
        drop(window);
        drop(search);
        switch.set_active(true);
        *state.borrow_mut() = PluginState::Faulted {
            reason: "fixture fault".into(),
        };
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        while !row
            .subtitle()
            .is_some_and(|text| text.contains("fixture fault"))
        {
            assert!(
                std::time::Instant::now() < deadline,
                "fault description never reached the real row"
            );
            gtk::glib::MainContext::default().iteration(false);
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(
            switch.is_active(),
            "actual process fault must not change desired enablement"
        );
        *state.borrow_mut() = PluginState::Running;
        while !row.subtitle().is_some_and(|text| text.contains("Running")) {
            assert!(
                std::time::Instant::now() < deadline,
                "restart description never reached the real row"
            );
            gtk::glib::MainContext::default().iteration(false);
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert_eq!(row.activatable_widget(), Some(switch.clone().upcast()));
        let weak = row.downgrade();
        drop(switch);
        drop(row);
        drop(page);
        assert!(
            weak.upgrade().is_none(),
            "observer must not keep Preferences rows alive"
        );
    }

    #[test]
    fn preferences_parses_from_its_name() {
        assert_eq!(
            shortcuts::parse_action("preferences"),
            Some(Action::OpenPreferences)
        );
    }
}
