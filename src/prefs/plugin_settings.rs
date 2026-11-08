//! edits are committed explicitly, never by initial control signals. schema
//! defaults remain in memory until the user presses Save.

use super::*;
use crate::plugin::inventory::Entry;
use crate::plugin::schema::{Apply, Kind, SettingsSchema};
use serde_json::{Map, Value};

type ReadValue = Box<dyn Fn() -> Result<Option<Value>, String>>;

/// The artwork of an installed plugin: its packaged raster icon, a theme icon
/// it names, or the generic add-on icon.
pub(super) fn entry_icon(entry: &Entry, pixel_size: i32) -> gtk::Image {
    let icon = match entry.inspected.as_ref() {
        Ok(inspected) => match (&inspected.manifest.icon, &inspected.raster_icon) {
            (_, Some(bytes)) => {
                let loader = gdk_pixbuf::PixbufLoader::with_type("png").ok();
                loader.and_then(|loader| {
                    use gdk_pixbuf::prelude::*;
                    let written = loader.write(bytes);
                    let closed = loader.close();
                    if written.is_err() || closed.is_err() {
                        return None;
                    }
                    loader.pixbuf().map(|image| {
                        gtk::Image::from_paintable(Some(&gdk::Texture::for_pixbuf(&image)))
                    })
                })
            }
            (Some(crate::plugin::manifest::Icon::Theme(name)), _) => {
                Some(gtk::Image::from_icon_name(name))
            }
            _ => None,
        },
        Err(_) => None,
    }
    .unwrap_or_else(|| gtk::Image::from_icon_name("application-x-addon-symbolic"));
    icon.set_pixel_size(pixel_size);
    icon
}

/// What the settings form needs from the application.
pub(super) struct Form {
    pub save: Rc<SavePluginSettings>,
    pub load: Rc<PluginDetailsQuery>,
    pub status: Rc<ConfigurationQuery>,
    pub restart: Rc<RestartPlugin>,
}

