//! source callbacks retain the exact registration generation they displayed.
//! refresh/remove never retarget a row by its label or sorted position.

use super::*;
use crate::plugin::{
    data::Problem,
    install::{
        registry::{self, Outcome, Registration, Request},
        source::{Inspection, Job},
    },
    origin::Change,
};
use std::cell::Cell;
use std::path::PathBuf;

pub(super) type Refresh = dyn Fn(u64) -> Result<Job, Problem>;

pub(super) struct Panel {
    root: Option<PathBuf>,
    refresh: Rc<Refresh>,
    show: Rc<dyn Fn(Inspection)>,
    group: glib::WeakRef<adw::PreferencesGroup>,
    status: glib::WeakRef<adw::ActionRow>,
    save: glib::WeakRef<gtk::Button>,
    rows: RefCell<Vec<adw::ActionRow>>,
    buttons: RefCell<Vec<(glib::WeakRef<gtk::Button>, bool)>>,
    registrations: RefCell<Vec<Registration>>,
    candidate: RefCell<Option<(Inspection, Option<Registration>)>>,
    busy: Cell<bool>,
    generation: Cell<u64>,
}

fn timestamp(time: u64) -> String {
    glib::DateTime::from_unix_utc(time as i64)
        .and_then(|time| time.format_iso8601())
        .map(|time| time.to_string())
        .unwrap_or_else(|_| format!("Unix time {time}"))
}

impl Panel {
    pub fn new(
        page: &adw::PreferencesPage,
        preview: &adw::PreferencesGroup,
        root: Option<PathBuf>,
        refresh: Rc<Refresh>,
        show: Rc<dyn Fn(Inspection)>,
    ) -> Rc<Self> {
        let group = adw::PreferencesGroup::new();
        group.set_title("Registered sources");
        group.set_description(Some("Registrations and cached metadata are separate from installed plugins. Removing a source does not uninstall, disable or delete settings."));
        let status = adw::ActionRow::builder()
            .title("Loading source registrations")
            .build();
        status.set_subtitle_lines(0);
        group.add(&status);
        page.add(&group);
        let save = gtk::Button::with_label("Add source");
        save.set_valign(gtk::Align::Center);
        save.set_visible(false);
        preview.set_header_suffix(Some(&save));
        let panel = Rc::new(Self {
            root,
            refresh,
            show,
            group: group.downgrade(),
            status: status.downgrade(),
            save: save.downgrade(),
            rows: RefCell::new(Vec::new()),
            buttons: RefCell::new(Vec::new()),
            registrations: RefCell::new(Vec::new()),
            candidate: RefCell::new(None),
            busy: Cell::new(false),
            generation: Cell::new(0),
        });
        save.connect_clicked({
            let panel = Rc::downgrade(&panel);
            move |_| {
                let Some(panel) = panel.upgrade() else {
                    return;
                };
                let Some((inspection, expected)) = panel.candidate.borrow().clone() else {
                    return;
                };
                let request = match expected {
                    None => Request::Add(Box::new(inspection)),
                    Some(expected) if expected.is_official() => Request::Refresh {
                        expected,
                        inspection: Box::new(inspection),
                    },
                    Some(expected)
                        if inspection.origin().compare(&expected.origin)
                            == Change::PublisherChanged =>
                    {
                        Request::ApprovePublisher {
                            expected,
                            inspection: Box::new(inspection),
                        }
                    }
                    Some(expected) => Request::Refresh {
                        expected,
                        inspection: Box::new(inspection),
                    },
                };
                panel.storage(request, "Saving explicitly approved source metadata");
            }
        });
        panel.render(Vec::new());
        panel.storage(Request::EffectiveList, "Loading source registrations");
        panel
    }

    pub fn clear(&self) {
        self.generation.set(self.generation.get().wrapping_add(1));
        self.candidate.borrow_mut().take();
        if let Some(save) = self.save.upgrade() {
            save.set_visible(false);
        }
    }

