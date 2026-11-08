//! a repository look-up supplies a read-only preview. Only the separate Add
//! source action gives those bytes persistence authority; closing the preview
//! cancels its request without writing a registration or catalogue cache.

use super::*;
use crate::plugin::{
    data::Problem,
    install::{
        registry::{self, Outcome, Registration, Request},
        source::{Inspection, Job, Repository},
    },
    origin::Change,
};
use std::{cell::Cell, path::PathBuf};

pub(super) type Lookup = dyn Fn(Repository) -> Result<Job, Problem>;

struct State {
    root: Option<PathBuf>,
    lookup: Rc<Lookup>,
    changed: Rc<dyn Fn()>,
    registrations: RefCell<Vec<Registration>>,
    registry: RefCell<Option<registry::Job>>,
    request: RefCell<Option<Job>>,
    candidate: RefCell<Option<Inspection>>,
    adding: Cell<bool>,
    loaded: Cell<bool>,
    closed: Cell<bool>,
    dialog: glib::WeakRef<adw::Dialog>,
    entry: glib::WeakRef<adw::EntryRow>,
    lookup_button: glib::WeakRef<gtk::Button>,
    add: glib::WeakRef<gtk::Button>,
    cancel: glib::WeakRef<gtk::Button>,
    status: glib::WeakRef<adw::ActionRow>,
    preview: glib::WeakRef<adw::PreferencesGroup>,
    rows: RefCell<Vec<adw::ActionRow>>,
}

pub(super) fn present(
    parent: &gtk::Window,
    root: Option<PathBuf>,
    lookup: Rc<Lookup>,
    changed: Rc<dyn Fn()>,
) {
    let dialog = adw::Dialog::builder()
        .title("Add source")
        .content_width(560)
        .content_height(440)
        .build();
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&adw::HeaderBar::new());
    let page = adw::PreferencesPage::new();
    let address = adw::PreferencesGroup::new();
    let entry = adw::EntryRow::new();
    entry.set_title("GitHub owner/repository");
    entry.set_widget_name("source-repository");
    let lookup_button = gtk::Button::with_label("Look up");
    lookup_button.set_widget_name("source-lookup");
    lookup_button.set_valign(gtk::Align::Center);
    entry.add_suffix(&lookup_button);
    address.add(&entry);
    let status = adw::ActionRow::new();
    status.set_subtitle_lines(0);
    status.set_title("Read-only look-up");
    status.set_subtitle("No request sent.");
    address.add(&status);
    page.add(&address);
    let preview = adw::PreferencesGroup::new();
    preview.set_title("Available plugins");
    preview.set_visible(false);
    page.add(&preview);
    let actions = adw::PreferencesGroup::new();
    let row = adw::ActionRow::new();
    row.set_title("Save this source");
    let cancel = gtk::Button::with_label("Cancel");
    cancel.set_valign(gtk::Align::Center);
    let add = gtk::Button::with_label("Add source");
    add.set_widget_name("source-add");
    add.set_valign(gtk::Align::Center);
    add.add_css_class("suggested-action");
    row.add_suffix(&cancel);
    row.add_suffix(&add);
    actions.add(&row);
    page.add(&actions);
    toolbar.set_content(Some(&page));
    dialog.set_child(Some(&toolbar));
    let registry = root
        .clone()
        .ok_or(Problem("source data directory is unavailable"))
        .and_then(|root| registry::Job::start(root, Request::EffectiveList));
    let state = Rc::new(State {
        root,
        lookup,
        changed,
        registrations: RefCell::new(Vec::new()),
        registry: RefCell::new(registry.ok()),
        request: RefCell::new(None),
        candidate: RefCell::new(None),
        adding: Cell::new(false),
        loaded: Cell::new(false),
        closed: Cell::new(false),
        dialog: dialog.downgrade(),
        entry: entry.downgrade(),
        lookup_button: lookup_button.downgrade(),
        add: add.downgrade(),
        cancel: cancel.downgrade(),
        status: status.downgrade(),
        preview: preview.downgrade(),
        rows: RefCell::new(Vec::new()),
    });
    if state.registry.borrow().is_none() {
        state.loaded.set(true);
        state.report("Saved sources are unavailable; look-up is still read only");
    }
    lookup_button.connect_clicked({
        let state = state.clone();
        move |_| state.start_lookup()
    });
    entry.connect_entry_activated({
        let state = state.clone();
        move |_| state.start_lookup()
    });
    entry.connect_changed({
        let state = state.clone();
        move |entry| {
            entry.remove_css_class("error");
            state.candidate.borrow_mut().take();
            if let Some(preview) = state.preview.upgrade() {
                preview.set_visible(false);
            }
            state.controls();
        }
    });
    add.connect_clicked({
        let state = state.clone();
        move |_| state.save()
    });
    cancel.connect_clicked({
        let dialog = dialog.downgrade();
        move |_| {
            if let Some(dialog) = dialog.upgrade() {
                dialog.close();
            }
        }
    });
    dialog.connect_closed({
        let state = state.clone();
        move |_| {
            state.closed.set(true);
            state.request.borrow_mut().take();
            state.candidate.borrow_mut().take();
        }
    });
    state.controls();
    glib::timeout_add_local(std::time::Duration::from_millis(50), move || {
        if state.closed.get() || state.dialog.upgrade().is_none() {
            return glib::ControlFlow::Break;
        }
        state.poll();
        glib::ControlFlow::Continue
    });
    dialog.present(Some(parent));
}

impl State {
    fn report(&self, message: &str) {
        if let Some(status) = self.status.upgrade() {
            status.set_subtitle(&escape_display(message));
        }
    }

