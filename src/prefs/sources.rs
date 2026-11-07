//! explicit source checks share the selected catalogue channel. Registry
//! mutations retain the exact displayed registration so stale callbacks cannot
//! replace a newer publisher approval or remove the built-in source.

use super::*;
use crate::plugin::{
    data::Problem,
    install::{
        registry::{self, Outcome, Registration, Request},
        source::{Inspection, Job, RateLimit, Repository},
    },
    origin::Change,
};
use std::{
    cell::Cell,
    collections::{BTreeMap, VecDeque},
    path::PathBuf,
};

type Lookup = dyn Fn(Repository) -> Result<Job, Problem>;
type Refresh = dyn Fn(u64) -> Result<Job, Problem>;

#[derive(Clone)]
pub(crate) struct Services {
    pub root: Option<PathBuf>,
    pub lookup: Rc<Lookup>,
    pub refresh: Rc<Refresh>,
}

#[derive(Clone)]
enum Notice {
    Checking,
    Checked,
    Unsaved,
    Failed(String),
    Limited(RateLimit),
    PublisherChanged(Box<Inspection>),
}

enum Active {
    Loading(registry::Job),
    Checking { expected: Registration, job: Job },
    Saving { source: u64, job: registry::Job },
}

struct Controller {
    services: Services,
    changed: Rc<dyn Fn()>,
    prerelease: Rc<dyn Fn(Inspection)>,
    page: glib::WeakRef<adw::PreferencesPage>,
    group: glib::WeakRef<adw::PreferencesGroup>,
    banner: glib::WeakRef<adw::Banner>,
    all: glib::WeakRef<gtk::Button>,
    rows: RefCell<Vec<adw::ActionRow>>,
    registrations: RefCell<Vec<Registration>>,
    notices: RefCell<BTreeMap<u64, Notice>>,
    attempted_at: RefCell<BTreeMap<u64, u64>>,
    queue: RefCell<VecDeque<Registration>>,
    active: RefCell<Option<Active>>,
    loaded: Cell<bool>,
    reload_after: Cell<bool>,
}

fn age(source: &Registration) -> String {
    source.retrieved_at().map_or_else(
        || "No saved list".into(),
        |time| {
            let now = registry::now().unwrap_or(0);
            let text = super::plugin_manager::age_text(now, time);
            format!(
                "Saved list checked {text}{}",
                if source.stale_at(now) {
                    " · older than a day or clock changed"
                } else {
                    ""
                }
            )
        },
    )
}

fn description(
    source: &Registration,
    notice: Option<&Notice>,
    attempted_at: Option<u64>,
) -> String {
    let count = source
        .cached_inspection()
        .and_then(Result::ok)
        .map(|inspection| inspection.catalogue.plugins.len());
    let count = count.map_or_else(
        || "Plugin count unknown".into(),
        |count| format!("{count} plugins"),
    );
    let state = match notice {
        None => "Saved data only; not checked in this session".into(),
        Some(Notice::Checking) => "Checking…".into(),
        Some(Notice::Checked) => "Checked".into(),
        Some(Notice::Unsaved) => "Pre-release list checked; not saved".into(),
        Some(Notice::Failed(problem)) => {
            format!("Could not check: {problem}. Using the saved list.")
        }
        Some(Notice::Limited(limit)) => format!(
            "Rate limited · remaining {}{}",
            limit
                .remaining
                .map_or_else(|| "unknown".into(), |value| value.to_string()),
            limit
                .retry_after
                .map_or_else(String::new, |seconds| format!(
                    " · retry after {seconds} s; no retry scheduled"
                ))
        ),
        Some(Notice::PublisherChanged(_)) => {
            "Publisher changed. Review before replacing the saved list.".into()
        }
    };
    let last_attempt = attempted_at.map_or_else(String::new, |time| {
        format!(
            "\nLast check requested {}",
            super::plugin_manager::age_text(registry::now().unwrap_or(0), time)
        )
    });
    format!("{count} · {}{last_attempt}\n{state}", age(source))
}

