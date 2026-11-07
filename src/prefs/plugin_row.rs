//! one plugin row: icon, name, one-line description, shield, info control,
//! settings control when settings exist, the switch for installed plugins and
//! a single state slot. rows retain their widgets while observations change.
//! package-generation replacement invalidates old rows instead of retargeting
//! their controls.

use super::plugin_manager::{Env, Op};
use super::trust::{self, Shield};
use super::*;
use crate::plugin::host::Overview;
use crate::plugin::install::listing::{InstalledPlugin, Link, Offer, Standing};
use crate::plugin::inventory::Entry;
use std::cell::Cell;

/// What a row shows besides its fixed controls, from the current listing.
pub(super) enum Shown<'a> {
    Installed {
        info: &'a Overview,
        entry: Option<&'a Entry>,
        plugin: Option<&'a InstalledPlugin>,
        several_sources: bool,
    },
    Available {
        offer: &'a Offer,
        several_sources: bool,
    },
}

/// The one control that sits at the end of a row. Install and Uninstall are
/// never shown together; an operation in progress replaces whichever applies.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Slot {
    Install,
    Update,
    Replace,
    Uninstall,
    Busy { can_stop: bool },
    Empty,
}

impl Slot {
    /// In priority order: operation in progress, update, install, uninstall.
    pub(super) fn choose(shown: &Shown<'_>, op: Option<&Op>) -> Self {
        if let Some(Op::Running { can_stop, .. }) = op {
            return Self::Busy {
                can_stop: *can_stop,
            };
        }
        match shown {
            Shown::Available { .. } => Self::Install,
            Shown::Installed { entry: None, .. } => Self::Empty,
            Shown::Installed {
                plugin: Some(plugin),
                ..
            } => match plugin.standing {
                Standing::Update(_) => Self::Update,
                Standing::Replace(_) => Self::Replace,
                _ => Self::Uninstall,
            },
            Shown::Installed { plugin: None, .. } => Self::Uninstall,
        }
    }

    fn page(self) -> &'static str {
        match self {
            Self::Install => "install",
            Self::Update | Self::Replace => "update",
            Self::Uninstall => "uninstall",
            Self::Busy { .. } => "busy",
            Self::Empty => "empty",
        }
    }
}

/// The widgets of the state slot, shared by a row and the plugin page.
#[derive(Clone)]
pub(super) struct SlotWidgets {
    pub stack: gtk::Stack,
    pub install: gtk::Button,
    pub update: gtk::Button,
    pub uninstall: gtk::Button,
    pub stop: gtk::Button,
    spinner: gtk::Spinner,
}

impl SlotWidgets {
    pub(super) fn new() -> Self {
        let stack = gtk::Stack::new();
        stack.set_valign(gtk::Align::Center);
        stack.set_hhomogeneous(false);
        let install = gtk::Button::with_label("Install");
        install.add_css_class("suggested-action");
        let update = gtk::Button::with_label("Update");
        update.add_css_class("suggested-action");
        let uninstall = gtk::Button::with_label("Uninstall");
        let busy = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        let spinner = gtk::Spinner::new();
        let stop = gtk::Button::from_icon_name("process-stop-symbolic");
        stop.set_tooltip_text(Some("Stop"));
        busy.append(&spinner);
        busy.append(&stop);
        stack.add_named(&install, Some("install"));
        stack.add_named(&update, Some("update"));
        stack.add_named(&uninstall, Some("uninstall"));
        stack.add_named(&busy, Some("busy"));
        stack.add_named(
            &gtk::Box::new(gtk::Orientation::Horizontal, 0),
            Some("empty"),
        );
        Self {
            stack,
            install,
            update,
            uninstall,
            stop,
            spinner,
        }
    }

    pub(super) fn show(&self, slot: Slot, name: &str) {
        self.stack.set_visible_child_name(slot.page());
        let busy = matches!(slot, Slot::Busy { .. });
        self.spinner.set_spinning(busy);
        if let Slot::Busy { can_stop } = slot {
            // an uninstall cannot be cancelled once begun, so no stop is offered.
            self.stop.set_visible(can_stop);
        }
        self.update.set_label(if slot == Slot::Replace {
            "Replace"
        } else {
            "Update"
        });
        for (button, verb) in [
            (&self.install, "Install"),
            (
                &self.update,
                if slot == Slot::Replace {
                    "Replace"
                } else {
                    "Update"
                },
            ),
            (&self.uninstall, "Uninstall"),
            (&self.stop, "Stop installing"),
        ] {
            button.update_property(&[gtk::accessible::Property::Label(&format!("{verb} {name}"))]);
        }
    }
}