    pub fn inspected(&self, inspection: Inspection) {
        self.clear();
        let expected = self
            .registrations
            .borrow()
            .iter()
            .find(|source| source.origin.repository_id == inspection.repository_id)
            .cloned();
        self.candidate.replace(Some((inspection, expected)));
        self.update_save();
    }

    fn update_save(&self) {
        for (button, available) in self.buttons.borrow().iter() {
            if let Some(button) = button.upgrade() {
                button.set_sensitive(!self.busy.get() && *available);
            }
        }
        let Some(save) = self.save.upgrade() else {
            return;
        };
        let candidate = self.candidate.borrow();
        save.set_visible(candidate.is_some());
        save.set_sensitive(!self.busy.get());
        if let Some((inspection, previous)) = candidate.as_ref() {
            let valid = inspection.validate_source().is_ok();
            save.set_sensitive(!self.busy.get() && valid);
            let change = previous
                .as_ref()
                .map(|previous| inspection.origin().compare(&previous.origin));
            save.set_label(if !valid {
                "Official source identity mismatch"
            } else if previous.as_ref().is_some_and(Registration::is_official) {
                "Save official metadata refresh"
            } else {
                match change {
                    None => "Add source",
                    Some(Change::PublisherChanged) => "Approve publisher change",
                    _ => "Save metadata refresh",
                }
            });
            save.set_tooltip_text(Some(&format!("Repository {} / publisher {}. This saves discovery metadata only; no plugin is installed, enabled or granted runtime access.", inspection.repository_id, inspection.publisher_id)));
        }
    }

    fn feedback(&self, title: &str, detail: &str) {
        if let Some(status) = self.status.upgrade() {
            status.set_title(title);
            status.set_subtitle(&escape_display(detail));
        }
    }

    fn render(self: &Rc<Self>, registrations: Vec<Registration>) {
        let registrations = registry::effective_sources(registrations);
        let Some(group) = self.group.upgrade() else {
            return;
        };
        for row in self.rows.borrow_mut().drain(..) {
            group.remove(&row);
        }
        self.buttons.borrow_mut().clear();
        self.registrations.replace(registrations.clone());
        for source in registrations {
            let source = Rc::new(source);
            let row = adw::ActionRow::builder()
                .title(escape_display(&source.display_title()))
                .build();
            row.set_subtitle_lines(0);
            let freshness = source
                .retrieved_at()
                .map(|time| {
                    format!(
                        "Retrieved {} · {}",
                        timestamp(time),
                        if registry::now().map_or(true, |now| source.stale_at(now)) {
                            "stale or clock changed"
                        } else {
                            "less than 24 hours old (not an online check)"
                        }
                    )
                })
                .unwrap_or_else(|| "No retained snapshot; refresh when online".into());
            let origin = source.display_origin();
            let protection = if source.is_official() {
                "Application-owned official source; cannot be removed. Discovery does not install, enable or grant access.\n"
            } else {
                ""
            };
            let unavailable = source
                .available()
                .err()
                .map(|problem| format!("\nUnavailable: {}", problem.0))
                .unwrap_or_default();
            row.set_subtitle(&escape_display(&format!(
                "{protection}{}/{} · repository {} · publisher {} · {} cached snapshots\n{freshness}{unavailable}",
                origin.owner,
                origin.repository,
                origin.repository_id,
                origin.publisher_id,
                source.snapshot_count()
            )));
            for (label, operation) in [("Cached", 0), ("Refresh", 1), ("Remove source", 2)] {
                if operation == 2 && source.is_official() {
                    continue;
                }
                let button = gtk::Button::with_label(label);
                button.set_valign(gtk::Align::Center);
                let available =
                    operation != 0 || (source.snapshot_count() > 0 && source.available().is_ok());
                button.set_sensitive(!self.busy.get() && available);
                self.buttons
                    .borrow_mut()
                    .push((button.downgrade(), available));
                button.update_property(&[gtk::accessible::Property::Label(&format!(
                    "{label} repository {}",
                    source.origin.repository_id
                ))]);
                button.connect_clicked({
                    let panel = Rc::downgrade(self);
                    let expected = source.clone();
                    move |_| {
                        let Some(panel) = panel.upgrade() else {
                            return;
                        };
                        match operation {
                            0 => {
                                panel.clear();
                                panel.storage(
                                    Request::Cached(expected.as_ref().clone()),
                                    "Reading cached metadata; no network request",
                                );
                            }
                            1 => panel.refresh_source(expected.as_ref().clone()),
                            _ => {
                                panel.clear();
                                panel.storage(
                                    Request::Remove(expected.as_ref().clone()),
                                    "Removing source registration only",
                                );
                            }
                        }
                    }
                });
                row.add_suffix(&button);
            }
            group.add(&row);
            self.rows.borrow_mut().push(row);
        }
    }

