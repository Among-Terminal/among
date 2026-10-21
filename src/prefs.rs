//! The preferences window: keyboard shortcuts and plugins.
//!
//! Both features have to be inspectable and changeable *by the user*, not just by
//! editing configuration. Putting them in one window is also what makes the
//! capability discoverable: a binding registry that can only be read from a
//! debug log is not user-facing in any meaningful sense.

use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;
use gtk::gdk;

use crate::plugin::host::PluginState;
use crate::shortcuts::{self, Action, Bindings};

mod plugin_settings;

type ManagedDetails = (crate::plugin::inventory::Entry, Option<serde_json::Value>);
type PluginDetailsQuery = dyn Fn(&str) -> Option<ManagedDetails>;
type SavePluginSettings =
    dyn Fn(&str, &str, &serde_json::Map<String, serde_json::Value>) -> Result<(), String>;

/// What the preferences window needs from the running application.
///
/// Handed over as closures rather than a reference to the window so this module
/// does not need to know how the application stores its state.
pub struct PreferencesContext {
    pub on_closed: Rc<dyn Fn()>,
    pub storage_status: gtk::StringList,
    pub bindings: Rc<RefCell<Bindings>>,
    pub plugin_listing: Rc<dyn Fn() -> Vec<(String, PluginState)>>,
    pub plugin_enabled: Rc<dyn Fn(&str) -> bool>,
    pub plugin_details: Rc<PluginDetailsQuery>,
    pub save_plugin_settings: Rc<SavePluginSettings>,
    pub enable_plugin: Rc<dyn Fn(&str)>,
    pub disable_plugin: Rc<dyn Fn(&str)>,
    /// Rebuilds the window's shortcut controller after a rebind.
    pub bindings_changed: Rc<dyn Fn()>,
}

/// Open the preferences window, parented to the main window.
pub fn present(parent: &impl IsA<gtk::Window>, ctx: PreferencesContext) {
    let window = adw::PreferencesWindow::builder()
        .transient_for(parent)
        .search_enabled(false)
        .build();
    window.set_title(Some("Preferences"));
    let on_closed = ctx.on_closed.clone();
    window.connect_destroy(move |_| on_closed());

    window.add(&shortcuts_page(&ctx));
    window.add(&plugins_page(&ctx));
    window.present();
}

fn shortcuts_page(ctx: &PreferencesContext) -> adw::PreferencesPage {
    let page = adw::PreferencesPage::new();
    page.set_title("Keyboard");
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
    let page = adw::PreferencesPage::new();
    page.set_title("Plugins");
    page.set_icon_name(Some("application-x-addon-symbolic"));
    add_storage_status(&page, ctx);

    let group = adw::PreferencesGroup::new();
    group.set_title("Plugins");
    group.set_description(Some(
        "Plugins run as separate processes, so one that crashes cannot take the \
         terminals down with it.",
    ));
    page.add(&group);

    let listing = (ctx.plugin_listing)();
    if listing.is_empty() {
        let row = adw::ActionRow::new();
        row.set_title("No plugins configured");
        row.set_subtitle("Add an entry to ~/.config/among/plugins.json");
        group.add(&row);
        return page;
    }

    for (name, state) in listing {
        let row = adw::ActionRow::new();
        let details = (ctx.plugin_details)(&name);
        row.set_title(&escape_display(
            details
                .as_ref()
                .and_then(|(entry, _)| entry.inspected.as_ref().ok())
                .map_or(name.as_str(), |inspected| inspected.manifest.name.as_str()),
        ));
        row.set_subtitle(&describe_state(&state));
        if let Some((entry, _)) = details {
            plugin_settings::add_actions(
                &row,
                entry,
                ctx.save_plugin_settings.clone(),
                ctx.plugin_details.clone(),
            );
        }

        let switch = gtk::Switch::new();
        switch.set_valign(gtk::Align::Center);
        switch.set_active((ctx.plugin_enabled)(&name));
        // A plugin that is not installed cannot be switched on: there is
        // nothing to start, and pretending otherwise would be misleading.
        if matches!(
            state,
            PluginState::Missing { .. }
                | PluginState::Incompatible { .. }
                | PluginState::Unavailable { .. }
        ) && !switch.is_active()
        {
            switch.set_sensitive(false);
        }

        let enable = ctx.enable_plugin.clone();
        let disable = ctx.disable_plugin.clone();
        let plugin_name = name.clone();
        let listing = ctx.plugin_listing.clone();
        let weak_row = row.downgrade();
        switch.connect_active_notify(move |switch| {
            if switch.is_active() {
                enable(&plugin_name);
            } else {
                disable(&plugin_name);
            }
            if let Some(row) = weak_row.upgrade() {
                if let Some((_, state)) =
                    listing().into_iter().find(|(name, _)| name == &plugin_name)
                {
                    row.set_subtitle(&describe_state(&state));
                }
            }
        });
        // faults and restarts can happen without a switch interaction; stop
        // observing when the preferences row is destroyed.
        let weak_row = row.downgrade();
        let listing = ctx.plugin_listing.clone();
        gtk::glib::timeout_add_local(std::time::Duration::from_millis(500), move || {
            let Some(row) = weak_row.upgrade() else {
                return gtk::glib::ControlFlow::Break;
            };
            if let Some((_, state)) = listing().into_iter().find(|(plugin, _)| plugin == &name) {
                let text = describe_state(&state);
                if row.subtitle().as_deref() != Some(text.as_str()) {
                    row.set_subtitle(&text);
                }
            }
            gtk::glib::ControlFlow::Continue
        });
        row.add_suffix(&switch);
        row.set_activatable_widget(Some(&switch));
        group.add(&row);
    }
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
        PluginState::Incompatible {
            plugin_major,
            host_major,
        } => format!(
            "Incompatible: the plugin speaks protocol major {plugin_major}, this build speaks {host_major}"
        ),
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
    fn plugin_states_are_described_for_the_user() {
        assert!(describe_state(&PluginState::Running).contains("Running"));
        assert!(describe_state(&PluginState::Disabled).contains("Disabled"));
        assert!(describe_state(&PluginState::Missing { reason: "x".into() }).contains("x"));
        let incompatible = describe_state(&PluginState::Incompatible {
            plugin_major: 9,
            host_major: 1,
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
        let page = plugins_page(&PreferencesContext {
            on_closed: Rc::new(|| {}),
            storage_status: gtk::StringList::new(&[]),
            bindings: Rc::new(RefCell::new(Bindings::defaults())),
            plugin_listing: Rc::new(move || vec![("clock".into(), observed.borrow().clone())]),
            plugin_enabled: Rc::new(|_| true),
            plugin_details: Rc::new(|_| None),
            save_plugin_settings: Rc::new(|_, _, _| Err("no settings in this fixture".into())),
            enable_plugin: Rc::new(move |_| *enabled.borrow_mut() = PluginState::Running),
            disable_plugin: Rc::new(move |_| *disabled.borrow_mut() = PluginState::Disabled),
            bindings_changed: Rc::new(|| {}),
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
        assert_eq!(row.subtitle().as_deref(), Some("Running"));
        switch.set_active(false);
        assert_eq!(row.subtitle().as_deref(), Some("Disabled"));
        switch.set_active(true);
        assert_eq!(row.subtitle().as_deref(), Some("Running"));
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
        while row.subtitle().as_deref() != Some("Running") {
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