fn icon_button(icon: &str, tooltip: &str, name: &str) -> gtk::Button {
    let button = gtk::Button::from_icon_name(icon);
    button.add_css_class("flat");
    button.set_valign(gtk::Align::Center);
    button.set_tooltip_text(Some(tooltip));
    button.set_widget_name(name);
    button
}

/// The version and, for a package not installed yet, its download size.
fn version_text(version: Option<&str>, size: Option<u64>) -> String {
    let mut details = Vec::new();
    if let Some(version) = version {
        details.push(version.to_owned());
    }
    if let Some(size) = size {
        details.push(glib::format_size(size).to_string());
    }
    details.join(" · ")
}

/// The unsteady state of an installed plugin as one line, or `None` while it
/// is running or simply switched off. A refusal reason is kept even for a
/// disabled plugin so it is not lost.
pub(super) fn status_line(info: &Overview) -> Option<String> {
    let state = match &info.state {
        PluginState::Running | PluginState::Disabled => None,
        other => Some(describe_state(other)),
    };
    state
        .or_else(|| (!info.available).then(|| format!("Files unavailable: {}", info.declared)))
        .or_else(|| {
            info.declared
                .starts_with("Declared API unavailable:")
                .then(|| info.declared.clone())
        })
}

// gtk owns the widgets; the row index must not prolong a closed page's life.
pub(super) struct Row {
    pub row: adw::ActionRow,
    icon_holder: gtk::Box,
    warning: gtk::Image,
    version: gtk::Label,
    source: gtk::Label,
    shield: gtk::Button,
    info: gtk::Button,
    gear: gtk::Button,
    allow: gtk::Button,
    switch: gtk::Switch,
    slot: SlotWidgets,
    updating: Rc<Cell<bool>>,
    generation: Option<String>,
    installed: bool,
}

impl Row {
    pub(super) fn matches_generation(&self, generation: &Option<String>) -> bool {
        self.generation == *generation
    }

    pub(super) fn new(env: &Rc<Env>, key: &str, shown: &Shown<'_>) -> Self {
        let row = adw::ActionRow::new();
        // the identity names the row, so a view or a test can tell two plugins
        // with the same display name apart.
        row.set_widget_name(key);
        row.set_title_lines(1);
        row.set_subtitle_lines(1);
        let icon_holder = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        row.add_prefix(&icon_holder);
        let warning = gtk::Image::from_icon_name("dialog-warning-symbolic");
        warning.set_visible(false);
        let version = gtk::Label::new(None);
        version.add_css_class("caption");
        version.add_css_class("dim-label");
        let source = gtk::Label::new(None);
        source.add_css_class("caption");
        source.add_css_class("dim-label");
        source.set_visible(false);
        let shield = icon_button("security-high-symbolic", "", "plugin-shield");
        let info = icon_button("help-about-symbolic", "Plugin details", "plugin-info");
        let gear = icon_button(
            "preferences-system-symbolic",
            "Plugin settings",
            "plugin-settings",
        );
        gear.set_visible(false);
        let allow = gtk::Button::with_label("Allow reduced protection…");
        allow.set_valign(gtk::Align::Center);
        allow.set_visible(false);
        let switch = gtk::Switch::new();
        switch.set_valign(gtk::Align::Center);
        let slot = SlotWidgets::new();
        for widget in [
            warning.upcast_ref::<gtk::Widget>(),
            version.upcast_ref(),
            source.upcast_ref(),
            allow.upcast_ref(),
            shield.upcast_ref(),
            info.upcast_ref(),
            gear.upcast_ref(),
            switch.upcast_ref(),
            slot.stack.upcast_ref(),
        ] {
            row.add_suffix(widget);
        }
        let installed = matches!(shown, Shown::Installed { .. });
        switch.set_visible(installed);
        let generation = match shown {
            Shown::Installed { info, .. } => info.generation.clone(),
            Shown::Available { .. } => None,
        };
        let built = Self {
            row,
            icon_holder,
            warning,
            version,
            source,
            shield,
            info,
            gear,
            allow,
            switch,
            slot,
            updating: Rc::new(Cell::new(false)),
            generation,
            installed,
        };
        let expected = match shown {
            Shown::Installed { entry, .. } => entry.map(|entry| entry.receipt.clone()),
            Shown::Available { .. } => None,
        };
        built.wire(env, key, expected);
        built
    }