/// The settings form as a subpage. Nothing is written until Save; going back
/// leaves the page and with it every unsaved edit.
pub(super) fn settings_subpage(
    entry: Entry,
    stored: Option<Value>,
    schema: SettingsSchema,
    name: String,
    form: Form,
) -> adw::NavigationPage {
    let Form {
        save,
        load,
        status,
        restart,
    } = form;
    let page = adw::PreferencesPage::new();
    let group = adw::PreferencesGroup::new();
    group.set_title(&escape_display(&format!("{name} settings")));
    group.set_description(Some(match schema.apply {
        Apply::Live => "Use Save to store changes locally. Going back discards unsaved edits.",
        Apply::Restart => {
            "Use Save to store changes locally, then restart the plugin to apply them."
        }
    }));
    let commit = gtk::Button::with_label("Save");
    commit.add_css_class("suggested-action");
    commit.set_sensitive(schema.supported());
    commit.set_valign(gtk::Align::Start);
    group.set_header_suffix(Some(&commit));
    let mut editors: Vec<(String, ReadValue)> = Vec::new();
    for field in &schema.fields {
        let value = stored
            .as_ref()
            .and_then(|stored| stored.get("values"))
            .and_then(|values| values.get(&field.key))
            .or(field.default.as_ref());
        let unset = value.is_none() && !field.required;
        let dirty = Rc::new(std::cell::Cell::new(false));
        let read: ReadValue = match &field.kind {
            Kind::Boolean => {
                let row = adw::SwitchRow::new();
                row.set_title(&escape_display(&field.label));
                row.set_active(value.and_then(Value::as_bool).unwrap_or(false));
                row.connect_active_notify({
                    let dirty = dirty.clone();
                    move |_| dirty.set(true)
                });
                group.add(&row);
                Box::new(move || Ok((!unset || dirty.get()).then(|| Value::Bool(row.is_active()))))
            }
            Kind::Enum { choices } => {
                let row = adw::ComboRow::new();
                row.set_title(&escape_display(&field.label));
                let labels: Vec<_> = choices.iter().map(|choice| choice.label.as_str()).collect();
                row.set_model(Some(&gtk::StringList::new(&labels)));
                row.set_selected(
                    choices
                        .iter()
                        .position(|choice| {
                            Some(choice.value.as_str()) == value.and_then(Value::as_str)
                        })
                        .map(|index| index as u32)
                        .unwrap_or(if unset { gtk::INVALID_LIST_POSITION } else { 0 }),
                );
                row.connect_selected_notify({
                    let dirty = dirty.clone();
                    move |_| dirty.set(true)
                });
                let choices = choices.clone();
                group.add(&row);
                Box::new(move || {
                    if unset && !dirty.get() {
                        return Ok(None);
                    }
                    choices
                        .get(row.selected() as usize)
                        .map(|choice| Some(Value::String(choice.value.clone())))
                        .ok_or_else(|| "Select a declared value".into())
                })
            }
            Kind::String { .. } | Kind::Integer { .. } | Kind::Number { .. } => {
                let row = adw::EntryRow::new();
                row.set_title(&escape_display(&field.label));
                let string = matches!(field.kind, Kind::String { .. });
                let text = value
                    .map(|value| {
                        if string {
                            value.as_str().unwrap_or("").into()
                        } else {
                            value.to_string()
                        }
                    })
                    .unwrap_or_default();
                row.set_text(&text);
                row.connect_changed({
                    let dirty = dirty.clone();
                    move |_| dirty.set(true)
                });
                group.add(&row);
                Box::new(move || {
                    if unset && !dirty.get() {
                        return Ok(None);
                    }
                    if string {
                        Ok(Some(Value::String(row.text().into())))
                    } else {
                        serde_json::from_str::<Value>(&row.text())
                            .map(Some)
                            .map_err(|_| "Enter a finite number within the declared bounds".into())
                    }
                })
            }
            Kind::Secret | Kind::Unsupported => {
                let row = adw::ActionRow::new();
                row.set_title(&escape_display(&field.label));
                row.set_subtitle("Unavailable; no plain-text substitute is supported");
                group.add(&row);
                continue;
            }
        };
        let checked = field.clone();
        editors.push((
            field.key.clone(),
            Box::new(move || {
                let value = read().map_err(|_| {
                    format!(
                        "Setting {}: enter an accepted value of its declared type",
                        checked.key
                    )
                })?;
                if let Some(value) = &value {
                    checked.validate(value).map_err(|_| {
                        format!(
                            "Setting {}: value is outside the declared type or bounds",
                            checked.key
                        )
                    })?;
                }
                Ok(value)
            }),
        ));
    }
    let result = adw::ActionRow::new();
    result.set_title("Local changes");
    result.set_subtitle("No changes saved from this page.");
    let feedback = adw::PreferencesGroup::new();
    feedback.set_title("Save and runtime status");
    feedback.add(&result);
    let runtime = adw::ActionRow::new();
    runtime.set_title("Runtime application");
    runtime.set_subtitle(status(&entry.key()).description());
    let restart_button = gtk::Button::with_label("Restart plugin…");
    restart_button.set_valign(gtk::Align::Center);
    restart_button.set_visible(status(&entry.key()) == Configuration::RestartRequired);
    restart_button.update_property(&[gtk::accessible::Property::Label(&format!(
        "Restart plugin {}",
        entry.key()
    ))]);
    let restart = restart.clone();
    let restart_key = entry.key();
    let restart_generation = entry.receipt.generation.clone();
    let restart_receipt = entry.receipt.clone();
    let restart_selection = load.clone();
    restart_button.connect_clicked(move |button| {
        if !restart_selection(&restart_key)
            .is_some_and(|(current, _)| current.receipt == restart_receipt)
        {
            return;
        }
        restart(
            &restart_key,
            &restart_generation,
            button
                .root()
                .and_then(|root| root.downcast::<gtk::Window>().ok()),
        )
    });
    runtime.add_suffix(&restart_button);
    feedback.add(&runtime);
    let weak_runtime = runtime.downgrade();
    let weak_restart = restart_button.downgrade();
    let status = status.clone();
    let status_key = entry.key();
    let status_selection = load.clone();
    let status_receipt = entry.receipt.clone();
    glib::timeout_add_local(std::time::Duration::from_millis(200), move || {
        let Some(row) = weak_runtime.upgrade() else {
            return glib::ControlFlow::Break;
        };
        if !status_selection(&status_key)
            .is_some_and(|(current, _)| current.receipt == status_receipt)
        {
            row.set_subtitle(
                "The selected package changed or was removed. Reopen its current settings.",
            );
            if let Some(button) = weak_restart.upgrade() {
                button.set_visible(false);
            }
            return glib::ControlFlow::Break;
        }
        let configuration = status(&status_key);
        row.set_subtitle(configuration.description());
        if let Some(button) = weak_restart.upgrade() {
            button.set_visible(configuration == Configuration::RestartRequired);
        }
        glib::ControlFlow::Continue
    });
    let save = save.clone();
    let key = entry.key();
    let generation = entry.receipt.generation.clone();
    let schema = schema.clone();
    let result = result.clone();
    let stored = stored.clone();
    let save_selection = load.clone();
    let save_receipt = entry.receipt.clone();
    commit.connect_clicked(move |_| {
        if !save_selection(&key).is_some_and(|(current, _)| current.receipt == save_receipt) {
            result.set_subtitle(
                "The selected package changed or was removed. Reopen its current settings.",
            );
            return;
        }
        let changes: Result<Map<_, _>, String> = editors
            .iter()
            .filter_map(|(key, read)| match read() {
                Ok(Some(value)) => Some(Ok((key.clone(), value))),
                Ok(None) => None,
                Err(problem) => Some(Err(problem)),
            })
            .collect();
        let outcome = changes.and_then(|changes| {
            // validation errors are fixed host text and never echo values.
            schema
                .merge_edit(stored.as_ref(), &changes)
                .map_err(|problem| problem.to_string())?;
            save(&key, &generation, &changes)
        });
        result.set_subtitle(&escape_display(match &outcome {
            Ok(durability) => durability.description(),
            Err(problem) => problem,
        }));
    });
    page.add(&group);
    page.add(&feedback);
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&adw::HeaderBar::new());
    toolbar.set_content(Some(&page));
    adw::NavigationPage::new(&toolbar, "Settings")
}