    fn storage(self: &Rc<Self>, request: Request, title: &str) {
        if self.busy.replace(true) {
            self.feedback(
                "Source operation already active",
                "Wait for the bounded operation to finish before another source edit.",
            );
            return;
        }
        let job = self
            .root
            .clone()
            .ok_or(Problem("source data directory is unavailable"))
            .and_then(|root| registry::Job::start(root, request));
        self.update_save();
        self.feedback(title, "Installed selections, switches and local plugin settings are unchanged by source operations.");
        let job = match job {
            Ok(job) => job,
            Err(problem) => {
                self.busy.set(false);
                self.update_save();
                self.feedback("Source operation was not started", problem.0);
                return;
            }
        };
        let job = Rc::new(job);
        let cancel = gtk::Button::with_label("Cancel source storage");
        cancel.set_valign(gtk::Align::Center);
        cancel.connect_clicked({let job=job.clone();let panel=Rc::downgrade(self);move |button| {
            let accepted=job.cancel();button.set_sensitive(false);
            if let Some(panel)=panel.upgrade() {panel.feedback(if accepted {"Cancelling source storage operation"}else{"Source outcome is already being finalized; cancellation is too late"},"Cancellation cannot undo an authoritative source change which has already begun commitment.");}
        }});
        if let Some(status) = self.status.upgrade() {
            status.add_suffix(&cancel);
        }
        let cancel = cancel.downgrade();
        let panel = Rc::downgrade(self);
        let generation = self.generation.get();
        glib::timeout_add_local(std::time::Duration::from_millis(50), move || {
            let Some(panel) = panel.upgrade() else {
                return glib::ControlFlow::Break;
            };
            let Some(outcome) = job.poll() else {
                return glib::ControlFlow::Continue;
            };
            if let (Some(status), Some(cancel)) = (panel.status.upgrade(), cancel.upgrade()) {
                status.remove(&cancel);
            }
            panel.busy.set(false);
            match outcome {
                Ok(Outcome::EffectiveList {
                    registrations,
                    problem,
                }) => {
                    let has_custom = registrations.iter().any(|source| !source.is_official());
                    panel.render(registrations);
                    if let Some(problem) = problem {
                        panel.feedback(
                            if problem == registry::OFFICIAL_STORAGE_PROBLEM {
                                "Official metadata is unavailable; custom sources remain available"
                            } else {
                                "User source storage is unavailable; official source remains built in"
                            },
                            problem.0,
                        );
                    } else {
                        panel.feedback(if has_custom { "Cached source registrations loaded" } else { "Source registrations loaded" }, "The official source is application-owned. Cached metadata is not an online check; no plugin was installed or granted access.");
                    }
                }
                Ok(Outcome::List(registrations)) => {
                    let empty = registrations.is_empty();
                    panel.render(registrations);
                    panel.feedback(if empty { "No registered sources" } else { "Cached source registrations loaded" }, "Offline cache is not evidence of current network availability or publisher authentication.");
                }
                Ok(Outcome::Changed {
                    registrations,
                    durability,
                    problem,
                }) => {
                    panel.render(registrations);
                    if panel.generation.get() == generation {
                        panel.clear();
                    }
                    if let Some(problem) = problem {
                        panel.feedback(
                            if durability == crate::settings::Durability::Durable {
                                "Source change saved; other source storage is unavailable"
                            } else {
                                "Source change written; durability uncertain and other source storage unavailable"
                            },
                            problem.0,
                        );
                    } else {
                        panel.feedback(if durability == crate::settings::Durability::Durable { "Source change saved" } else { "Source change written; durability uncertain" }, "No installed package, enabled switch or local plugin settings were changed.");
                    }
                }
                Ok(Outcome::Cached {
                    inspection,
                    retrieved_at,
                    stale,
                }) if panel.generation.get() == generation => {
                    (panel.show)(inspection);
                    panel.feedback(if stale { "Cached metadata is stale or its clock is uncertain" } else { "Cached metadata; not an online check" }, &format!("Retrieved {}. No network request was made; refresh explicitly to check current metadata.", timestamp(retrieved_at)));
                }
                Ok(Outcome::Cached { .. }) => {}
                Err(problem) => panel.feedback(
                    "Source operation failed; retained registry/cache unchanged",
                    problem.0,
                ),
            }
            panel.update_save();
            glib::ControlFlow::Break
        });
    }