    fn wire(&self, env: &Rc<Env>, key: &str, expected: Option<crate::plugin::inventory::Receipt>) {
        let is_current: Rc<dyn Fn() -> bool> = Rc::new({
            let env = env.clone();
            let key = key.to_owned();
            let generation = self.generation.clone();
            let installed = self.installed;
            move || {
                if let Some(expected) = &expected {
                    return (env.ctx.plugin_details)(&key)
                        .is_some_and(|(entry, _)| &entry.receipt == expected);
                }
                match (env.ctx.plugin_inventory)()
                    .into_iter()
                    .find(|info| info.key == key)
                {
                    Some(info) => installed && info.generation == generation,
                    None => !installed,
                }
            }
        });
        let window_of = |button: &gtk::Button| {
            button
                .root()
                .and_then(|root| root.downcast::<adw::PreferencesWindow>().ok())
        };
        self.info.connect_clicked({
            let env = env.clone();
            let key = key.to_owned();
            let is_current = is_current.clone();
            move |button| {
                if !is_current() {
                    return;
                }
                if let Some(window) = window_of(button) {
                    super::plugin_page::open(&env, &key, &window);
                }
            }
        });
        self.gear.connect_clicked({
            let env = env.clone();
            let key = key.to_owned();
            let is_current = is_current.clone();
            move |button| {
                if !is_current() {
                    return;
                }
                if let Some(window) = window_of(button) {
                    super::plugin_page::open_settings(&env, &key, &window);
                }
            }
        });
        self.slot.install.connect_clicked({
            let env = env.clone();
            let key = key.to_owned();
            let is_current = is_current.clone();
            move |_| {
                if is_current() {
                    super::plugin_manager::install(&env, &key)
                }
            }
        });
        self.slot.update.connect_clicked({
            let env = env.clone();
            let key = key.to_owned();
            let is_current = is_current.clone();
            move |button| {
                if is_current() {
                    super::plugin_manager::update(&env, &key, window_of(button))
                }
            }
        });
        self.slot.uninstall.connect_clicked({
            let env = env.clone();
            let key = key.to_owned();
            let is_current = is_current.clone();
            move |button| {
                if !is_current() {
                    return;
                }
                super::plugin_manager::uninstall(
                    &env,
                    &key,
                    button
                        .root()
                        .and_then(|root| root.downcast::<gtk::Window>().ok()),
                )
            }
        });
        self.slot.stop.connect_clicked({
            let env = env.clone();
            let key = key.to_owned();
            let is_current = is_current.clone();
            move |_| {
                if is_current() {
                    super::plugin_manager::stop(&env, &key)
                }
            }
        });
        self.allow.connect_clicked({
            let env = env.clone();
            let key = key.to_owned();
            let is_current = is_current.clone();
            move |button| {
                if is_current() {
                    super::plugin_manager::allow_reduced(&env, &key, button)
                }
            }
        });
        let signal = self.updating.clone();
        let weak_row = self.row.downgrade();
        let generation = self.generation.clone();
        let env = env.clone();
        let key = key.to_owned();
        self.switch.connect_active_notify(move |switch| {
            if signal.get() {
                return;
            }
            let current = (env.ctx.plugin_inventory)()
                .into_iter()
                .find(|info| info.key == key);
            if current
                .as_ref()
                .is_none_or(|info| info.generation != generation)
            {
                if let Some(row) = weak_row.upgrade() {
                    row.set_subtitle("Plugin changed or was removed; reopen its current controls.");
                }
                return;
            }
            env.clear_note(&key);
            if switch.is_active() {
                (env.ctx.enable_plugin)(
                    &key,
                    switch
                        .root()
                        .and_then(|root| root.downcast::<gtk::Window>().ok()),
                );
            } else {
                (env.ctx.disable_plugin)(&key);
            }
        });
    }