    fn controls(&self) {
        let busy = self.request.borrow().is_some() || self.registry.borrow().is_some();
        if let Some(entry) = self.entry.upgrade() {
            entry.set_sensitive(!busy);
        }
        if let Some(button) = self.lookup_button.upgrade() {
            button.set_sensitive(
                !busy
                    && self.loaded.get()
                    && self
                        .entry
                        .upgrade()
                        .is_some_and(|entry| !entry.text().trim().is_empty()),
            );
        }
        if let Some(add) = self.add.upgrade() {
            add.set_sensitive(
                !busy
                    && self.candidate.borrow().as_ref().is_some_and(|candidate| {
                        candidate.validate_source().is_ok() && !candidate.is_prerelease()
                    }),
            );
        }
        if let Some(cancel) = self.cancel.upgrade() {
            cancel.set_sensitive(!self.adding.get());
        }
        if let Some(dialog) = self.dialog.upgrade() {
            dialog.set_can_close(!self.adding.get());
        }
    }

    fn start_lookup(&self) {
        if self.request.borrow().is_some() || self.registry.borrow().is_some() || !self.loaded.get()
        {
            return;
        }
        let Some(entry) = self.entry.upgrade() else {
            return;
        };
        self.candidate.borrow_mut().take();
        entry.remove_css_class("error");
        if let Some(preview) = self.preview.upgrade() {
            preview.set_visible(false);
        }
        let started =
            Repository::parse(&entry.text()).and_then(|repository| (self.lookup)(repository));
        match started {
            Ok(job) => {
                *self.request.borrow_mut() = Some(job);
                self.report("Looking up…");
            }
            Err(problem) => {
                entry.add_css_class("error");
                self.report(problem.0);
            }
        }
        self.controls();
    }

    fn show(&self, inspection: Inspection) {
        let Some(preview) = self.preview.upgrade() else {
            return;
        };
        for row in self.rows.borrow_mut().drain(..) {
            preview.remove(&row);
        }
        for plugin in &inspection.catalogue.plugins {
            for version in &plugin.versions {
                let row = adw::ActionRow::new();
                row.set_title(&escape_display(&format!(
                    "{} — {}",
                    version.manifest.name, version.manifest.version
                )));
                row.set_subtitle(&escape_display(&version.manifest.description));
                row.set_subtitle_lines(1);
                preview.add(&row);
                self.rows.borrow_mut().push(row);
            }
        }
        preview.set_visible(true);
        let expected = self
            .registrations
            .borrow()
            .iter()
            .find(|source| source.origin.repository_id == inspection.repository_id)
            .cloned();
        if let Some(add) = self.add.upgrade() {
            add.set_label(match &expected {
                None => "Add source",
                Some(source)
                    if inspection.origin().compare(&source.origin) == Change::PublisherChanged =>
                {
                    "Approve publisher change"
                }
                Some(_) => "Save metadata refresh",
            });
        }
        self.report(&format!(
            "{}/{} · repository {} · publisher {}. Read-only preview; nothing installed or saved.",
            inspection.repository.owner,
            inspection.repository.name,
            inspection.repository_id,
            inspection.publisher_id
        ));
        *self.candidate.borrow_mut() = Some(inspection);
    }

    fn save(&self) {
        if self.closed.get() || self.request.borrow().is_some() || self.registry.borrow().is_some()
        {
            return;
        }
        let Some(inspection) = self.candidate.borrow().clone() else {
            return;
        };
        if inspection.is_prerelease() {
            self.report("Pre-release catalogues are not saved as source metadata.");
            return;
        }
        if let Err(problem) = inspection.validate_source() {
            self.report(problem.0);
            return;
        }
        let expected = self
            .registrations
            .borrow()
            .iter()
            .find(|source| source.origin.repository_id == inspection.repository_id)
            .cloned();
        let request = match expected {
            None => Request::Add(Box::new(inspection)),
            Some(expected)
                if !expected.is_official()
                    && inspection.origin().compare(&expected.origin)
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
        match self
            .root
            .clone()
            .ok_or(Problem("source data directory is unavailable"))
            .and_then(|root| registry::Job::start(root, request))
        {
            Ok(job) => {
                self.adding.set(true);
                *self.registry.borrow_mut() = Some(job);
                self.report("Saving source…");
            }
            Err(problem) => self.report(problem.0),
        }
        self.controls();
    }

    fn poll(&self) {
        let outcome = self
            .registry
            .borrow()
            .as_ref()
            .and_then(registry::Job::poll);
        if let Some(outcome) = outcome {
            self.registry.borrow_mut().take();
            self.loaded.set(true);
            match outcome {
                Ok(Outcome::EffectiveList {
                    registrations,
                    problem,
                }) => {
                    *self.registrations.borrow_mut() = registrations;
                    if let Some(problem) = problem {
                        self.report(problem.0);
                    }
                }
                Ok(Outcome::Changed {
                    durability,
                    problem,
                    ..
                }) => {
                    (self.changed)();
                    self.adding.set(false);
                    if let Some(problem) = problem {
                        self.report(problem.0);
                    } else if durability != crate::settings::Durability::Durable {
                        self.report("Source written; durability is not confirmed. Reopen Sources to inspect it.");
                    } else if let Some(dialog) = self.dialog.upgrade() {
                        dialog.set_can_close(true);
                        dialog.close();
                    }
                }
                Err(problem) => {
                    self.adding.set(false);
                    self.report(problem.0);
                }
                Ok(_) => {
                    self.adding.set(false);
                    self.report("Unexpected source storage response");
                }
            }
        }
        let outcome = self.request.borrow().as_ref().and_then(Job::poll);
        if let Some(outcome) = outcome {
            self.request.borrow_mut().take();
            match outcome {
                Ok(inspection) => self.show(inspection),
                Err(problem) => self.report(problem.0),
            }
        }
        self.controls();
    }
}
