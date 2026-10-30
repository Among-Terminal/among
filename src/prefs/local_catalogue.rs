//! Installation from a local schema2 catalogue, offered only by a primary
//! started with `--plugin-development`. Choosing a file reads metadata only;
//! each package is installed by an explicit action after its review, and a
//! development install of the same ID is replaced only after a separate
//! confirmation.

use super::*;
use crate::plugin::data::Problem;
use crate::plugin::install::local::{LocalCatalogue, LocalSelection};
use crate::plugin::install::source::InstallJob;
use gtk::gio;

type Install = dyn Fn(LocalSelection) -> Result<InstallJob, Problem>;

pub(super) fn present(
    parent: &gtk::Window,
    changed: Rc<dyn Fn()>,
    details: Rc<super::PluginDetailsQuery>,
) {
    let window = window(parent, changed, details, installer());
    let chooser = gtk::FileDialog::builder()
        .title("Choose among-plugins.json")
        .modal(true)
        .build();
    let filter = gtk::FileFilter::new();
    filter.set_name(Some("Plugin catalogue (among-plugins.json)"));
    filter.add_pattern("among-plugins.json");
    let filters = gio::ListStore::new::<gtk::FileFilter>();
    filters.append(&filter);
    chooser.set_filters(Some(&filters));
    let opened = window.clone();
    chooser.open(
        Some(&window.0),
        gio::Cancellable::NONE,
        move |file| match file.ok().and_then(|file| file.path()) {
            Some(path) => opened.show(&path),
            None => opened.status("No catalogue was chosen; nothing was read."),
        },
    );
}

/// Opens the local catalogue window for `path` without a file chooser. Native
/// tests drive the rest of the window with real input.
#[cfg(test)]
pub(crate) fn present_path_for_test(
    parent: &gtk::Window,
    path: &std::path::Path,
    changed: Rc<dyn Fn()>,
    details: Rc<super::PluginDetailsQuery>,
) {
    window(parent, changed, details, installer()).show(path);
}

fn installer() -> Rc<Install> {
    Rc::new(|selection| {
        crate::plugin::install::local::start(
            selection,
            crate::plugin::inventory::root()
                .ok_or(Problem("managed data directory is unavailable"))?,
        )
    })
}

#[derive(Clone)]
struct Window(
    adw::PreferencesWindow,
    adw::PreferencesGroup,
    Rc<dyn Fn()>,
    Rc<super::PluginDetailsQuery>,
    Rc<Install>,
);

fn window(
    parent: &gtk::Window,
    changed: Rc<dyn Fn()>,
    details: Rc<super::PluginDetailsQuery>,
    install: Rc<Install>,
) -> Window {
    let window = adw::PreferencesWindow::builder()
        .transient_for(parent)
        .modal(false)
        .title("Install from local catalogue")
        .search_enabled(false)
        .default_width(640)
        .default_height(480)
        .build();
    let page = adw::PreferencesPage::new();
    page.set_title("Local catalogue");
    let group = adw::PreferencesGroup::new();
    group.set_title("Development packages");
    group.set_description(Some(crate::plugin::inventory::LOCAL_MISSING_PROVENANCE));
    page.add(&group);
    window.add(&page);
    window.present();
    Window(window, group, changed, details, install)
}

impl Window {
    fn status(&self, text: &str) {
        let row = adw::ActionRow::builder()
            .title(escape_display(text))
            .build();
        self.1.add(&row);
    }

    fn show(&self, path: &std::path::Path) {
        let catalogue = match LocalCatalogue::open(path) {
            Ok(catalogue) => catalogue,
            Err(problem) => {
                self.status(problem.0);
                return;
            }
        };
        self.1
            .set_title(&escape_display(&catalogue.path().display().to_string()));
        for plugin in &catalogue.catalogue.plugins {
            for version in &plugin.versions {
                let row = adw::ActionRow::builder()
                    .title(escape_display(&format!(
                        "{} {} ({})",
                        version.manifest.name, version.manifest.version, plugin.id
                    )))
                    .build();
                let button = gtk::Button::with_label("Review…");
                button.set_valign(gtk::Align::Center);
                button.update_property(&[gtk::accessible::Property::Label(&format!(
                    "Review local {} version {}",
                    plugin.id, version.manifest.version
                ))]);
                row.add_suffix(&button);
                match catalogue.select(&plugin.id, &version.manifest.version) {
                    Err(problem) => {
                        row.set_subtitle(problem.0);
                        button.set_sensitive(false);
                    }
                    Ok(selection) => {
                        row.set_subtitle(&escape_display(&format!(
                            "{} · package {}",
                            selection.identity(),
                            catalogue
                                .artifact(&plugin.id, &version.manifest.version)
                                .map_or_else(
                                    |problem| problem.to_string(),
                                    |artifact| artifact.asset.clone()
                                )
                        )));
                        let window = self.clone();
                        let row_weak = row.downgrade();
                        button.connect_clicked(move |button| {
                            if let Some(row) = row_weak.upgrade() {
                                window.review(button, &row, selection.clone());
                            }
                        });
                    }
                }
                self.1.add(&row);
            }
        }
    }

    fn review(&self, button: &gtk::Button, row: &adw::ActionRow, selection: LocalSelection) {
        // Whatever is installed now decides between a new install and a
        // replacement, so an earlier look at the inventory cannot be stale.
        let current = (self.3)(&selection.identity()).map(|(entry, _)| entry.receipt);
        let selection = match &current {
            Some(current) => match selection.replacing(current) {
                Ok(selection) => selection,
                Err(problem) => {
                    row.set_subtitle(problem.0);
                    return;
                }
            },
            None => selection,
        };
        let replacing = selection.replaces().is_some();
        let dialog = adw::AlertDialog::builder()
            .heading(if replacing {
                "Replace the development install?"
            } else {
                "Install this local package?"
            })
            .body(selection.review_description())
            .build();
        dialog.add_response("cancel", "Cancel");
        dialog.add_response("install", if replacing { "Replace" } else { "Install" });
        dialog.set_response_appearance(
            "install",
            if replacing {
                adw::ResponseAppearance::Destructive
            } else {
                adw::ResponseAppearance::Suggested
            },
        );
        dialog.set_default_response(Some("cancel"));
        dialog.set_close_response("cancel");
        let window = self.clone();
        let button = button.downgrade();
        let row = row.downgrade();
        dialog.connect_response(None, move |_, response| {
            if response != "install" {
                return;
            }
            let (Some(button), Some(row)) = (button.upgrade(), row.upgrade()) else {
                return;
            };
            if !window.0.is_visible() {
                return;
            }
            let now = (window.3)(&selection.identity()).map(|(entry, _)| entry.receipt);
            if now.as_ref() != selection.replaces() {
                row.set_subtitle("The installed development package changed; review again.");
                return;
            }
            super::repositories::run_job(
                &button,
                &row,
                (window.4)(selection.clone()),
                false,
                window.2.clone(),
                &window.0,
            );
        });
        dialog.present(Some(&self.0));
    }
}