    /// Brings the row in line with what is known now. Cheap: it only touches
    /// properties whose value changed.
    pub(super) fn refresh(&self, env: &Env, key: &str, shown: &Shown<'_>, query: &str) {
        let op = env.state.op(key);
        let (name, version, description, status, origin, features, weaker, several) = match shown {
            Shown::Installed {
                info,
                entry,
                several_sources,
                ..
            } => {
                let features = entry
                    .and_then(|entry| entry.inspected.as_ref().ok())
                    .map(|inspected| inspected.manifest.required_features.clone())
                    .unwrap_or_default();
                (
                    info.name.clone(),
                    info.version.clone(),
                    info.description.clone(),
                    status_line(info),
                    entry.map(trust::Origin::of_installed),
                    features,
                    info.weaker_protection,
                    *several_sources,
                )
            }
            Shown::Available {
                offer,
                several_sources,
            } => (
                offer.name.clone(),
                Some(offer.version.clone()),
                offer.description.clone(),
                None,
                Some(trust::Origin::of_offer(offer)),
                offer.required_features.clone(),
                false,
                *several_sources,
            ),
        };
        let size = match shown {
            Shown::Available { offer, .. } => offer.size,
            Shown::Installed { .. } => None,
        };
        self.row.set_title(&escape_display(&name));
        self.version
            .set_text(&version_text(version.as_deref(), size));
        // an operation's own line wins over the plugin's steady description.
        let (line, problem) = match &op {
            Some(Op::Running { phase, .. }) => (phase.clone(), false),
            Some(Op::Failed(text) | Op::Uncertain(text)) => (text.clone(), true),
            None | Some(Op::Done(_)) => match &status {
                Some(text) => (text.clone(), true),
                None => (description, false),
            },
        };
        self.row.set_subtitle(&escape_display(&line));
        self.row.set_tooltip_text(Some(&line));
        self.warning.set_visible(problem);
        self.set_icon(shown);
        // source label: named only while several sources exist, and always
        // for a plugin whose source was removed.
        let removed = matches!(
            shown,
            Shown::Installed {
                plugin: Some(plugin),
                ..
            } if matches!(plugin.link, Link::SourceRemoved { .. })
        );
        let label = match shown {
            _ if removed => Some("source removed".to_owned()),
            Shown::Available { offer, .. } if several && !offer.official => Some(format!(
                "{}/{}",
                offer.source.owner, offer.source.repository
            )),
            Shown::Installed {
                plugin:
                    Some(InstalledPlugin {
                        link: Link::Source(origin),
                        ..
                    }),
                ..
            } if several
                && !crate::plugin::install::listing::is_official(
                    origin.repository_id,
                    Some(origin.publisher_id),
                ) =>
            {
                Some(format!("{}/{}", origin.owner, origin.repository))
            }
            _ => None,
        };
        self.source.set_visible(label.is_some());
        self.source.set_text(label.as_deref().unwrap_or(""));
        if let Some(origin) = origin {
            let shield = Shield::of(origin, weaker);
            self.shield.set_icon_name(shield.icon_name());
            let tooltip = shield.tooltip(&features);
            self.shield.set_tooltip_text(Some(&tooltip));
            self.shield.update_property(&[
                gtk::accessible::Property::Label(shield.title()),
                gtk::accessible::Property::Description(&tooltip),
            ]);
        }
        self.shield.set_visible(origin.is_some());
        self.info
            .update_property(&[gtk::accessible::Property::Label(&format!(
                "Details of {name}"
            ))]);
        let has_settings = matches!(
            shown,
            Shown::Installed { entry: Some(entry), .. }
                if entry.inspected.as_ref().is_ok_and(|inspected| inspected.manifest.settings.is_some())
        );
        self.gear.set_visible(has_settings);
        self.gear
            .update_property(&[gtk::accessible::Property::Label(&format!(
                "Settings of {name}"
            ))]);
        if let Shown::Installed { info, .. } = shown {
            self.row.set_visible(info.search_text().contains(query));
            self.switch
                .update_property(&[gtk::accessible::Property::Label(&format!(
                    "Enable {} ({})",
                    info.name, info.key
                ))]);
            self.switch.set_sensitive(info.enabled || info.available);
            self.allow.set_visible(info.consent_offer.is_some());
            // even setting the same active value cancels GTK's keyboard-toggle
            // animation before its active-notify can commit the user's choice.
            if self.switch.is_active() != info.enabled {
                self.updating.set(true);
                self.switch.set_active(info.enabled);
                self.updating.set(false);
            }
        } else if let Shown::Available { offer, .. } = shown {
            let text = format!(
                "{} {} {} {}",
                offer.name, offer.description, offer.plugin_id, offer.source.owner
            )
            .to_lowercase();
            self.row.set_visible(text.contains(query));
        }
        let slot = Slot::choose(shown, op.as_ref());
        self.slot.show(slot, &name);
        if let Shown::Available { offer, .. } = shown {
            match &offer.selection {
                Ok(_) => {
                    self.slot.install.set_sensitive(true);
                    self.slot.install.set_tooltip_text(None);
                }
                Err(problem) => {
                    self.slot.install.set_sensitive(false);
                    self.slot.install.set_tooltip_text(Some(problem.0));
                }
            }
        }
        debug_assert!(self.installed == matches!(shown, Shown::Installed { .. }));
    }