    fn refresh_source(self: &Rc<Self>, expected: Registration) {
        if self.busy.replace(true) {
            return;
        }
        self.clear();
        self.update_save();
        self.feedback("Refreshing numeric repository identity", "Failed or cancelled refresh leaves the previous cache and all installed plugins unchanged.");
        let job = match (self.refresh)(expected.origin.repository_id) {
            Ok(job) => job,
            Err(problem) => {
                self.busy.set(false);
                self.update_save();
                self.feedback("Refresh was not started", problem.0);
                return;
            }
        };
        let cancel = gtk::Button::with_label("Cancel source request");
        cancel.set_valign(gtk::Align::Center);
        let job = Rc::new(job);
        cancel.connect_clicked({
            let job = job.clone();
            move |button| {
                job.cancel();
                button.set_sensitive(false);
            }
        });
        if let Some(status) = self.status.upgrade() {
            status.add_suffix(&cancel);
        }
        let panel = Rc::downgrade(self);
        let cancel = cancel.downgrade();
        let generation = self.generation.get();
        glib::timeout_add_local(std::time::Duration::from_millis(50), move || {
            let Some(panel) = panel.upgrade() else {
                return glib::ControlFlow::Break;
            };
            let Some(outcome) = job.poll() else {
                return glib::ControlFlow::Continue;
            };
            panel.busy.set(false);
            if let (Some(status), Some(cancel)) = (panel.status.upgrade(), cancel.upgrade()) {
                status.remove(&cancel);
            }
            match outcome {
                Ok(inspection) => {
                    if let Err(problem) = inspection.validate_source() {
                        panel.feedback("Official source identity mismatch", problem.0);
                        panel.update_save();
                        return glib::ControlFlow::Break;
                    }
                    let change = inspection.origin().compare(&expected.origin);
                    if generation == panel.generation.get() {
                        (panel.show)(inspection.clone());
                        panel
                            .candidate
                            .replace(Some((inspection.clone(), Some(expected.clone()))));
                    }
                    if change == Change::IdentityChanged
                        || (change == Change::PublisherChanged && !expected.is_official())
                    {
                        panel.feedback("Source approval was not changed", change.description());
                    } else {
                        panel.storage(
                            Request::Refresh {
                                expected: expected.clone(),
                                inspection: Box::new(inspection),
                            },
                            "Saving refreshed source metadata",
                        );
                    }
                }
                Err(problem) => panel.feedback(
                    "Refresh failed; retained metadata remains available offline",
                    problem.0,
                ),
            }
            panel.update_save();
            glib::ControlFlow::Break
        });
    }
}

#[cfg(test)]
mod tests;
