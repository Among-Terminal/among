//! edits are committed explicitly, never by initial control signals. schema
//! defaults remain in memory until the user presses Save.

use super::*;
use crate::plugin::inventory::Entry;
use crate::plugin::schema::{Apply, Kind};
use serde_json::{Map, Value};

type ReadValue = Box<dyn Fn() -> Result<Value, String>>;

pub(super) fn add_actions(
    row: &adw::ActionRow,
    entry: Entry,
    save: Rc<SavePluginSettings>,
    load: Rc<PluginDetailsQuery>,
    status: Rc<ConfigurationQuery>,
) {
    let details = gtk::Button::with_label("Details…");
    details.set_valign(gtk::Align::Center);
    details.update_property(&[gtk::accessible::Property::Label(&format!(
        "Details for {}",
        entry.key()
    ))]);
    let details_receipt = entry.receipt.clone();
    let details_key = entry.key();
    let current_details = load.clone();
    let description = match &entry.inspected {
        Ok(inspected) => {
            let manifest = &inspected.manifest;
            let compatibility = manifest
                .compatibility(
                    crate::plugin::managed::MAJOR,
                    crate::plugin::managed::MINOR,
                    crate::plugin::managed::FEATURES,
                )
                .map_or_else(
                    |problem| problem.to_string(),
                    |()| "Compatible declared API".into(),
                );
            format!("{}\n\nVersion: {}\nGitHub: {}/{} (repository {})\nIdentity: {}\nRevision: {}\nRelease / asset: {} / {}\nPackage SHA-256: {}\nEntrypoint: {}\nAPI: {}.{}\n{}\nRequired features: {}\nOptional features: {}\nDelivery channels: {}\n{}\n\nPlugins run as your user, not in an OS sandbox.",
                manifest.description,manifest.version,entry.receipt.owner,entry.receipt.repository,
                inspected.identity.repository_id(),entry.key(),entry.receipt.revision,entry.receipt.release_id,entry.receipt.asset_id,
                entry.receipt.package_sha256,inspected.executable.display(),manifest.api.major,manifest.api.minor,compatibility,
                manifest.required_features.join(", "),manifest.optional_features.join(", "),
                manifest.delivery_channels.iter().map(|channel|format!("{} ({})",channel.name,channel.id)).collect::<Vec<_>>().join(", "),
                inspected.icon_problem.map_or("".into(),|problem|format!("Artwork: {problem}")))
        }
        Err(problem) => format!(
            "Identity: {}\nGitHub: {}/{}\n{}\nNo executable was probed.",
            entry.key(),
            entry.receipt.owner,
            entry.receipt.repository,
            problem
        ),
    };
    details.connect_clicked(move |button| {
        let dialog = adw::AlertDialog::builder()
            .heading("Plugin details")
            .body(
                if current_details(&details_key)
                    .is_some_and(|(current, _)| current.receipt == details_receipt)
                {
                    description.as_str()
                } else {
                    "The selected package changed or was removed. Reopen its current details."
                },
            )
            .build();
        dialog.add_response("close", "Close");
        dialog.present(
            button
                .root()
                .and_then(|root| root.downcast::<gtk::Window>().ok())
                .as_ref(),
        );
    });
    row.add_suffix(&details);
    let Ok(inspected) = entry.inspected.as_ref() else {
        return;
    };
    let icon = match (&inspected.manifest.icon, &inspected.raster_icon) {
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
    }
    .unwrap_or_else(|| gtk::Image::from_icon_name("application-x-addon-symbolic"));
    icon.set_pixel_size(32);
    row.add_prefix(&icon);
    let Some(schema) = inspected.manifest.settings.clone() else {
        return;
    };
    let settings = gtk::Button::with_label("Settings…");
    settings.set_valign(gtk::Align::Center);
    settings.update_property(&[gtk::accessible::Property::Label(&format!(
        "Settings for {}",
        entry.key()
    ))]);
    settings.connect_clicked(move |button| {
        let current = load(&entry.key()).filter(|(current, _)| current.receipt == entry.receipt);
        let Some((_, stored)) = current else { return };
        let window = adw::PreferencesWindow::builder()
            .title("Plugin settings")
            .search_enabled(false)
            .build();
        window.set_transient_for(
            button
                .root()
                .and_then(|root| root.downcast::<gtk::Window>().ok())
                .as_ref(),
        );
        let page = adw::PreferencesPage::new();
        let group = adw::PreferencesGroup::new();
        group.set_title("Local settings");
        group.set_description(Some(match schema.apply {
            Apply::Live => "Saved locally. Runtime application is reported separately.",
            Apply::Restart => "Saving requires a plugin restart before runtime application.",
        }));
        let mut editors: Vec<(String, ReadValue)> = Vec::new();
        for field in &schema.fields {
            let value = stored
                .as_ref()
                .and_then(|stored| stored.get("values"))
                .and_then(|values| values.get(&field.key))
                .or(field.default.as_ref());
            let read: ReadValue = match &field.kind {
                Kind::Boolean => {
                    let row = adw::SwitchRow::new();
                    row.set_title(&escape_display(&field.label));
                    row.set_active(value.and_then(Value::as_bool).unwrap_or(false));
                    group.add(&row);
                    Box::new(move || Ok(Value::Bool(row.is_active())))
                }
                Kind::Enum { choices } => {
                    let row = adw::ComboRow::new();
                    row.set_title(&escape_display(&field.label));
                    let labels: Vec<_> =
                        choices.iter().map(|choice| choice.label.as_str()).collect();
                    row.set_model(Some(&gtk::StringList::new(&labels)));
                    row.set_selected(
                        choices
                            .iter()
                            .position(|choice| {
                                Some(choice.value.as_str()) == value.and_then(Value::as_str)
                            })
                            .unwrap_or(0) as u32,
                    );
                    let choices = choices.clone();
                    group.add(&row);
                    Box::new(move || {
                        choices
                            .get(row.selected() as usize)
                            .map(|choice| Value::String(choice.value.clone()))
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
                    group.add(&row);
                    Box::new(move || {
                        if string {
                            Ok(Value::String(row.text().into()))
                        } else {
                            serde_json::from_str::<Value>(&row.text()).map_err(|_| {
                                "Enter a finite number within the declared bounds".into()
                            })
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
            editors.push((field.key.clone(), read));
        }
        let result = adw::ActionRow::new();
        result.set_title("Local changes");
        let runtime = adw::ActionRow::new();
        runtime.set_title("Runtime application");
        runtime.set_subtitle(status(&entry.key()).description());
        group.add(&runtime);
        let weak_runtime = runtime.downgrade();
        let status = status.clone();
        let status_key = entry.key();
        glib::timeout_add_local(std::time::Duration::from_millis(200), move || {
            let Some(row) = weak_runtime.upgrade() else {
                return glib::ControlFlow::Break;
            };
            row.set_subtitle(status(&status_key).description());
            glib::ControlFlow::Continue
        });
        let commit = gtk::Button::with_label("Save");
        commit.set_sensitive(schema.supported());
        commit.set_valign(gtk::Align::Center);
        result.add_suffix(&commit);
        group.add(&result);
        let save = save.clone();
        let key = entry.key();
        let generation = entry.receipt.generation.clone();
        let schema = schema.clone();
        let result = result.clone();
        let stored = stored.clone();
        commit.connect_clicked(move |_| {
            let changes: Result<Map<_, _>, String> = editors
                .iter()
                .map(|(key, read)| read().map(|value| (key.clone(), value)))
                .collect();
            let outcome = changes.and_then(|changes| {
                // validation errors are fixed host text and never echo values.
                schema
                    .merge_edit(stored.as_ref(), &changes)
                    .map_err(|problem| problem.to_string())?;
                save(&key, &generation, &changes)
            });
            result.set_subtitle(&escape_display(match &outcome {
                Ok(()) => "Saved locally",
                Err(problem) => problem,
            }));
        });
        page.add(&group);
        window.add(&page);
        window.present();
    });
    row.add_suffix(&settings);
}
