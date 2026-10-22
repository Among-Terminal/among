use super::*;

pub(super) fn present(parent: &gtk::Window, ctx: &PreferencesContext) {
    let window = adw::PreferencesWindow::builder()
        .title("Retained plugin settings")
        .transient_for(parent)
        .search_enabled(false)
        .default_width(640)
        .default_height(520)
        .build();
    let page = adw::PreferencesPage::new();
    let group = adw::PreferencesGroup::new();
    group.set_title("Settings of uninstalled plugins");
    group.set_description(Some("Uninstall retains local values. Delete saved and pending settings only through a separate confirmation. This does not remove repositories, grants, other preferences or external files, and is not secure erasure. Installed plugins must be uninstalled first."));
    match (ctx.retained_settings)() {
        Ok(entries) if !entries.is_empty() => {
            for expected in entries {
                let key = expected.identity().key();
                let row = adw::ActionRow::builder()
                    .title(escape_display(&key))
                    .subtitle("Local settings remain; no installed package is selected.")
                    .build();
                row.set_subtitle_lines(0);
                let delete = gtk::Button::with_label("Delete settings…");
                delete.set_valign(gtk::Align::Center);
                delete.update_property(&[gtk::accessible::Property::Label(&format!(
                    "Delete retained settings for {key}"
                ))]);
                delete.connect_clicked({
                    let delete_settings = ctx.delete_retained_settings.clone();
                    let row = row.downgrade();
                    move |button| {
                        let Some(parent) = button.root().and_then(|root| root.downcast::<gtk::Window>().ok()) else {return;};
                        let dialog = adw::AlertDialog::builder().heading("Delete retained settings?")
                            .body(format!("{key}\nDelete only this identity's saved settings payload and matching pending settings edits. This cannot be undone. Other preferences, grants, repositories, legacy files and external effects are not deleted. An installed identity or changed settings will refuse this request; reopen the current list.")).build();
                        dialog.add_response("cancel", "Cancel");
                        dialog.add_response("delete", "Delete settings");
                        dialog.set_default_response(Some("cancel"));
                        dialog.set_close_response("cancel");
                        dialog.set_response_appearance("delete", adw::ResponseAppearance::Destructive);
                        dialog.connect_response(None, {
                            let expected = expected.clone();
                            let delete_settings = delete_settings.clone();
                            let row = row.clone();
                            let button = button.downgrade();
                            let parent = parent.downgrade();
                            move |_, response| {
                                if response != "delete" || parent.upgrade().is_none_or(|parent| !parent.is_visible()) {return;}
                                let Some(row) = row.upgrade() else {return;};
                                match delete_settings(&expected) {
                                    Ok(durability) => {
                                        row.set_subtitle(if durability == crate::settings::Durability::Durable {"Retained settings deleted locally."} else {"Settings deletion written; durability uncertain. Reopen to inspect current saved state."});
                                        if let Some(button) = button.upgrade() {button.set_sensitive(false);}
                                    }
                                    Err(problem) => row.set_subtitle(&escape_display(&problem)),
                                }
                            }
                        });
                        dialog.present(Some(&parent));
                    }
                });
                row.add_suffix(&delete);
                row.set_activatable_widget(Some(&delete));
                group.add(&row);
            }
        }
        result => {
            let row = adw::ActionRow::new();
            match result {
                Ok(_) => {
                    row.set_title("No retained settings to delete");
                    row.set_subtitle(
                        "Only uninstalled identities with saved or pending settings appear here.",
                    );
                }
                Err(problem) => {
                    row.set_title("Retained settings unavailable");
                    row.set_subtitle(&escape_display(&problem));
                }
            }
            group.add(&row);
        }
    }
    page.add(&group);
    window.add(&page);
    window.present();
}
