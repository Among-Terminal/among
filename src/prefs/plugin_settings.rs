//! edits are committed explicitly, never by initial control signals. schema
//! defaults remain in memory until the user presses Save.

use super::*;
use crate::plugin::inventory::Entry;
use crate::plugin::schema::{Apply, Kind};
use serde_json::{Map, Value};

type ReadValue = Box<dyn Fn() -> Result<Option<Value>, String>>;

pub(super) fn add_actions(
    row: &adw::ActionRow,
    entry: Entry,
    save: Rc<SavePluginSettings>,
    load: Rc<PluginDetailsQuery>,
    status: Rc<ConfigurationQuery>,
    operations: (Rc<RestartPlugin>, Rc<PackageAction>),
    inventory: Rc<dyn Fn() -> Vec<crate::plugin::host::Overview>>,
) {
    let (restart, package_action) = operations;
    let details = gtk::Button::with_label("Details…");
    details.add_css_class("flat");
    details.set_tooltip_text(Some("Plugin details"));
    details.set_valign(gtk::Align::Center);
    details.update_property(&[gtk::accessible::Property::Label(&format!(
        "Details for {}",
        entry.key()
    ))]);
    let details_receipt = entry.receipt.clone();
    let details_key = entry.key();
    let current_details = load.clone();
    details.connect_clicked(move |button| {
        let parent = button
            .root()
            .and_then(|root| root.downcast::<gtk::Window>().ok());
        let current =
            current_details(&details_key).filter(|(current, _)| current.receipt == details_receipt);
        let Some((current, _)) = current else {
            let dialog = adw::AlertDialog::builder()
                .heading("Plugin changed")
                .body("The selected package changed or was removed. Reopen its current details.")
                .build();
            dialog.add_response("close", "Close");
            dialog.present(parent.as_ref());
            return;
        };
        let window = adw::PreferencesWindow::builder()
            .title("Plugin details")
            .search_enabled(false)
            .default_width(640)
            .default_height(560)
            .build();
        window.set_transient_for(parent.as_ref());
        let page = adw::PreferencesPage::new();
        let summary = adw::PreferencesGroup::new();
        let info = inventory().into_iter().find(|info| info.key == details_key);
        summary.set_title(&escape_display(
            info.as_ref()
                .map_or(details_key.as_str(), |info| info.name.as_str()),
        ));
        summary.set_description(Some(
            "Read-only snapshot; opening details does not run code. Plugins run as your user, not in an OS sandbox.",
        ));
        if let Some(info) = info {
            detail_row(&summary, "Description", &info.description);
            detail_row(&summary, "Runtime state", &describe_state(&info.state));
            if let Some(version)=&info.running_version {detail_row(&summary,"Running package version",version);}
            detail_row(&summary, "API compatibility", &info.declared);
            detail_row(
                &summary,
                "Observed API",
                &info
                    .observed
                    .map_or("Not observed in this session".into(), |api| {
                        format!("{}.{}", api.major, api.minor)
                    }),
            );
        }
        let source = adw::PreferencesGroup::new();
        source.set_title("Source and identity");
        detail_row(&source, "Identity", &current.key());
        let repository_id = current.inspected.as_ref().map_or(current.receipt.repository_id,
            |inspected| inspected.identity.repository_id());
        detail_row(
            &source,
            "GitHub repository",
            &format!(
                "{}/{} (repository {})",
                current.receipt.owner, current.receipt.repository, repository_id
            ),
        );
        if let Ok(inspected) = &current.inspected {
            let manifest = &inspected.manifest;
            detail_row(&source, "Version", &manifest.version);
            detail_row(
                &source,
                "Required features",
                &manifest.required_features.join(", "),
            );
            detail_row(
                &source,
                "Optional features",
                &manifest.optional_features.join(", "),
            );
            detail_row(
                &source,
                "Delivery channels",
                &manifest
                    .delivery_channels
                    .iter()
                    .map(|channel| format!("{} ({})", channel.name, channel.id))
                    .collect::<Vec<_>>()
                    .join(", "),
            );
            if let Some(problem) = inspected.icon_problem {
                detail_row(&source, "Artwork", &problem.to_string());
            }
        } else if let Err(problem) = &current.inspected {
            detail_row(&summary, "Metadata unavailable", &problem.to_string());
        }
        let package = adw::PreferencesGroup::new();
        package.set_title("Package provenance");
        detail_row(&package, "Catalogue-declared version commit", &current.receipt.revision);
        detail_row(
            &package,
            "Release / asset",
            &format!(
                "{} / {}",
                current.receipt.release_id, current.receipt.asset_id
            ),
        );
        detail_row(&package, "Package SHA-256", &current.receipt.package_sha256);
        detail_row(&package, "Manifest SHA-256", &current.receipt.manifest_sha256);
        detail_row(&package, "Selected generation", &current.receipt.generation);
        if let Some(receipt) = &current.receipt.source {
            detail_row(&source, "Numeric publisher identity", &receipt.publisher_id.to_string());
            detail_row(&package, "Resolved catalogue revision", &receipt.catalogue_revision);
            detail_row(&package, "Catalogue SHA-256", &receipt.catalogue_sha256);
        } else {
            detail_row(&source, "Numeric publisher identity", "Unconfirmed in this pre-installer receipt; an update requires renewed review");
        }
        if let Some(binding) = current.receipt.source.as_ref().and_then(|source| source.release_binding.as_ref()) {
            detail_row(&package, "Release tag", &binding.tag);
            detail_row(&package, "Observed tag reference object", &binding.object);
            detail_row(&package, "Resolved release commit", &binding.commit);
            detail_row(&package, "Release-tag consistency", "Observed before transfer and rechecked before commit. This is not build reproducibility, independent publisher authentication or a sandbox.");
        } else {
            detail_row(&package, "Release-tag consistency", "Not recorded for this installation; a revision alone is not tag verification.");
        }
        if let Ok(inspected) = &current.inspected {
            detail_row(
                &package,
                "Entrypoint",
                &inspected.executable.display().to_string(),
            );
        }
        page.add(&summary);
        page.add(&source);
        page.add(&package);
        window.add(&page);
        let actions=adw::PreferencesGroup::new();actions.set_title("Package actions");
        for (label,action,available) in [("Recover previous…",crate::plugin::install::mutation::Action::RestorePrevious,current.previous.is_some()),("Uninstall…",crate::plugin::install::mutation::Action::Uninstall,true)] {
            if !available{continue;}
            let row=adw::ActionRow::builder().title(label).build();
            let button=gtk::Button::with_label(label);button.set_valign(gtk::Align::Center);
            button.update_property(&[gtk::accessible::Property::Label(&format!("{label} {}",current.key()))]);
            button.connect_clicked({let package_action=package_action.clone();let receipt=current.receipt.clone();move |button| {
                package_action(receipt.clone(),action,button.root().and_then(|root|root.downcast::<gtk::Window>().ok()));
            }});
            row.add_suffix(&button);row.set_activatable_widget(Some(&button));actions.add(&row);
        }
        page.add(&actions);
        window.present();
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
    let name = inspected.manifest.name.clone();
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
            .default_width(640)
            .default_height(520)
            .build();
        window.set_transient_for(
            button
                .root()
                .and_then(|root| root.downcast::<gtk::Window>().ok())
                .as_ref(),
        );
        let page = adw::PreferencesPage::new();
        let group = adw::PreferencesGroup::new();
        group.set_title(&escape_display(&format!("{name} settings")));
        group.set_description(Some(match schema.apply {
            Apply::Live => "Use Save to store changes locally. Closing discards unsaved edits.",
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
                    Box::new(move || {
                        Ok((!unset || dirty.get()).then(|| Value::Bool(row.is_active())))
                    })
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
                                .map_err(|_| {
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
        result.set_subtitle("No changes saved from this window.");
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
        window.add(&page);
        window.present();
    });
    row.add_suffix(&settings);
}

fn detail_row(group: &adw::PreferencesGroup, title: &str, value: &str) {
    let row = adw::ActionRow::new();
    row.set_title(title);
    row.set_subtitle(&escape_display(if value.is_empty() {
        "None declared"
    } else {
        value
    }));
    row.set_subtitle_lines(0);
    row.set_subtitle_selectable(true);
    group.add(&row);
}
