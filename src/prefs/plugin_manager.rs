//! the Plugins page: Installed and Available groups built from the saved
//! snapshots of every registered source. opening the page reads saved files
//! only; the network is used when the user presses Load or refresh, and
//! never in the background. rows retain identity and widget instances while
//! observations and search change.

use super::plugin_row::{Row, Shown};
use super::*;
use crate::plugin::data::Problem;
use crate::plugin::host::Overview;
use crate::plugin::install::listing::{
    self, Listing, Offer, SourceCatalogue, SourceState, Standing,
};
use crate::plugin::install::registry::{self, Outcome, Registration, Request};
use crate::plugin::install::source::{self, Inspection, InstallJob, Job as SourceJob, Selection};
use crate::plugin::origin::Change;
use crate::plugin::sandbox::level::Protection;
use std::cell::Cell;
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

pub(super) type FocusAfterRemoval = Rc<dyn Fn(&str)>;
type StartSource = dyn Fn(u64) -> Result<SourceJob, Problem>;
type StartInstall = dyn Fn(Selection) -> Result<InstallJob, Problem>;

/// The jobs the page starts, injectable so tests drive it without a network.
#[derive(Clone)]
pub(crate) struct Services {
    /// Where registered sources are stored.
    pub(crate) root: Option<PathBuf>,
    pub(crate) refresh: Rc<StartSource>,
    pub(crate) prerelease: Rc<StartSource>,
    pub(crate) install: Rc<StartInstall>,
}