pub(super) fn present(
    window: &adw::PreferencesWindow,
    services: Services,
    changed: Rc<dyn Fn()>,
    prerelease: Rc<dyn Fn(Inspection)>,
) {
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&adw::HeaderBar::new());
    let banner = adw::Banner::new("");
    toolbar.add_top_bar(&banner);
    let page = adw::PreferencesPage::new();
    let group = adw::PreferencesGroup::new();
    group.set_title("Sources");
    let all = gtk::Button::with_label("Check all");
    all.set_widget_name("sources-check-all");
    all.set_tooltip_text(Some(
        "Check sources in order and stop if GitHub refuses a rate limit",
    ));
    group.set_header_suffix(Some(&all));
    page.add(&group);
    let add_group = adw::PreferencesGroup::new();
    let add = adw::ActionRow::new();
    add.set_title("Add source");
    add.add_prefix(&gtk::Image::from_icon_name("list-add-symbolic"));
    add.set_activatable(true);
    add.set_widget_name("sources-add");
    add_group.add(&add);
    page.add(&add_group);
    toolbar.set_content(Some(&page));
    let nav = adw::NavigationPage::new(&toolbar, "Sources");
    nav.set_widget_name("plugin-sources-page");
    let controller = Rc::new(Controller {
        services: services.clone(),
        changed: changed.clone(),
        prerelease,
        page: page.downgrade(),
        group: group.downgrade(),
        banner: banner.downgrade(),
        all: all.downgrade(),
        rows: RefCell::new(Vec::new()),
        registrations: RefCell::new(Vec::new()),
        notices: RefCell::new(BTreeMap::new()),
        attempted_at: RefCell::new(BTreeMap::new()),
        queue: RefCell::new(VecDeque::new()),
        active: RefCell::new(None),
        loaded: Cell::new(false),
        reload_after: Cell::new(false),
    });
    all.connect_clicked({
        let controller = controller.clone();
        move |_| controller.check_all()
    });
    add.connect_activated({
        let controller = controller.clone();
        move |row| {
            let Some(window) = row
                .root()
                .and_then(|root| root.downcast::<gtk::Window>().ok())
            else {
                return;
            };
            let changed = Rc::new({
                let controller = Rc::downgrade(&controller);
                move || {
                    if let Some(controller) = controller.upgrade() {
                        (controller.changed)();
                        controller.reload();
                    }
                }
            });
            add_dialog(&window, controller.services.clone(), changed);
        }
    });
    controller.reload();
    window.push_subpage(&nav);
    // Removing a source destroys the focused row. Escape must still navigate
    // back from Sources when focus falls onto the window, rather than close
    // Preferences through its window-level shortcut.
    let keyboard = gtk::EventControllerKey::new();
    keyboard.set_propagation_phase(gtk::PropagationPhase::Capture);
    keyboard.connect_key_pressed({
        let window = window.downgrade();
        let nav = nav.downgrade();
        move |_, key, _, modifiers| {
            let (Some(window), Some(nav)) = (window.upgrade(), nav.upgrade()) else {
                return glib::Propagation::Proceed;
            };
            if key == gdk::Key::Escape
                && modifiers.is_empty()
                && nav.is_mapped()
                && window.visible_dialog().is_none()
                && window.pop_subpage()
            {
                return glib::Propagation::Stop;
            }
            glib::Propagation::Proceed
        }
    });
    window.add_controller(keyboard.clone());
    let window = window.downgrade();
    let mut keyboard = Some(keyboard);
    let nav = nav.downgrade();
    let mut closing = false;
    glib::timeout_add_local(std::time::Duration::from_millis(50), move || {
        if controller.page.upgrade().is_none()
            || nav.upgrade().is_none_or(|nav| nav.parent().is_none())
        {
            if !closing {
                closing = true;
                if let (Some(window), Some(keyboard)) = (window.upgrade(), keyboard.take()) {
                    window.remove_controller(&keyboard);
                }
                controller.queue.borrow_mut().clear();
                (controller.changed)();
            }
            // A registry commit can already be authoritative when navigation
            // removes its view. Keep observing it so the parent list reloads.
            if !matches!(*controller.active.borrow(), Some(Active::Saving { .. })) {
                controller.active.borrow_mut().take();
                return glib::ControlFlow::Break;
            }
        }
        controller.poll();
        glib::ControlFlow::Continue
    });
}

impl Controller {
    fn root_job(&self, request: Request) -> Result<registry::Job, Problem> {
        registry::Job::start(
            self.services
                .root
                .clone()
                .ok_or(Problem("source data directory is unavailable"))?,
            request,
        )
    }

    fn report(&self, text: &str) {
        if let Some(banner) = self.banner.upgrade() {
            banner.set_title(&escape_display(text));
            banner.set_revealed(!text.is_empty());
        }
    }