    fn set_icon(&self, shown: &Shown<'_>) {
        // the icon is rebuilt only once: artwork does not change while a row lives.
        if self.icon_holder.first_child().is_some() {
            return;
        }
        let image = match shown {
            Shown::Installed {
                entry: Some(entry), ..
            } => plugin_settings::entry_icon(entry, 32),
            Shown::Available { offer, .. } => match &offer.icon {
                Some(crate::plugin::manifest::Icon::Theme(name)) => {
                    let image = gtk::Image::from_icon_name(name);
                    image.set_pixel_size(32);
                    image
                }
                _ => {
                    let image = gtk::Image::from_icon_name("application-x-addon-symbolic");
                    image.set_pixel_size(32);
                    image
                }
            },
            Shown::Installed { entry: None, .. } => {
                let image = gtk::Image::from_icon_name("application-x-addon-symbolic");
                image.set_pixel_size(32);
                image
            }
        };
        self.icon_holder.append(&image);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugin::install::listing::Standing;
    use crate::plugin::origin::Origin;

    fn offer() -> Offer {
        Offer {
            source: Origin {
                repository_id: 1,
                publisher_id: 2,
                owner: "acme".into(),
                repository: "plugins".into(),
            },
            official: false,
            plugin_id: "notes".into(),
            name: "Notes".into(),
            description: String::new(),
            version: "1.0.0".into(),
            icon: None,
            required_features: Vec::new(),
            size: None,
            selection: Err(crate::plugin::data::Problem("not for this platform")),
        }
    }

    fn installed(standing: Standing) -> InstalledPlugin {
        InstalledPlugin {
            key: "github:1/notes".into(),
            link: Link::Local,
            standing,
        }
    }

    #[test]
    fn the_slot_shows_one_control_and_an_operation_outranks_the_rest() {
        let info = Overview::fixture("github:1/notes", PluginState::Disabled, false);
        let offer = offer();
        let running = Op::Running {
            phase: "Downloading".into(),
            can_stop: true,
        };
        let available = Shown::Available {
            offer: &offer,
            several_sources: false,
        };
        assert_eq!(Slot::choose(&available, None), Slot::Install);
        assert_eq!(
            Slot::choose(&available, Some(&running)),
            Slot::Busy { can_stop: true }
        );
        let plugin = installed(Standing::Current);
        let steady = Shown::Installed {
            info: &info,
            entry: None,
            plugin: Some(&plugin),
            several_sources: false,
        };
        // a plugin without package files has nothing to install or remove.
        assert_eq!(Slot::choose(&steady, None), Slot::Empty);
        let uninstall_only = Shown::Installed {
            info: &info,
            entry: None,
            plugin: None,
            several_sources: false,
        };
        assert_eq!(Slot::choose(&uninstall_only, None), Slot::Empty);
        let failed = Op::Failed("refused".into());
        assert_eq!(Slot::choose(&available, Some(&failed)), Slot::Install);
        // an uninstall cannot be cancelled once begun: it shows no stop.
        let removing = Op::Running {
            phase: "Removing files".into(),
            can_stop: false,
        };
        assert_eq!(
            Slot::choose(&available, Some(&removing)),
            Slot::Busy { can_stop: false }
        );
    }
}