impl Services {
    pub(super) fn production() -> Self {
        Self {
            root: registry::root(),
            refresh: Rc::new(SourceJob::refresh),
            prerelease: Rc::new(SourceJob::refresh_prerelease),
            install: Rc::new(|selection| {
                InstallJob::start(
                    selection,
                    crate::plugin::inventory::root()
                        .ok_or(Problem("managed data directory is unavailable"))?,
                )
            }),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Channel {
    Stable,
    Prerelease,
}

/// What the page last learned from an explicit load or refresh.
#[derive(Clone, PartialEq, Eq)]
enum Notice {
    Nothing,
    /// The source has no stable release.
    NoStable,
    /// A prerelease catalogue is shown and was not saved.
    Unsaved(String),
    Failed(String),
}

enum Refreshing {
    Fetching(SourceJob),
    Saving(registry::Job),
}

/// The state of one operation on a plugin, shown in its row.
#[derive(Clone, Debug)]
pub(super) enum Op {
    Running { phase: String, can_stop: bool },
    Done(String),
    Uncertain(String),
    Failed(String),
}

struct Running {
    job: Rc<InstallJob>,
    updating: bool,
}

pub(super) struct State {
    registrations: RefCell<Vec<Registration>>,
    catalogues: RefCell<Vec<SourceCatalogue>>,
    // Development prerelease catalogues are session-only and keyed by the
    // numeric source identity, so several sources never replace each other.
    overlay: RefCell<BTreeMap<u64, Inspection>>,
    loader: RefCell<Option<registry::Job>>,
    reload_requested: Cell<bool>,
    loaded: Cell<bool>,
    refreshing: RefCell<Option<Refreshing>>,
    channel: Cell<Channel>,
    notice: RefCell<Notice>,
    pub(super) listing: RefCell<Listing>,
    ops: RefCell<BTreeMap<String, Op>>,
    jobs: RefCell<BTreeMap<String, Running>>,
    dirty: Cell<bool>,
}

impl State {
    fn new(development: bool) -> Self {
        Self {
            registrations: RefCell::new(Vec::new()),
            catalogues: RefCell::new(Vec::new()),
            overlay: RefCell::new(BTreeMap::new()),
            loader: RefCell::new(None),
            reload_requested: Cell::new(false),
            loaded: Cell::new(false),
            refreshing: RefCell::new(None),
            channel: Cell::new(if development {
                Channel::Prerelease
            } else {
                Channel::Stable
            }),
            notice: RefCell::new(Notice::Nothing),
            listing: RefCell::new(Listing::default()),
            ops: RefCell::new(BTreeMap::new()),
            jobs: RefCell::new(BTreeMap::new()),
            dirty: Cell::new(true),
        }
    }

    pub(super) fn op(&self, key: &str) -> Option<Op> {
        self.ops.borrow().get(key).cloned()
    }

    fn set_op(&self, key: &str, op: Op) {
        self.ops.borrow_mut().insert(key.to_owned(), op);
        self.dirty.set(true);
    }
}

pub(super) struct Env {
    pub ctx: PreferencesContext,
    pub services: Services,
    pub state: State,
    lookup: Rc<super::source_dialog::Lookup>,
    #[cfg(test)]
    observation: Option<Rc<RefCell<Option<Inspection>>>>,
    /// Moves focus to where a plugin that just left Installed now is in the
    /// list, or to the search field; set once the page exists.
    pub focus: RefCell<Option<FocusAfterRemoval>>,
}

impl Env {
    fn new(
        ctx: &PreferencesContext,
        services: Services,
        lookup: Rc<super::source_dialog::Lookup>,
    ) -> Self {
        Self {
            ctx: ctx.clone(),
            services,
            state: State::new(crate::plugin::development::enabled()),
            lookup,
            #[cfg(test)]
            observation: None,
            focus: RefCell::new(None),
        }
    }

    /// Drops a finished operation's message, so a later state is not
    /// described by an old line.
    pub(super) fn clear_note(&self, key: &str) {
        let finished = matches!(
            self.state.ops.borrow().get(key),
            Some(Op::Done(_) | Op::Uncertain(_) | Op::Failed(_))
        );
        if finished {
            self.state.ops.borrow_mut().remove(key);
            self.state.dirty.set(true);
        }
    }
}

pub(super) fn age_text(now: u64, retrieved_at: u64) -> String {
    match now.checked_sub(retrieved_at) {
        None => "at an unknown time".into(),
        Some(seconds) if seconds < 60 => "just now".into(),
        Some(seconds) if seconds < 3600 => format!("{} min ago", seconds / 60),
        Some(seconds) if seconds < 86_400 => format!("{} h ago", seconds / 3600),
        Some(seconds) => format!("{} d ago", seconds / 86_400),
    }
}

/// The first row of the page while this instance runs in development mode, so
/// the mode is visible wherever its actions appear. The banner only informs:
/// the mode is fixed by the command line and cannot be switched here.
fn development_banner() -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::new();
    let banner = adw::Banner::new(
        "Plugin development mode: started with --plugin-development. This is not saved; start Among without the flag to leave it.",
    );
    banner.set_revealed(true);
    group.add(&banner);
    group
}

/// The development-only action that installs packages from a local schema2
/// catalogue. An ordinary instance never builds this row.
fn local_catalogue_row(ctx: &PreferencesContext) -> adw::ActionRow {
    let row = adw::ActionRow::builder()
        .title("Install from a local catalogue")
        .subtitle("Development mode: choose an among-plugins.json with its package files next to it. Packages are checked like release packages but have no repository, release or tag provenance.")
        .build();
    row.add_prefix(&gtk::Image::from_icon_name("folder-symbolic"));
    let button = gtk::Button::with_label("Install from local catalogue…");
    button.set_valign(gtk::Align::Center);
    button.connect_clicked({
        let changed = ctx.installations_changed.clone();
        let details = ctx.plugin_details.clone();
        move |button| {
            if let Some(parent) = button
                .root()
                .and_then(|root| root.downcast::<gtk::Window>().ok())
            {
                super::local_catalogue::present(&parent, changed.clone(), details.clone());
            }
        }
    });
    row.add_suffix(&button);
    row.set_activatable_widget(Some(&button));
    row
}

struct Ui {
    page: glib::WeakRef<adw::PreferencesPage>,
    installed: adw::PreferencesGroup,
    available: adw::PreferencesGroup,
    retained: adw::PreferencesGroup,
    known_installed: Rc<RefCell<Option<BTreeSet<String>>>>,
    search: gtk::SearchEntry,
    empty: adw::ActionRow,
    status: adw::StatusPage,
    status_button: gtk::Button,
    age: gtk::Label,
    spinner: gtk::Spinner,
    refresh: gtk::Button,
    installed_rows: RefCell<BTreeMap<String, Row>>,
    available_rows: RefCell<BTreeMap<String, Row>>,
}

pub(super) fn page(ctx: &PreferencesContext) -> adw::PreferencesPage {
    page_with(ctx, Services::production())
}

pub(super) fn page_with(ctx: &PreferencesContext, services: Services) -> adw::PreferencesPage {
    page_with_lookup(ctx, services, Rc::new(SourceJob::start))
}

pub(super) fn page_with_lookup(
    ctx: &PreferencesContext,
    services: Services,
    lookup: Rc<super::source_dialog::Lookup>,
) -> adw::PreferencesPage {
    build_page(Rc::new(Env::new(ctx, services, lookup)))
}

#[cfg(test)]
pub(super) fn page_with_observation(
    ctx: &PreferencesContext,
    services: Services,
    observation: Rc<RefCell<Option<Inspection>>>,
) -> adw::PreferencesPage {
    let mut env = Env::new(ctx, services, Rc::new(SourceJob::start));
    env.observation = Some(observation);
    build_page(Rc::new(env))
}

fn build_page(env: Rc<Env>) -> adw::PreferencesPage {
    let ctx = &env.ctx;
    let development = crate::plugin::development::enabled();
    let page = adw::PreferencesPage::new();
    page.set_title("Plugins");
    page.set_icon_name(Some("application-x-addon-symbolic"));
    if development {
        page.add(&development_banner());
    }
    add_storage_status(&page, ctx);

    let search_group = adw::PreferencesGroup::new();
    let search = gtk::SearchEntry::new();
    search.set_placeholder_text(Some("Search plugins"));
    search.update_property(&[gtk::accessible::Property::Label(
        "Search plugins by name, identity or origin",
    )]);
    search_group.add(&search);
    page.add(&search_group);

    let installed = adw::PreferencesGroup::new();
    installed.set_title("Installed");
    installed.set_visible(false);
    page.add(&installed);

    let available = adw::PreferencesGroup::new();
    available.set_title("Available");
    let header = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    let age = gtk::Label::new(None);
    age.add_css_class("dim-label");
    age.add_css_class("caption");
    header.append(&age);
    let spinner = gtk::Spinner::new();
    header.append(&spinner);
    if development {
        header.append(&channel_switch(&env));
    }
    let refresh = gtk::Button::from_icon_name("view-refresh-symbolic");
    refresh.add_css_class("flat");
    refresh.set_tooltip_text(Some("Check for new plugins"));
    refresh.update_property(&[gtk::accessible::Property::Label("Check for new plugins")]);
    refresh.set_widget_name("plugins-refresh");
    header.append(&refresh);
    let add = gtk::Button::from_icon_name("list-add-symbolic");
    add.add_css_class("flat");
    add.set_tooltip_text(Some("Add a plugin source"));
    add.update_property(&[gtk::accessible::Property::Label("Add a plugin source")]);
    add.set_widget_name("plugins-add-source");
    header.append(&add);
    available.set_header_suffix(Some(&header));
    let status = adw::StatusPage::new();
    status.add_css_class("compact");
    let status_button = gtk::Button::with_label("Load");
    status_button.add_css_class("pill");
    status_button.set_halign(gtk::Align::Center);
    status.set_child(Some(&status_button));
    available.add(&status);
    page.add(&available);

    let sources = adw::PreferencesGroup::new();
    let sources_row = adw::ActionRow::new();
    sources_row.set_title("Sources");
    sources_row.set_widget_name("plugins-sources");
    sources_row.add_suffix(&gtk::Image::from_icon_name("go-next-symbolic"));
    sources_row.set_activatable(true);
    sources_row.connect_activated({
        let env = env.clone();
        move |row| {
            if let Some(window) = row
                .root()
                .and_then(|root| root.downcast::<adw::PreferencesWindow>().ok())
            {
                super::sources::present(
                    &window,
                    source_services(&env),
                    source_changed(&env),
                    Rc::new({
                        let env = env.clone();
                        move |inspection| observed(&env, inspection)
                    }),
                );
            }
        }
    });
    sources.add(&sources_row);
    page.add(&sources);

    if development {
        let development_group = adw::PreferencesGroup::new();
        development_group.add(&local_catalogue_row(ctx));
        page.add(&development_group);
    }

    let retained = adw::PreferencesGroup::new();
    let known_installed = Rc::new(RefCell::new(None));
    let retained_row = adw::ActionRow::builder()
        .title("Settings of removed plugins")
        .subtitle("Review and separately delete local settings of uninstalled plugins.")
        .build();
    retained_row.add_suffix(&gtk::Image::from_icon_name("go-next-symbolic"));
    retained_row.set_activatable(true);
    retained_row.connect_activated({
        let ctx = ctx.clone();
        let known_installed = known_installed.clone();
        move |row| {
            if let Some(parent) = row
                .root()
                .and_then(|root| root.downcast::<gtk::Window>().ok())
            {
                let window = super::retained_settings::present(&parent, &ctx);
                window.connect_destroy({
                    let known_installed = known_installed.clone();
                    move |_| *known_installed.borrow_mut() = None
                });
            }
        }
    });
    retained.add(&retained_row);
    retained.set_visible(false);
    page.add(&retained);

    let empty_group = adw::PreferencesGroup::new();
    let empty = adw::ActionRow::new();
    empty.set_title("No matching plugins");
    empty.set_subtitle("Try a different name, identity or origin, or clear the search.");
    empty.set_visible(false);
    empty_group.add(&empty);
    page.add(&empty_group);

    let ui = Rc::new(Ui {
        page: page.downgrade(),
        installed,
        available,
        retained,
        known_installed,
        search,
        empty,
        status,
        status_button: status_button.clone(),
        age,
        spinner,
        refresh: refresh.clone(),
        installed_rows: RefCell::new(BTreeMap::new()),
        available_rows: RefCell::new(BTreeMap::new()),
    });
    for button in [&refresh, &status_button] {
        button.connect_clicked({
            let env = env.clone();
            move |_| start_refresh(&env)
        });
    }
    add.connect_clicked({
        let env = env.clone();
        move |button| {
            if let Some(parent) = button
                .root()
                .and_then(|root| root.downcast::<gtk::Window>().ok())
            {
                super::sources::add_dialog(&parent, source_services(&env), source_changed(&env));
            }
        }
    });
    ui.search.connect_search_changed({
        let env = env.clone();
        let ui = Rc::downgrade(&ui);
        move |_| {
            if let Some(ui) = ui.upgrade() {
                reconcile(&env, &ui);
            }
        }
    });
    *env.focus.borrow_mut() = Some(Rc::new({
        let env = Rc::downgrade(&env);
        let ui = Rc::downgrade(&ui);
        move |key: &str| {
            let (Some(env), Some(ui)) = (env.upgrade(), ui.upgrade()) else {
                return;
            };
            reconcile(&env, &ui);
            let row = ui.available_rows.borrow();
            match row.get(key) {
                Some(row) if row.row.is_visible() => {
                    row.row.grab_focus();
                }
                _ => {
                    ui.search.grab_focus();
                }
            }
        }
    }));
    start_load(&env);
    reconcile(&env, &ui);
    let mut ticks = 0_u32;
    glib::timeout_add_local(std::time::Duration::from_millis(100), move || {
        if ui.page.upgrade().is_none() {
            abandon(&env);
            return glib::ControlFlow::Break;
        }
        ticks = ticks.wrapping_add(1);
        poll_operations(&env);
        poll_sources(&env);
        if env.state.dirty.replace(false) || ticks.is_multiple_of(5) {
            reconcile(&env, &ui);
        }
        glib::ControlFlow::Continue
    });
    page
}

/// Two linked toggle buttons choosing which release channel a refresh reads.
fn channel_switch(env: &Rc<Env>) -> gtk::Box {
    let switch = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    switch.add_css_class("linked");
    switch.set_valign(gtk::Align::Center);
    let stable = gtk::ToggleButton::with_label("Stable");
    let prerelease = gtk::ToggleButton::with_label("Pre-release");
    prerelease.set_group(Some(&stable));
    prerelease.set_active(true);
    stable.set_tooltip_text(Some("Refresh reads the latest stable release"));
    prerelease.set_tooltip_text(Some("Refresh reads the newest prerelease; it is not saved"));
    for (button, channel) in [
        (&stable, Channel::Stable),
        (&prerelease, Channel::Prerelease),
    ] {
        button.connect_toggled({
            let env = env.clone();
            move |button| {
                if button.is_active() {
                    env.state.channel.set(channel);
                }
            }
        });
        switch.append(button);
    }
    switch.set_widget_name("plugins-channel");
    switch
}

fn start_load(env: &Rc<Env>) {
    if env.state.loader.borrow().is_some() {
        env.state.reload_requested.set(true);
        return;
    }
    let Some(root) = env.services.root.clone() else {
        env.state.loaded.set(true);
        *env.state.notice.borrow_mut() =
            Notice::Failed("the plugin source directory is unavailable".into());
        rebuild_catalogues(env);
        return;
    };
    match registry::Job::start(root, Request::EffectiveList) {
        Ok(job) => *env.state.loader.borrow_mut() = Some(job),
        Err(problem) => {
            env.state.loaded.set(true);
            *env.state.notice.borrow_mut() = Notice::Failed(problem.to_string());
            rebuild_catalogues(env);
        }
    }
}

fn source_services(env: &Rc<Env>) -> super::sources::Services {
    super::sources::Services {
        root: env.services.root.clone(),
        lookup: env.lookup.clone(),
        refresh: Rc::new({
            let env = env.clone();
            move |id| match env.state.channel.get() {
                Channel::Stable => (env.services.refresh)(id),
                Channel::Prerelease => (env.services.prerelease)(id),
            }
        }),
    }
}

fn source_changed(env: &Rc<Env>) -> Rc<dyn Fn()> {
    let env = env.clone();
    Rc::new(move || {
        // A saved source mutation invalidates session-only offers. Re-read
        // locally now, so Add/Remove is reflected without reopening Plugins.
        if env.state.channel.get() == Channel::Stable {
            env.state.overlay.borrow_mut().clear();
        }
        *env.state.notice.borrow_mut() = Notice::Nothing;
        start_load(&env);
    })
}

/// Turns the registrations into catalogues once per change, so the
/// periodic reconciliation never decodes snapshots again.
fn rebuild_catalogues(env: &Env) {
    let state = &env.state;
    let mut catalogues: Vec<SourceCatalogue> =
        registry::effective_sources(state.registrations.borrow().clone())
            .iter()
            .map(SourceCatalogue::from_registration)
            .collect();
    for source in &mut catalogues {
        if let Some(overlay) = state.overlay.borrow().get(&source.origin.repository_id) {
            if matches!(
                overlay.origin().compare(&source.origin),
                Change::Unchanged | Change::Renamed
            ) {
                *source = SourceCatalogue::from_inspection(overlay.clone(), source.official);
            }
        }
    }
    *state.catalogues.borrow_mut() = catalogues;
    state.dirty.set(true);
}

fn start_refresh(env: &Rc<Env>) {
    let state = &env.state;
    if state.refreshing.borrow().is_some() {
        return;
    }
    #[cfg(test)]
    if let Some(observation) = &env.observation {
        observation.borrow_mut().take();
    }
    let repository = registry::official_source().origin.repository_id;
    let channel = state.channel.get();
    let started = match channel {
        Channel::Stable => (env.services.refresh)(repository),
        Channel::Prerelease => (env.services.prerelease)(repository),
    };
    match started {
        Ok(job) => {
            *state.refreshing.borrow_mut() = Some(Refreshing::Fetching(job));
            *state.notice.borrow_mut() = Notice::Nothing;
        }
        Err(problem) => *state.notice.borrow_mut() = Notice::Failed(problem.to_string()),
    }
    state.dirty.set(true);
}

fn poll_sources(env: &Rc<Env>) {
    let state = &env.state;
    let finished = state.loader.borrow().as_ref().and_then(registry::Job::poll);
    if let Some(outcome) = finished {
        state.loader.borrow_mut().take();
        state.loaded.set(true);
        match outcome {
            Ok(Outcome::EffectiveList {
                registrations,
                problem,
            }) => {
                state.overlay.borrow_mut().retain(|id, inspection| {
                    registrations.iter().any(|source| {
                        source.origin.repository_id == *id
                            && matches!(
                                inspection.origin().compare(&source.origin),
                                Change::Unchanged | Change::Renamed
                            )
                    })
                });
                *state.registrations.borrow_mut() = registrations;
                if let Some(problem) = problem {
                    *state.notice.borrow_mut() = Notice::Failed(problem.to_string());
                }
            }
            Ok(_) => {}
            Err(problem) => *state.notice.borrow_mut() = Notice::Failed(problem.to_string()),
        }
        rebuild_catalogues(env);
        if state.reload_requested.replace(false) {
            start_load(env);
        }
    }
    let current = state.refreshing.borrow_mut().take();
    match current {
        Some(Refreshing::Fetching(job)) => match job.poll() {
            None => *state.refreshing.borrow_mut() = Some(Refreshing::Fetching(job)),
            Some(Ok(inspection)) => observed(env, inspection),
            Some(Err(problem)) => {
                *state.notice.borrow_mut() = if problem == source::NO_STABLE_RELEASE {
                    Notice::NoStable
                } else {
                    Notice::Failed(problem.to_string())
                };
                state.dirty.set(true);
            }
        },
        Some(Refreshing::Saving(job)) => match job.poll() {
            None => *state.refreshing.borrow_mut() = Some(Refreshing::Saving(job)),
            Some(Ok(Outcome::Changed {
                registrations,
                problem,
                ..
            })) => {
                *state.registrations.borrow_mut() = registrations;
                *state.notice.borrow_mut() = problem
                    .map(|problem| Notice::Failed(problem.to_string()))
                    .unwrap_or(Notice::Nothing);
                rebuild_catalogues(env);
            }
            Some(Ok(_)) => rebuild_catalogues(env),
            Some(Err(problem)) => {
                *state.notice.borrow_mut() = Notice::Failed(problem.to_string());
                rebuild_catalogues(env);
            }
        },
        None => {}
    }
}

/// A refresh read a catalogue. A prerelease one is shown for this page only,
/// because the source cache refuses to keep it; a stable one is saved, and the
/// list changes when the saved snapshot comes back.
fn observed(env: &Rc<Env>, inspection: Inspection) {
    let state = &env.state;
    if inspection.is_prerelease() {
        let expected = registry::effective_sources(state.registrations.borrow().clone())
            .into_iter()
            .find(|source| source.origin.repository_id == inspection.repository_id);
        let Some(expected) = expected else {
            *state.notice.borrow_mut() =
                Notice::Failed("Source was removed; its prerelease was not kept.".into());
            state.dirty.set(true);
            return;
        };
        let change = inspection.origin().compare(&expected.origin);
        let refusal = inspection
            .validate_source()
            .err()
            .map(|problem| problem.to_string())
            .or_else(|| {
                matches!(change, Change::IdentityChanged | Change::PublisherChanged)
                    .then(|| change.description().to_owned())
            });
        if let Some(refusal) = refusal {
            *state.notice.borrow_mut() = Notice::Failed(refusal);
            state.dirty.set(true);
            return;
        }
        #[cfg(test)]
        if state.channel.get() == Channel::Prerelease {
            if let Some(observation) = &env.observation {
                *observation.borrow_mut() = Some(inspection.clone());
            }
        }
        state
            .overlay
            .borrow_mut()
            .insert(inspection.repository_id, inspection);
        *state.notice.borrow_mut() =
            Notice::Unsaved("Showing the newest pre-release catalogue. It is not saved.".into());
        rebuild_catalogues(env);
        return;
    }
    state.overlay.borrow_mut().remove(&inspection.repository_id);
    let expected = state
        .registrations
        .borrow()
        .iter()
        .find(|registration| registration.is_official())
        .cloned()
        .unwrap_or_else(registry::official_source);
    let started = env
        .services
        .root
        .clone()
        .ok_or(Problem("the plugin source directory is unavailable"))
        .and_then(|root| {
            registry::Job::start(
                root,
                Request::Refresh {
                    expected,
                    inspection: Box::new(inspection),
                },
            )
        });
    match started {
        Ok(job) => *state.refreshing.borrow_mut() = Some(Refreshing::Saving(job)),
        Err(problem) => *state.notice.borrow_mut() = Notice::Failed(problem.to_string()),
    }
    rebuild_catalogues(env);
}

/// The page is gone. A package operation that already began committing still
/// has to reach the host; one that has not is cancelled, as a closed window
/// always did.
fn abandon(env: &Rc<Env>) {
    let jobs = std::mem::take(&mut *env.state.jobs.borrow_mut());
    if jobs.is_empty() {
        return;
    }
    for running in jobs.values() {
        running.job.cancel();
    }
    let changed = env.ctx.installations_changed.clone();
    let mut jobs = jobs;
    glib::timeout_add_local(std::time::Duration::from_millis(50), move || {
        jobs.retain(|_, running| match running.job.poll() {
            Some(outcome) => {
                if outcome.is_ok() {
                    changed();
                }
                false
            }
            None => true,
        });
        if jobs.is_empty() {
            glib::ControlFlow::Break
        } else {
            glib::ControlFlow::Continue
        }
    });
}

fn poll_operations(env: &Rc<Env>) {
    let state = &env.state;
    let keys: Vec<String> = state.jobs.borrow().keys().cloned().collect();
    for key in keys {
        let polled = {
            let jobs = state.jobs.borrow();
            let Some(running) = jobs.get(&key) else {
                continue;
            };
            (
                running.job.poll(),
                running.job.phase(),
                running.updating,
                running.job.can_cancel(),
            )
        };
        let (outcome, phase, updating, can_stop) = polled;
        let Some(outcome) = outcome else {
            state.set_op(
                &key,
                Op::Running {
                    phase: phase.into(),
                    can_stop,
                },
            );
            continue;
        };
        state.jobs.borrow_mut().remove(&key);
        match outcome {
            Ok(commit) => {
                (env.ctx.installations_changed)();
                state.set_op(&key, match commit.durability {
                    crate::settings::Durability::Durable if updating => Op::Done("Updated. The previous version is kept; the plugin stays switched on or off as you left it.".into()),
                    crate::settings::Durability::Durable => Op::Done("Installed and switched off. Use the switch to turn it on.".into()),
                    _ => Op::Uncertain("Saved, but the write is not confirmed. Check the installed list before retrying.".into()),
                });
            }
            Err(problem) => state.set_op(&key, Op::Failed(problem.to_string())),
        }
    }
}

pub(super) fn listed_offer(env: &Env, key: &str) -> Option<Offer> {
    env.state
        .listing
        .borrow()
        .available
        .iter()
        .find(|offer| offer.key() == key)
        .cloned()
}

fn start_install(env: &Rc<Env>, key: &str, selection: Selection, updating: bool) {
    match (env.services.install)(selection) {
        Ok(job) => {
            let phase = job.phase().to_owned();
            env.state.jobs.borrow_mut().insert(
                key.to_owned(),
                Running {
                    job: Rc::new(job),
                    updating,
                },
            );
            env.state.set_op(
                key,
                Op::Running {
                    phase,
                    can_stop: true,
                },
            );
        }
        Err(problem) => env.state.set_op(key, Op::Failed(problem.to_string())),
    }
}

pub(super) fn install(env: &Rc<Env>, key: &str) {
    if let Some(Offer {
        selection: Ok(selection),
        ..
    }) = listed_offer(env, key)
    {
        start_install(env, key, selection, false);
    }
}

pub(super) fn stop(env: &Rc<Env>, key: &str) {
    if let Some(running) = env.state.jobs.borrow().get(key) {
        running.job.cancel();
    }
    env.state.dirty.set(true);
}

/// Asks for confirmation, then installs the newer or replacing version. The
/// confirmation text is the selection's own review, so what the user reads is
/// what is applied, and the installed generation is checked again at the
/// confirmation.
pub(super) fn update(env: &Rc<Env>, key: &str, window: Option<adw::PreferencesWindow>) {
    let Some((entry, _)) = (env.ctx.plugin_details)(key) else {
        return;
    };
    let standing = env
        .state
        .listing
        .borrow()
        .installed
        .iter()
        .find(|plugin| plugin.key == key)
        .map(|plugin| plugin.standing.clone());
    let (selection, replacing) = match standing {
        Some(Standing::Update(selection)) => (*selection, false),
        Some(Standing::Replace(selection)) => (*selection, true),
        _ => return,
    };
    let dialog = adw::AlertDialog::builder()
        .heading(if replacing {
            "Replace this plugin with the prerelease?"
        } else {
            "Apply this plugin update?"
        })
        .body(selection.review_description())
        .build();
    dialog.add_response("cancel", "Cancel");
    dialog.add_response("apply", if replacing { "Replace" } else { "Apply update" });
    dialog.set_default_response(Some("cancel"));
    dialog.set_close_response("cancel");
    let expected = entry.receipt.clone();
    dialog.connect_response(None, {
        let env = env.clone();
        let key = key.to_owned();
        move |_, response| {
            if response != "apply" {
                return;
            }
            let Some((entry, stored)) =
                (env.ctx.plugin_details)(&key).filter(|(entry, _)| entry.receipt == expected)
            else {
                env.state.set_op(
                    &key,
                    Op::Failed(
                        "The installed version changed; open the plugin again before updating."
                            .into(),
                    ),
                );
                return;
            };
            match selection.clone().for_installed(&entry, stored.as_ref()) {
                Ok(selection) => {
                    let updating = selection.is_update();
                    start_install(&env, &key, selection, updating);
                }
                Err(problem) => env.state.set_op(&key, Op::Failed(problem.to_string())),
            }
        }
    });
    dialog.present(window.as_ref());
}

/// Uninstall (or recover the previous generation). The application asks for
/// confirmation and reports progress back; nothing changes in the row until
/// the user confirms.
pub(super) fn package_action(
    env: &Rc<Env>,
    key: &str,
    action: crate::plugin::install::mutation::Action,
    parent: Option<gtk::Window>,
) {
    let Some((entry, _)) = (env.ctx.plugin_details)(key) else {
        return;
    };
    let events: Rc<dyn Fn(PackageEvent)> = Rc::new({
        let env = env.clone();
        let key = key.to_owned();
        move |event| match event {
            PackageEvent::Started => env.state.set_op(
                &key,
                Op::Running {
                    phase: "Starting".into(),
                    can_stop: false,
                },
            ),
            PackageEvent::Phase(phase) => env.state.set_op(
                &key,
                Op::Running {
                    phase,
                    can_stop: false,
                },
            ),
            PackageEvent::Finished(Ok(durability)) => {
                if durability == crate::settings::Durability::Durable {
                    if action == crate::plugin::install::mutation::Action::RestorePrevious {
                        env.state.set_op(
                            &key,
                            Op::Done("The previous version is selected. The enabled choice is kept; access review and admission still apply.".into()),
                        );
                    } else {
                        // the plugin left Installed: its row and page go away.
                        env.state.ops.borrow_mut().remove(&key);
                        env.state.dirty.set(true);
                    }
                } else {
                    env.state.set_op(
                        &key,
                        Op::Uncertain(
                            "Saved, but the write is not confirmed. Check the installed list before retrying."
                                .into(),
                        ),
                    );
                }
            }
            PackageEvent::Finished(Err(problem)) => env.state.set_op(&key, Op::Failed(problem)),
        }
    });
    (env.ctx.package_action)(entry.receipt, action, parent, events);
}

pub(super) fn uninstall(env: &Rc<Env>, key: &str, parent: Option<gtk::Window>) {
    package_action(
        env,
        key,
        crate::plugin::install::mutation::Action::Uninstall,
        parent,
    );
}

/// The consent question for a plugin whose start was refused at a reduced
/// protection level. The offer is read again at the click: a stale row cannot
/// grant a level the kernel no longer offers.
pub(super) fn allow_reduced(env: &Rc<Env>, key: &str, button: &gtk::Button) {
    let Some((info, offer)) = (env.ctx.plugin_inventory)()
        .into_iter()
        .find(|info| info.key == key)
        .and_then(|info| info.consent_offer.map(|offer| (info, offer)))
    else {
        return;
    };
    let files = match offer {
        Protection::SeccompOnly => {
            "This kernel has no usable Landlock, so the plugin keeps every file \
             access of your user account: only the seccomp limits apply."
        }
        _ => {
            "File access is limited to the plugin's data directory and the \
             system runtime, except that this kernel does not restrict \
             truncating files."
        }
    };
    let dialog = adw::AlertDialog::builder()
        .heading(format!("Run {} with reduced protection?", info.name))
        .body(format!(
            "This kernel can only give the plugin {}. The plugin is still \
             denied sockets and control of other processes. {files} The \
             permission applies to this plugin only and is asked again if \
             the protection gets lower.",
            offer.label()
        ))
        .build();
    dialog.add_response("cancel", "Cancel");
    dialog.add_response("allow", "Allow reduced protection");
    dialog.set_response_appearance("allow", adw::ResponseAppearance::Destructive);
    dialog.set_close_response("cancel");
    let key = key.to_owned();
    let allow_protection = env.ctx.allow_reduced_protection.clone();
    let window = button
        .root()
        .and_then(|root| root.downcast::<gtk::Window>().ok());
    let parent = window.clone();
    dialog.connect_response(None, move |_, response| {
        if response == "allow" {
            if let Err(problem) = allow_protection(&key, &offer.token()) {
                eprintln!("among: {problem}");
                // the row may be gone by now, so the message goes to a dialog
                // on the window that showed the question.
                let failure = adw::AlertDialog::builder()
                    .heading("Could not allow reduced protection")
                    .body(problem.as_str())
                    .build();
                failure.add_response("close", "Close");
                failure.set_close_response("close");
                failure.present(parent.as_ref());
            }
        }
    });
    dialog.present(window.as_ref());
}

fn sort_inventory(inventory: &mut [Overview]) {
    inventory.sort_by(|first, second| {
        first
            .name
            .cmp(&second.name)
            .then_with(|| first.key.cmp(&second.key))
    });
}

fn now() -> u64 {
    registry::now().unwrap_or(0)
}

fn reconcile(env: &Rc<Env>, ui: &Ui) {
    let state = &env.state;
    let mut inventory = (env.ctx.plugin_inventory)();
    sort_inventory(&mut inventory);
    let details: Vec<_> = inventory
        .iter()
        .map(|info| (env.ctx.plugin_details)(&info.key))
        .collect();
    let installed: Vec<_> = details
        .iter()
        .flatten()
        .map(|(entry, stored)| listing::Installed {
            entry,
            stored: stored.as_ref(),
        })
        .collect();
    let built = listing::assemble(&state.catalogues.borrow(), &installed, now());
    drop(installed);
    let several = built.several_sources();
    let query = ui.search.text().to_lowercase();

    // Installed
    let live: BTreeSet<_> = inventory.iter().map(|info| info.key.clone()).collect();
    {
        let mut rows = ui.installed_rows.borrow_mut();
        retire(&ui.installed, &mut rows, |key| live.contains(key));
        for (info, detail) in inventory.iter().zip(&details) {
            let entry = detail.as_ref().map(|(entry, _)| entry);
            let plugin = built.installed.iter().find(|plugin| plugin.key == info.key);
            let shown = Shown::Installed {
                info,
                entry,
                plugin,
                several_sources: several,
            };
            if rows
                .get(&info.key)
                .is_some_and(|row| !row.matches_generation(&info.generation))
            {
                if let Some(old) = rows.remove(&info.key) {
                    ui.installed.remove(&old.row);
                }
            }
            let row = rows.entry(info.key.clone()).or_insert_with(|| {
                let row = Row::new(env, &info.key, &shown);
                ui.installed.add(&row.row);
                row
            });
            row.refresh(env, &info.key, &shown, &query);
        }
        ui.installed.set_visible(!rows.is_empty());
    }

    // Available: a plugin installed in a way the listing could not match (no
    // details) must not appear a second time.
    let offers: Vec<&Offer> = built
        .available
        .iter()
        .filter(|offer| !live.contains(&offer.key()))
        .collect();
    let wanted: BTreeSet<_> = offers.iter().map(|offer| offer.key()).collect();
    {
        let mut rows = ui.available_rows.borrow_mut();
        retire(&ui.available, &mut rows, |key| wanted.contains(key));
        for offer in &offers {
            let key = offer.key();
            let shown = Shown::Available {
                offer,
                several_sources: several,
            };
            let row = rows.entry(key.clone()).or_insert_with(|| {
                let row = Row::new(env, &key, &shown);
                ui.available.add(&row.row);
                row
            });
            row.refresh(env, &key, &shown, &query);
        }
    }
    *state.listing.borrow_mut() = built;
    refresh_available_header(env, ui);

    let anything_visible = ui
        .installed_rows
        .borrow()
        .values()
        .chain(ui.available_rows.borrow().values())
        .any(|row| row.row.is_visible());
    let anything = !ui.installed_rows.borrow().is_empty() || !ui.available_rows.borrow().is_empty();
    ui.empty.set_visible(anything && !anything_visible);
    // Retained settings reads the installed inventory from disk. Refresh it
    // when installations change or the retained-settings window closes,
    // rather than scanning the store on every runtime-status tick.
    let due = ui.known_installed.borrow().as_ref() != Some(&live);
    if due {
        *ui.known_installed.borrow_mut() = Some(live.clone());
        let retained = (env.ctx.retained_settings)().is_ok_and(|settings| !settings.is_empty());
        ui.retained.set_visible(retained);
    }
}

fn retire(
    group: &adw::PreferencesGroup,
    rows: &mut BTreeMap<String, Row>,
    keep: impl Fn(&str) -> bool,
) {
    rows.retain(|key, row| {
        if keep(key) {
            true
        } else {
            group.remove(&row.row);
            false
        }
    });
}

/// The age of the saved list, the refresh state and, when no list can be
/// shown, a status page that says why and offers the one action that helps.
fn refresh_available_header(env: &Env, ui: &Ui) {
    let state = &env.state;
    let listing = state.listing.borrow();
    let busy = state.refreshing.borrow().is_some();
    ui.spinner.set_spinning(busy);
    ui.refresh.set_sensitive(!busy);
    let official = listing.sources.iter().find(|source| source.official);
    let notice = state.notice.borrow().clone();
    let mut age = String::new();
    if let Some(official) = official {
        if let SourceState::Saved {
            retrieved_at,
            stale,
        } = official.state
        {
            age = format!("checked {}", age_text(now(), retrieved_at));
            if stale {
                age.push_str(" · older than a day");
            }
        }
    }
    if !state.overlay.borrow().is_empty() {
        age = "pre-release catalogue, not saved".into();
    }
    if listing.several_sources() {
        age = format!("{} sources · {age}", listing.sources.len());
    }
    ui.age.set_text(&age);
    ui.age.set_visible(!age.is_empty());
    let description = match &notice {
        Notice::Failed(problem) => Some(format!(
            "Could not check for new plugins: {problem}. Showing what was saved."
        )),
        Notice::Unsaved(text) => Some(text.clone()),
        _ => None,
    };
    ui.available
        .set_description(description.as_deref().map(escape_display).as_deref());
    // the list itself: empty only while something explains why.
    let has_list = !ui.available_rows.borrow().is_empty();
    let unloaded = state.loaded.get()
        && !has_list
        && official.is_none_or(|official| official.state == SourceState::NoSnapshot)
        && state.overlay.borrow().is_empty();
    let (title, text, button) = if busy && !has_list {
        ("Loading the plugin list…", String::new(), None)
    } else if unloaded && notice == Notice::NoStable {
        (
            "No stable release yet",
            "The official source has no stable release. Plugins appear here after one is published.".to_owned(),
            None,
        )
    } else if unloaded {
        (
            "Plugin list not loaded",
            match &notice {
                Notice::Failed(problem) => format!("Could not load the list: {problem}"),
                _ => "No list of plugins is saved on this computer yet. Nothing is requested until you press Load.".to_owned(),
            },
            Some("Load"),
        )
    } else {
        ("", String::new(), None)
    };
    ui.status.set_visible(!title.is_empty());
    ui.status.set_title(title);
    ui.status.set_description(Some(&text));
    ui.status_button.set_visible(button.is_some());
    ui.status_button.set_sensitive(!busy);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compact_cards_keep_refusal_reasons_even_when_the_plugin_is_disabled() {
        let mut info = Overview::fixture("sample", PluginState::Disabled, false);
        assert!(super::super::plugin_row::status_line(&info).is_none());
        info.declared = "Declared API unavailable: required settings cannot be edited".into();
        assert!(super::super::plugin_row::status_line(&info)
            .unwrap()
            .contains("required settings"));
        info.available = false;
        info.declared = "invalid settings schema".into();
        assert!(super::super::plugin_row::status_line(&info)
            .unwrap()
            .contains("Files unavailable"));
    }

    #[test]
    fn a_running_plugin_has_no_status_line_and_a_faulted_one_names_its_reason() {
        let running = Overview::fixture("sample", PluginState::Running, true);
        assert!(super::super::plugin_row::status_line(&running).is_none());
        let faulted = Overview::fixture(
            "sample",
            PluginState::Faulted {
                reason: "the plugin closed its connection".into(),
            },
            true,
        );
        assert_eq!(
            super::super::plugin_row::status_line(&faulted).as_deref(),
            Some("Stopped (the plugin closed its connection)")
        );
    }

    #[test]
    fn ages_are_stated_in_the_largest_whole_unit() {
        assert_eq!(age_text(1_000, 1_000), "just now");
        assert_eq!(age_text(1_000 + 120, 1_000), "2 min ago");
        assert_eq!(age_text(1_000 + 2 * 3600 + 5, 1_000), "2 h ago");
        assert_eq!(age_text(1_000 + 3 * 86_400, 1_000), "3 d ago");
        assert_eq!(age_text(5, 1_000), "at an unknown time");
    }
}