    fn reload(self: &Rc<Self>) {
        if self.active.borrow().is_some() {
            self.reload_after.set(true);
            return;
        }
        match self.root_job(Request::EffectiveList) {
            Ok(job) => *self.active.borrow_mut() = Some(Active::Loading(job)),
            Err(problem) => {
                self.loaded.set(true);
                self.report(problem.0);
            }
        }
        self.render();
    }

    fn current(&self, expected: &Registration) -> bool {
        self.registrations
            .borrow()
            .iter()
            .any(|source| source == expected)
    }

    fn check_all(self: &Rc<Self>) {
        if self.active.borrow().is_some() || !self.loaded.get() {
            return;
        }
        *self.queue.borrow_mut() = self.registrations.borrow().iter().cloned().collect();
        self.report("");
        self.next();
    }

    fn next(self: &Rc<Self>) {
        let next = self.queue.borrow_mut().pop_front();
        if let Some(expected) = next {
            self.check(expected);
        } else if self.reload_after.replace(false) {
            self.reload();
        } else {
            self.render();
        }
    }

    fn check(self: &Rc<Self>, expected: Registration) {
        if self.active.borrow().is_some() || !self.current(&expected) {
            return;
        }
        let id = expected.origin.repository_id;
        if let Ok(now) = registry::now() {
            self.attempted_at.borrow_mut().insert(id, now);
        }
        match (self.services.refresh)(id) {
            Ok(job) => {
                self.notices.borrow_mut().insert(id, Notice::Checking);
                *self.active.borrow_mut() = Some(Active::Checking { expected, job });
                self.render();
            }
            Err(problem) => {
                self.notices
                    .borrow_mut()
                    .insert(id, Notice::Failed(problem.to_string()));
                self.next();
            }
        }
    }

    fn save(self: &Rc<Self>, id: u64, request: Request) {
        match self.root_job(request) {
            Ok(job) => *self.active.borrow_mut() = Some(Active::Saving { source: id, job }),
            Err(problem) => {
                self.notices
                    .borrow_mut()
                    .insert(id, Notice::Failed(problem.to_string()));
                self.next();
            }
        }
        self.render();
    }

    fn remove(self: &Rc<Self>, expected: Registration, parent: &gtk::Window) {
        if expected.is_official() || !self.current(&expected) || self.active.borrow().is_some() {
            return;
        }
        let origin = expected.display_origin();
        let dialog = adw::AlertDialog::builder().heading("Remove this source?")
            .body(format!("Remove {}/{} from the saved sources? Installed plugins keep running and their settings are kept. Their updates will no longer be offered.", origin.owner, origin.repository)).build();
        dialog.add_response("cancel", "Cancel");
        dialog.add_response("remove", "Remove source");
        dialog.set_close_response("cancel");
        dialog.set_default_response(Some("cancel"));
        dialog.set_response_appearance("remove", adw::ResponseAppearance::Destructive);
        dialog.connect_response(None, {
            let controller = self.clone();
            let parent = parent.downgrade();
            move |_, response| {
                if response == "remove"
                    && parent.upgrade().is_some_and(|parent| parent.is_visible())
                    && controller
                        .page
                        .upgrade()
                        .is_some_and(|page| page.is_mapped())
                    && !expected.is_official()
                    && controller.current(&expected)
                    && controller.active.borrow().is_none()
                {
                    controller.save(
                        expected.origin.repository_id,
                        Request::Remove(expected.clone()),
                    );
                }
            }
        });
        dialog.present(Some(parent));
    }

    fn review(
        self: &Rc<Self>,
        expected: Registration,
        inspection: Inspection,
        parent: &gtk::Window,
    ) {
        if expected.is_official() || !self.current(&expected) || self.active.borrow().is_some() {
            return;
        }
        if inspection.is_prerelease() {
            let dialog = adw::AlertDialog::builder().heading("Check the stable channel before approving this publisher")
                .body("Pre-release catalogues are not saved. Choose Stable in Plugins, check this source again, then review its publisher change.").build();
            dialog.add_response("close", "Close");
            dialog.set_close_response("close");
            dialog.present(Some(parent));
            return;
        }
        let dialog = adw::AlertDialog::builder().heading("Approve this publisher change?")
            .body(format!("{}/{} changed publisher from {} to {}. Approving replaces its saved metadata. Installed plugins still need their own access review; this does not enable or restart them.", inspection.repository.owner, inspection.repository.name, expected.origin.publisher_id, inspection.publisher_id)).build();
        dialog.add_response("cancel", "Cancel");
        dialog.add_response("approve", "Approve publisher change");
        dialog.set_default_response(Some("cancel"));
        dialog.set_close_response("cancel");
        dialog.set_response_appearance("approve", adw::ResponseAppearance::Destructive);
        dialog.connect_response(None, {
            let controller = self.clone();
            let parent = parent.downgrade();
            move |_, response| {
                if response == "approve"
                    && parent.upgrade().is_some_and(|parent| parent.is_visible())
                    && controller
                        .page
                        .upgrade()
                        .is_some_and(|page| page.is_mapped())
                    && controller.current(&expected)
                    && controller.active.borrow().is_none()
                {
                    controller.save(
                        expected.origin.repository_id,
                        Request::ApprovePublisher {
                            expected: expected.clone(),
                            inspection: Box::new(inspection.clone()),
                        },
                    );
                }
            }
        });
        dialog.present(Some(parent));
    }

    fn render(self: &Rc<Self>) {
        let Some(group) = self.group.upgrade() else {
            return;
        };
        let busy = self.active.borrow().is_some();
        if let Some(all) = self.all.upgrade() {
            all.set_sensitive(!busy && self.loaded.get());
        }
        for row in self.rows.borrow_mut().drain(..) {
            group.remove(&row);
        }
        for source in registry::effective_sources(self.registrations.borrow().clone()) {
            let id = source.origin.repository_id;
            let origin = source.display_origin();
            let row = adw::ActionRow::new();
            row.set_widget_name(&format!("plugin-source:{id}"));
            row.set_title(&escape_display(&format!(
                "{}/{}{}",
                origin.owner,
                origin.repository,
                if source.is_official() {
                    " · Official"
                } else {
                    ""
                }
            )));
            row.set_subtitle_lines(0);
            let notice = self.notices.borrow().get(&id).cloned();
            row.set_subtitle(&escape_display(&description(
                &source,
                notice.as_ref(),
                self.attempted_at.borrow().get(&id).copied(),
            )));
            let check = gtk::Button::from_icon_name("view-refresh-symbolic");
            check.set_tooltip_text(Some("Check source"));
            check.set_valign(gtk::Align::Center);
            check.set_widget_name(&format!("source-check:{id}"));
            check.update_property(&[gtk::accessible::Property::Label(&format!(
                "Check {}/{}",
                origin.owner, origin.repository
            ))]);
            check.set_sensitive(!busy && self.loaded.get());
            check.connect_clicked({
                let controller = Rc::downgrade(self);
                let expected = source.clone();
                move |_| {
                    if let Some(controller) = controller.upgrade() {
                        controller.check(expected.clone());
                    }
                }
            });
            row.add_suffix(&check);
            if matches!(notice, Some(Notice::Checking)) {
                let spinner = gtk::Spinner::new();
                spinner.set_spinning(true);
                row.add_suffix(&spinner);
            }
            if let Some(Notice::PublisherChanged(inspection)) = notice {
                let review = gtk::Button::with_label("Review…");
                review.set_valign(gtk::Align::Center);
                review.set_sensitive(!busy);
                review.connect_clicked({
                    let controller = Rc::downgrade(self);
                    let expected = source.clone();
                    move |button| {
                        if let (Some(controller), Some(parent)) = (
                            controller.upgrade(),
                            button
                                .root()
                                .and_then(|root| root.downcast::<gtk::Window>().ok()),
                        ) {
                            controller.review(expected.clone(), *inspection.clone(), &parent);
                        }
                    }
                });
                row.add_suffix(&review);
            }
            if !source.is_official() {
                let remove = gtk::Button::from_icon_name("user-trash-symbolic");
                remove.set_valign(gtk::Align::Center);
                remove.set_tooltip_text(Some("Remove source"));
                remove.set_widget_name(&format!("source-remove:{id}"));
                remove.set_sensitive(!busy);
                remove.update_property(&[gtk::accessible::Property::Label(&format!(
                    "Remove {}/{}",
                    origin.owner, origin.repository
                ))]);
                remove.connect_clicked({
                    let controller = Rc::downgrade(self);
                    let expected = source.clone();
                    move |button| {
                        if let (Some(controller), Some(parent)) = (
                            controller.upgrade(),
                            button
                                .root()
                                .and_then(|root| root.downcast::<gtk::Window>().ok()),
                        ) {
                            controller.remove(expected.clone(), &parent);
                        }
                    }
                });
                row.add_suffix(&remove);
            }
            group.add(&row);
            self.rows.borrow_mut().push(row);
        }
    }

    fn poll(self: &Rc<Self>) {
        let active = self.active.borrow_mut().take();
        match active {
            None => {}
            Some(Active::Loading(job)) => match job.poll() {
                None => *self.active.borrow_mut() = Some(Active::Loading(job)),
                Some(Ok(Outcome::EffectiveList {
                    registrations,
                    problem,
                })) => {
                    *self.registrations.borrow_mut() = registrations;
                    self.loaded.set(true);
                    self.report(problem.map_or("", |problem| problem.0));
                    self.render();
                }
                Some(Err(problem)) => {
                    self.loaded.set(true);
                    self.report(problem.0);
                    self.render();
                }
                Some(Ok(_)) => {
                    self.loaded.set(true);
                    self.report("Unexpected source storage response");
                    self.render();
                }
            },
            Some(Active::Checking { expected, job }) => {
                match job.poll() {
                    None => *self.active.borrow_mut() = Some(Active::Checking { expected, job }),
                    Some(Err(problem)) => {
                        let id = expected.origin.repository_id;
                        let notice = if let Some(limit) = job.rate_limit() {
                            self.queue.borrow_mut().clear();
                            self.report("Check all stopped at the GitHub rate limit. No retry is scheduled.");
                            Notice::Limited(limit)
                        } else {
                            Notice::Failed(problem.to_string())
                        };
                        self.notices.borrow_mut().insert(id, notice);
                        self.next();
                    }
                    Some(Ok(inspection)) => self.observed(expected, inspection),
                }
            }
            Some(Active::Saving { source, job }) => match job.poll() {
                None => *self.active.borrow_mut() = Some(Active::Saving { source, job }),
                Some(Ok(Outcome::Changed {
                    registrations,
                    durability,
                    problem,
                })) => {
                    *self.registrations.borrow_mut() = registrations;
                    self.notices.borrow_mut().insert(source, Notice::Checked);
                    (self.changed)();
                    let text = problem
                        .map(|problem| problem.to_string())
                        .unwrap_or_else(|| {
                            if durability == crate::settings::Durability::Durable {
                                String::new()
                            } else {
                                "Source change written; durability is not confirmed.".into()
                            }
                        });
                    self.report(&text);
                    self.next();
                }
                Some(Err(problem)) => {
                    self.notices
                        .borrow_mut()
                        .insert(source, Notice::Failed(problem.to_string()));
                    self.next();
                }
                Some(Ok(_)) => {
                    self.report("Unexpected source storage response");
                    self.next();
                }
            },
        }
        if self.active.borrow().is_none()
            && self.queue.borrow().is_empty()
            && self.reload_after.replace(false)
        {
            self.reload();
        }
    }

    fn observed(self: &Rc<Self>, expected: Registration, inspection: Inspection) {
        let id = expected.origin.repository_id;
        let change = inspection.origin().compare(&expected.origin);
        if !self.current(&expected) || change == Change::IdentityChanged {
            self.notices.borrow_mut().insert(
                id,
                Notice::Failed(
                    "Source changed; open its current registration before checking again".into(),
                ),
            );
            self.next();
            return;
        }
        if let Err(problem) = inspection.validate_source() {
            self.notices
                .borrow_mut()
                .insert(id, Notice::Failed(problem.to_string()));
            self.next();
            return;
        }
        if change == Change::PublisherChanged {
            self.notices
                .borrow_mut()
                .insert(id, Notice::PublisherChanged(Box::new(inspection)));
            self.next();
            return;
        }
        if inspection.is_prerelease() {
            (self.prerelease)(inspection);
            self.notices.borrow_mut().insert(id, Notice::Unsaved);
            self.next();
        } else {
            self.save(
                id,
                Request::Refresh {
                    expected,
                    inspection: Box::new(inspection),
                },
            );
        }
    }
}

pub(super) fn add_dialog(parent: &gtk::Window, services: Services, changed: Rc<dyn Fn()>) {
    super::source_dialog::present(parent, services.root, services.lookup, changed);
}

#[cfg(test)]
mod tests;
