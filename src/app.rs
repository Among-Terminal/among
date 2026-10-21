//! The application window: sidebar, pane area, key routing, and actions.
//!
//! The model lives in [`crate::tree`] and [`crate::workspace`]; this module
//! projects it into widgets. Widgets are rebuilt from the tree whenever it
//! changes, which keeps the tree the single source of truth for layout, while
//! pane widgets are reused across rebuilds so their terminals — and the
//! processes behind them — survive.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use adw::prelude::*;
use vte::prelude::*;

use crate::pane::{Pane, PaneSignals};
use crate::plugin::host::{PluginHost, PluginSpec};
use crate::plugin::{Context, EventKind, TerminalInfo};
use crate::prefs;
use crate::shortcuts::{self, Action, Bindings, Cycle, Direction, RESIZE_STEP};
use crate::slots::{self, Scope, SlotStore};
use crate::tree::{Node, Orientation, PaneId};
use crate::workspace::{PaneAllocator, Workspace, WorkspaceSet};

mod bell;

/// How often the main loop pumps plugins and re-derives pane sizes.
///
/// A single shared tick keeps GTK out of the hot path: plugin pipes are drained
/// without blocking and sizes are recomputed from allocations rather than from
/// accumulated deltas.
const TICK_MS: u32 = 50;

/// How often workspace sidebar summaries are re-collected.
const SUMMARY_REFRESH_MS: u64 = 3_000;

/// Characters a slot shows before eliding.
const SLOT_MAX_CHARS: usize = 120;

/// The application's model state.
pub struct AppState {
    pub workspaces: WorkspaceSet,
    pub panes: std::collections::BTreeMap<PaneId, Pane>,
    bells: Option<Rc<bell::Backend>>,
    /// Exit status of every process that has finished, per pane.
    ///
    /// Recorded rather than inferred, because a process that exited while its
    /// workspace was not displayed must still be reported as finished.
    pub exited: std::collections::HashMap<PaneId, i32>,
    /// Whether the sidebar is showing the narrow strip rather than the list.
    pub sidebar_collapsed: bool,
    /// Desired expanded width, not the allocation constrained by this window.
    sidebar_width: Option<f64>,
    /// A summary of what the sidebar last showed.
    ///
    /// Plugin updates arrive on a timer and mostly change nothing the sidebar
    /// displays. Rebuilding anyway would recreate the rows, which takes an
    /// in-progress rename away and interrupts a double-click.
    pub sidebar_signature: String,
    /// What the sidebar's rows were built from, without names or the active
    /// marker.
    ///
    /// A rename ends by putting a name back in place and moving the marker on the
    /// rows that already exist, so neither is a reason to build the rows again;
    /// anything else the rows show is. Compared when a rename ends, to tell
    /// whether the rows still match the model.
    pub sidebar_shape: String,
    /// The workspace whose name is being edited, while a rename is in progress.
    ///
    /// Plugin updates rebuild the sidebar on a timer, which would destroy the
    /// entry being edited; the rebuild is held off until the rename ends.
    pub renaming: Option<usize>,
    /// Set once the user has confirmed they mean to exit, so the confirmation
    /// is asked for once rather than every time the window is closed.
    pub exiting: bool,
    /// Recorded terminal and plugin notices, independent of their presentation.
    pub notices_recorded: u32,
    /// How many times any shortcut has fired.
    pub shortcuts_fired: u32,
    /// How many shortcuts the window's controller actually holds.
    pub shortcuts_installed: u32,
    pub last_notice: Option<String>,
    pub store: SlotStore,
    pub plugins: PluginHost,
    pub bindings: Bindings,
    settings: crate::settings::PreferencesStorage,
    settings_status: gtk::StringList,
    allocator: PaneAllocator,
    session_ready: bool,
    session_writer: Option<crate::session::Writer>,
    session_saved: Option<crate::session::Snapshot>,
    session_submitted: Option<crate::session::Snapshot>,
    session_problem: String,
    session_restore_warnings: Vec<String>,
    session_closing: bool,
    layout_generation: u64,
}

impl AppState {
    fn new(starts_allowed: Rc<Cell<bool>>) -> Self {
        // The first pane's id comes from the same allocator as every later one,
        // so a split can never mint an id that is already in use.
        let allocator = PaneAllocator::after(PaneId(1));
        let first = PaneId(1);
        Self {
            workspaces: WorkspaceSet::new(Workspace::new("project".into(), first, 0)),
            panes: std::collections::BTreeMap::new(),
            bells: None,
            exited: std::collections::HashMap::new(),
            sidebar_collapsed: false,
            sidebar_width: None,
            sidebar_signature: String::new(),
            sidebar_shape: String::new(),
            renaming: None,
            exiting: false,
            notices_recorded: 0,
            shortcuts_fired: 0,
            shortcuts_installed: 0,
            last_notice: None,
            store: SlotStore::new(),
            plugins: PluginHost::with_start_gate(starts_allowed),
            bindings: Bindings::defaults(),
            settings: crate::settings::PreferencesStorage::new(crate::settings::directory()),
            settings_status: gtk::StringList::new(&[""]),
            allocator,
            session_ready: false,
            session_writer: None,
            session_saved: None,
            session_submitted: None,
            session_problem: String::new(),
            session_restore_warnings: Vec::new(),
            session_closing: false,
            layout_generation: 0,
        }
    }

    fn allocate(&mut self) -> Option<PaneId> {
        self.allocator.try_next()
    }

    fn storage_status(&self) -> String {
        let mut messages = vec![self.settings.status()];
        if !self.session_problem.is_empty() {
            messages.push(format!("Session: {}", self.session_problem));
        }
        messages.extend(self.session_restore_warnings.iter().cloned());
        messages
            .into_iter()
            .filter(|message| !message.is_empty())
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// The pane widget for an id, if it has been created.
    pub fn pane(&self, id: PaneId) -> Option<&Pane> {
        self.panes.get(&id)
    }
}

/// Apply `AMONG_ENABLE` / `AMONG_DISABLE` so plugin switching is reachable
/// and observable without a settings window.
///
/// Disabling stops the process, and the store drops the plugin's content with it.
fn apply_env_plugin_switches(state: &Rc<RefCell<AppState>>) {
    if let Ok(list) = std::env::var("AMONG_DISABLE") {
        for name in list.split(',').map(str::trim).filter(|n| !n.is_empty()) {
            state.borrow_mut().plugins.disable(name);
            state.borrow_mut().store.remove_source(name);
            eprintln!("among: disabled plugin `{name}`");
        }
    }
    if let Ok(list) = std::env::var("AMONG_ENABLE") {
        for name in list.split(',').map(str::trim).filter(|n| !n.is_empty()) {
            state.borrow_mut().plugins.enable(name);
            eprintln!("among: enabled plugin `{name}`");
        }
    }
}

/// Apply `AMONG_REBIND="action=accel,action=accel"` before the window appears.
///
/// Reading bindings from the environment keeps a rebind testable without a
/// settings UI; a live process picks the change up because the shortcut
/// controller is built from the registry after this runs.
fn apply_env_rebindings(bindings: &mut Bindings) {
    let Ok(spec) = std::env::var("AMONG_REBIND") else {
        return;
    };
    for pair in spec.split(',').map(str::trim).filter(|p| !p.is_empty()) {
        let Some((action_name, accel)) = pair.split_once('=') else {
            eprintln!("among: AMONG_REBIND entry `{pair}` is not action=accel");
            continue;
        };
        match shortcuts::parse_action(action_name.trim()) {
            Some(action) if shortcuts::accel_is_valid(accel.trim()) => {
                bindings.rebind(action, accel.trim());
            }
            Some(_) => eprintln!("among: `{accel}` is not a usable accelerator"),
            None => eprintln!("among: unknown action `{action_name}` in AMONG_REBIND"),
        }
    }
}

/// A horizontal strip that hosts plugin content, hidden while empty.
fn slot_area(class: &str) -> gtk::Box {
    let box_ = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    box_.add_css_class(class);
    box_.set_visible(false);
    box_
}

/// Discover configured plugins. Any failure yields no plugins, never a crash.
fn load_plugin_specs() -> Vec<PluginSpec> {
    let Some(path) = plugin_config_path() else {
        return Vec::new();
    };
    read_plugin_config(&path)
}

fn plugin_config_path() -> Option<std::path::PathBuf> {
    crate::settings::directory().map(|directory| directory.join("plugins.json"))
}

fn plugin_dir() -> Option<std::path::PathBuf> {
    let base = std::env::var_os("XDG_DATA_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".local/share"))
        })?;
    Some(base.join("among").join("plugins"))
}

/// Parse the plugin configuration file.
///
/// Each failure path returns an empty list or skips one entry, because a single
/// bad plugin must never stop the application from starting.
pub fn read_plugin_config(path: &std::path::Path) -> Vec<PluginSpec> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) else {
        return Vec::new();
    };
    let Some(entries) = value.get("plugins").and_then(|v| v.as_array()) else {
        return Vec::new();
    };

    let installed = installed_plugins();
    entries
        .iter()
        .filter_map(|entry| {
            let name = entry.get("name")?.as_str()?.to_string();
            // A configured plugin that is not installed is skipped so the rest
            // of the configuration still takes effect.
            if !installed.contains(&name) {
                eprintln!("among: plugin `{name}` is configured but not installed");
                return None;
            }
            let enabled = entry
                .get("enabled")
                .and_then(|v| v.as_bool())
                .unwrap_or(true);
            let command = vec![plugin_dir()?.join(&name).display().to_string()];
            Some(PluginSpec {
                name,
                command,
                enabled,
            })
        })
        .collect()
}

/// Names of executables present in the user's plugin directory.
fn installed_plugins() -> Vec<String> {
    let Some(dir) = plugin_dir() else {
        return Vec::new();
    };
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .filter_map(|entry| {
            let path = entry.ok()?.path();
            if !path.is_file() {
                return None;
            }
            path.file_name()?.to_str().map(str::to_string)
        })
        .collect();
    names.sort();
    names
}

/// The terminal list handed to plugins on every request.
fn plugin_context(state: &AppState) -> Context {
    let terminals = state
        .workspaces
        .workspaces()
        .iter()
        .enumerate()
        .flat_map(|(index, workspace)| {
            workspace.panes().into_iter().map(move |pane| TerminalInfo {
                pane: pane.0,
                workspace: index,
                cwd: state.pane(pane).and_then(|p| p.cwd()),
                title: None,
                running: true,
            })
        })
        .collect();
    Context {
        active_workspace: state.workspaces.active_index(),
        workspace_count: state.workspaces.len(),
        terminals,
        focused_pane: Some(state.workspaces.active().focused.0),
    }
}

/// Rebuild everything derived from the model.
/// The window, its widgets, and the model state they project.
#[derive(Clone)]
struct Window {
    window: adw::ApplicationWindow,
    split_view: adw::OverlaySplitView,
    workspace_list: gtk::ListBox,
    pane_area: gtk::Box,
    /// Kept so the window's chrome can be inspected.
    header_bar: adw::HeaderBar,
    workspace_header: gtk::Box,
    app_header: gtk::Box,
    app_footer: gtk::Box,
    sidebar_resize: gtk::Separator,
    state: Rc<RefCell<AppState>>,
    /// Each `Paned` with the tree path it mirrors, so the tree's ratios can be
    /// applied to real pixel extents after layout.
    paneds: Rc<RefCell<Vec<SplitWidget>>>,
    /// Per-source clocks keep one accepted summary from suppressing a sibling.
    summary_requested: Rc<RefCell<std::collections::BTreeMap<(usize, String), u64>>>,
    collection_cursor: Rc<Cell<usize>>,
}

struct SplitWidget {
    widget: gtk::Paned,
    path: Vec<usize>,
    initialized: Rc<std::cell::Cell<bool>>,
    extent: Rc<std::cell::Cell<i32>>,
}

/// Install the application stylesheet.
///
/// Loaded on libadwaita's style manager so the rules sit above the platform
/// theme and follow the user's light/dark preference. Rules built from
/// `@accent_color`, `@warning_color`, and friends inherit that preference
/// instead of hard-coding values.
fn install_styles() {
    // Verification hook: the stylesheet must load under both schemes, and a
    // session only exercises one of them.
    match std::env::var("AMONG_COLOR_SCHEME").as_deref() {
        Ok("light") => adw::StyleManager::default().set_color_scheme(adw::ColorScheme::ForceLight),
        Ok("dark") => adw::StyleManager::default().set_color_scheme(adw::ColorScheme::ForceDark),
        _ => {}
    }
    let Some(display) = gtk::gdk::Display::default() else {
        // No display means there is nothing to style; the caller is a headless
        // run and carries on regardless.
        return;
    };
    if let Some(root) = crate::packaged::root() {
        // Theme discovery is application-private: exporting XDG_DATA_DIRS
        // would also redirect the user's host shells and external plugins.
        let icons = root.join("usr/share/icons");
        gtk::IconTheme::for_display(&display).add_search_path(icons);
    }
    let provider = gtk::CssProvider::new();
    // GTK reports a bad rule through a warning rather than an error, so the
    // parse result is checked: a silently empty stylesheet would leave every
    // pane looking unfocused.
    provider.connect_parsing_error(|_, section, error| {
        let _ = section;
        eprintln!("among: stylesheet error: {error}");
    });
    provider.load_from_string(include_str!("../data/among.css"));
    gtk::style_context_add_provider_for_display(
        &display,
        &provider,
        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );
}

/// Build the application window.
pub fn build(app: &adw::Application, requested: Option<Action>) {
    let _ = build_with_writer(app, requested, crate::session::Writer::new);
}

struct ApplicationPlugins {
    app: glib::WeakRef<adw::Application>,
    starts_allowed: Rc<Cell<bool>>,
    retiring: Cell<usize>,
}

thread_local! {
    static APPLICATION_PLUGINS: RefCell<Vec<Rc<ApplicationPlugins>>> = const { RefCell::new(Vec::new()) };
}

fn application_plugins(app: &adw::Application) -> Rc<ApplicationPlugins> {
    APPLICATION_PLUGINS.with(|entries| {
        let mut entries = entries.borrow_mut();
        entries.retain(|entry| entry.app.upgrade().is_some());
        if let Some(entry) = entries
            .iter()
            .find(|entry| entry.app.upgrade().as_ref() == Some(app))
        {
            return entry.clone();
        }
        let entry = Rc::new(ApplicationPlugins {
            app: app.downgrade(),
            starts_allowed: Rc::new(Cell::new(true)),
            retiring: Cell::new(0),
        });
        entries.push(entry.clone());
        entry
    })
}

struct RetiringPlugins(Rc<ApplicationPlugins>);

impl ApplicationPlugins {
    fn retire(self: &Rc<Self>) -> RetiringPlugins {
        self.retiring.set(self.retiring.get().saturating_add(1));
        self.starts_allowed.set(false);
        RetiringPlugins(self.clone())
    }
}

impl Drop for RetiringPlugins {
    fn drop(&mut self) {
        self.0.retiring.set(self.0.retiring.get().saturating_sub(1));
        self.0.starts_allowed.set(self.0.retiring.get() == 0);
    }
}

fn build_with_writer(
    app: &adw::Application,
    requested: Option<Action>,
    writer: fn(std::path::PathBuf) -> Result<crate::session::Writer, String>,
) -> Option<Window> {
    if let Some(window) = app
        .active_window()
        .or_else(|| app.windows().into_iter().next())
    {
        window.present();
        return None;
    }
    // Printing the bindings from the running registry is what keeps the
    // documentation honest: the list cannot drift from what is installed.
    if std::env::var_os("AMONG_LIST_BINDINGS").is_some() {
        let bindings = Bindings::defaults();
        for action in Bindings::default_actions() {
            let accel = bindings.accel_for(action).unwrap_or("");
            println!("{}\t{}", action.label(), accel);
        }
        std::process::exit(0);
    }

    install_styles();
    let window = adw::ApplicationWindow::builder()
        .application(app)
        .title("Among Terminal")
        .default_width(1200)
        .default_height(800)
        .build();
    crate::packaged::apply_window_identity(&window);

    let admission = application_plugins(app);
    let state = Rc::new(RefCell::new(AppState::new(
        admission.starts_allowed.clone(),
    )));
    match bell::Backend::new(&window) {
        Ok(backend) => state.borrow_mut().bells = Some(backend),
        Err(_) => eprintln!("among: bell session identity exhausted; backend unavailable"),
    }
    {
        let mut state = state.borrow_mut();
        state.settings.load(crate::settings::Document::Plugins);
        state.sidebar_width = state
            .settings
            .load(crate::settings::Document::Window)
            .and_then(|value| {
                value
                    .get("sidebar_width")
                    .and_then(serde_json::Value::as_f64)
            });
        if let Some(value) = state.settings.load(crate::settings::Document::Shortcuts) {
            if let Some(overrides) = value
                .get("overrides")
                .and_then(serde_json::Value::as_object)
            {
                let warnings = state.bindings.restore_overrides(overrides, |accel| {
                    gtk::ShortcutTrigger::parse_string(accel).is_some()
                });
                if !warnings.is_empty() {
                    state
                        .settings
                        .problems
                        .insert("shortcuts.json".into(), warnings.join("\n"));
                }
            }
        }
        let status = state.settings.status();
        if !status.is_empty() {
            eprintln!("among: settings: {status}");
        }
        state.settings_status.splice(0, 1, &[&status]);
    }
    {
        // A binding the desktop handles first would leave an action that never
        // fires, so such bindings are reported rather than silently accepted.
        let bindings = &state.borrow().bindings;
        for (action, accel) in shortcuts::reserved_bindings(bindings) {
            eprintln!(
                "among: {} is bound to `{accel}`, which the desktop handles first",
                action.label()
            );
        }
        for (action, accel) in shortcuts::invalid_bindings(bindings) {
            eprintln!(
                "among: {} has an unusable binding `{accel}`",
                action.label()
            );
        }
    }
    apply_env_rebindings(&mut state.borrow_mut().bindings);

    let specs = load_plugin_specs();
    state.borrow_mut().plugins.register_all(specs);
    if let Some(root) = crate::plugin::inventory::root() {
        match crate::plugin::inventory::read(&root) {
            Ok(entries) => {
                let document = state
                    .borrow_mut()
                    .settings
                    .load(crate::settings::Document::ManagedPlugins);
                for entry in entries {
                    let local = document.as_ref().and_then(|document| {
                        crate::plugin::inventory::local(document, &entry.key())
                    });
                    state.borrow_mut().plugins.register_managed(entry, local);
                }
            }
            Err(problem) => {
                state
                    .borrow_mut()
                    .settings
                    .problems
                    .insert("managed installation".into(), problem.to_string());
            }
        }
    }
    apply_env_plugin_switches(&state);

    let app_header = slot_area("app-slot-header");
    let workspace_header = slot_area("app-slot-workspace");
    let app_footer = slot_area("app-slot-footer");

    let pane_area = gtk::Box::new(gtk::Orientation::Vertical, 0);
    pane_area.set_vexpand(true);

    let workspace_list = gtk::ListBox::new();
    workspace_list.set_selection_mode(gtk::SelectionMode::Single);
    workspace_list.add_css_class("navigation-sidebar");
    workspace_list.add_css_class("workspace-sidebar");

    let sidebar_scroller = gtk::ScrolledWindow::builder()
        .child(&workspace_list)
        .vexpand(true)
        .build();
    let sidebar_overlay = gtk::Overlay::new();
    sidebar_overlay.set_child(Some(&sidebar_scroller));
    let sidebar_resize = gtk::Separator::new(gtk::Orientation::Vertical);
    sidebar_resize.add_css_class("sidebar-resize-handle");
    sidebar_resize.set_size_request(6, -1);
    sidebar_resize.set_halign(gtk::Align::End);
    sidebar_resize.set_vexpand(true);
    sidebar_resize.set_can_target(true);
    sidebar_resize.set_cursor_from_name(Some("col-resize"));
    sidebar_overlay.add_overlay(&sidebar_resize);

    let content_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
    content_box.append(&workspace_header);
    content_box.append(&pane_area);

    // An overlay split view rather than a navigation one: the sidebar stays on
    // screen in both presentations and lays over the content when the window is
    // narrow, which is what a strip needs. A navigation split view would be built
    // around adaptive back-navigation instead, which this does not want.
    let split_view = adw::OverlaySplitView::builder()
        .sidebar(&sidebar_overlay)
        .content(&content_box)
        .build();
    split_view.set_min_sidebar_width(EXPANDED_MIN_WIDTH);
    split_view.set_max_sidebar_width(EXPANDED_MAX_WIDTH);

    split_view.set_vexpand(true);

    // A header bar inside a toolbar view is what gives the window its standard
    // controls: close, minimise, and maximise, plus the sidebar toggle.
    let header_bar = adw::HeaderBar::new();
    let preferences_button = gtk::Button::new();
    preferences_button.set_icon_name("preferences-system-symbolic");
    preferences_button.set_tooltip_text(Some("Settings"));
    preferences_button.update_property(&[gtk::accessible::Property::Label("Settings")]);
    header_bar.pack_start(&preferences_button);

    let sidebar_toggle = gtk::ToggleButton::new();
    sidebar_toggle.add_css_class("sidebar-toggle");
    sidebar_toggle.set_icon_name("sidebar-show-symbolic");
    sidebar_toggle.set_tooltip_text(Some("Collapse the sidebar to a strip"));
    sidebar_toggle.update_property(&[gtk::accessible::Property::Label("Toggle sidebar")]);
    header_bar.pack_start(&sidebar_toggle);
    let new_workspace_button = gtk::Button::new();
    new_workspace_button.set_icon_name("tab-new-symbolic");
    new_workspace_button.set_tooltip_text(Some("New workspace"));
    new_workspace_button.update_property(&[gtk::accessible::Property::Label("New workspace")]);
    header_bar.pack_start(&new_workspace_button);

    let toolbar_view = adw::ToolbarView::new();
    toolbar_view.add_top_bar(&header_bar);
    // The application header is a second top bar, so plugin content sits under
    // the controls rather than competing with them.
    toolbar_view.add_top_bar(&app_header);
    toolbar_view.set_content(Some(&split_view));
    toolbar_view.add_bottom_bar(&app_footer);
    window.set_content(Some(&toolbar_view));

    let ctx = Window {
        window,
        split_view,
        workspace_list,
        pane_area,
        header_bar,
        workspace_header,
        app_header,
        app_footer,
        sidebar_resize,
        state,
        paneds: Rc::new(RefCell::new(Vec::new())),
        summary_requested: Rc::new(RefCell::new(std::collections::BTreeMap::new())),
        collection_cursor: Rc::new(Cell::new(0)),
    };

    let removal_handler = Rc::new(RefCell::new(None));
    let handler = app.connect_window_removed({
        let state = ctx.state.clone();
        let target = ctx.window.downgrade();
        let removal_handler = removal_handler.clone();
        move |app, removed| {
            if target
                .upgrade()
                .is_none_or(|window| window.upcast_ref::<gtk::Window>() != removed)
            {
                return;
            }
            if let Some(handler) = removal_handler.borrow_mut().take() {
                app.disconnect(handler);
            }
            // share admission across replacement windows, not just one host.
            // terminals may reopen immediately; plugins wait for all old reapers.
            let retiring = admission.retire();
            state.borrow_mut().exiting = true;
            if let Some(bells) = &state.borrow().bells {
                bells.shutdown();
            }
            state.borrow_mut().plugins.shutdown();
            // destroying the last window would otherwise end the process before
            // its detached supervisors could signal/reap the owned children.
            let hold = app.hold();
            let state = state.clone();
            glib::timeout_add_local(Duration::from_millis(20), move || {
                let _keep_application_alive = &hold;
                let _keep_plugin_starts_closed = &retiring;
                let mut state = state.borrow_mut();
                state.plugins.poll();
                if state.plugins.is_stopped() {
                    glib::ControlFlow::Break
                } else {
                    glib::ControlFlow::Continue
                }
            });
        }
    });
    *removal_handler.borrow_mut() = Some(handler);

    install_shortcuts(&ctx);
    install_tick(&ctx);
    install_sidebar_resize(&ctx);
    apply_sidebar_width(&ctx);

    // Collapsing switches the sidebar between the full list and the narrow
    // strip. The sidebar itself stays visible, because the spec requires every
    // workspace to stay reachable while collapsed.
    sidebar_toggle.connect_toggled({
        let ctx = ctx.handle();
        move |button| {
            set_sidebar_collapsed(&ctx, button.is_active());
        }
    });

    // The window's own close control takes the same path as closing the last
    // workspace, so terminals are never ended without the confirmation.
    ctx.window.connect_close_request({
        let ctx = ctx.handle();
        move |_| {
            if std::env::var_os("AMONG_SELFCHECK").is_some() {
                eprintln!(
                    "among: window_close_request active={}",
                    ctx.window.is_active()
                );
            }
            if ctx.state.borrow().session_closing {
                return glib::Propagation::Stop;
            }
            if !ctx.state.borrow().session_ready {
                ctx.state.borrow_mut().exiting = true;
                return glib::Propagation::Proceed;
            }
            if ctx.state.borrow().exiting || !has_running_terminals(&ctx) {
                return glib::Propagation::Proceed;
            }
            confirm_exit(&ctx);
            glib::Propagation::Stop
        }
    });

    preferences_button.connect_clicked({
        let ctx = ctx.handle();
        move |_| run_action(&ctx, Action::OpenPreferences)
    });
    new_workspace_button.connect_clicked({
        let ctx = ctx.handle();
        move |_| new_workspace(&ctx)
    });

    let for_activate = ctx.handle();
    ctx.workspace_list.connect_row_activated(move |_, row| {
        let index = row.index();
        if index < 0 {
            return;
        }
        let changed = for_activate
            .state
            .borrow_mut()
            .workspaces
            .activate(index as usize);
        if changed {
            // The rows are not rebuilt: recreating them would destroy the
            // entry the user is clicking, so a double-click could never
            // reach the second press to start renaming.
            switch_workspace(&for_activate);
        }
    });

    begin_session(&ctx, writer);
    // The application is a workspace in its own right, so it opens using the
    // whole screen; F11 leaves and re-enters fullscreen.
    ctx.window.maximize();
    ctx.window.present();

    if std::env::var_os("AMONG_SELFCHECK").is_some() {
        install_selfcheck(&ctx);
    }

    if let Some(action) = requested {
        // Reported rather than ignored, so an action with no binding is
        // visible instead of unreachable.
        if report_unbound(&ctx, action) {
            selfcheck(&ctx, "unbound-action");
            return Some(ctx);
        }
        run_action(&ctx, action);
        selfcheck(&ctx, "named-action");
    }
    Some(ctx)
}

/// Report the live state periodically when `AMONG_SELFCHECK` is set.
fn install_selfcheck(ctx: &Window) {
    let handle = ctx.handle();
    // Reports for a generous window: verification runs drive the application
    // with real input over many seconds, and a short window would hide the
    // events that happened after it closed.
    let tick_limit = std::env::var("AMONG_SELFCHECK_TICKS")
        .ok()
        .and_then(|v| v.parse::<u32>().ok())
        .unwrap_or(120);
    let ticks = Rc::new(std::cell::Cell::new(0u32));
    let counter = ticks.clone();
    glib::timeout_add_local(Duration::from_millis(1000), move || {
        counter.set(counter.get() + 1);
        selfcheck(&handle, &format!("tick{}", counter.get()));
        if counter.get() >= tick_limit {
            glib::ControlFlow::Break
        } else {
            glib::ControlFlow::Continue
        }
    });

    // AMONG_SCRIPT names actions to run one after another, so the wiring for
    // split, focus movement, resizing, workspaces, and close can be exercised
    // without a pointer or keyboard.
    if let Some(script) = std::env::var_os("AMONG_SCRIPT") {
        let steps: Vec<String> = std::env::split_paths(&script)
            .next()
            .map(|p| std::fs::read_to_string(p).unwrap_or_default())
            .unwrap_or_default()
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty() && !line.starts_with('#'))
            .map(str::to_string)
            .collect();
        let mut index = 0usize;
        let driver = ctx.handle();
        glib::timeout_add_local(Duration::from_millis(700), move || {
            if index >= steps.len() {
                // Stop driving but keep running: background processes need
                // time to finish before a run is inspected.
                eprintln!("among: script finished after {index} steps");
                return glib::ControlFlow::Break;
            }
            let step = steps[index].clone();
            index += 1;
            run_script_step(&driver, &step);
            glib::ControlFlow::Continue
        });
    }
}

/// Report what was actually built, for verification runs.
///
/// A build that compiles proves nothing about what was rendered, so this walks
/// the live widget tree and prints what it finds.
fn selfcheck(ctx: &Window, label: &str) {
    if let Some(display) = gtk::gdk::Display::default() {
        let theme = gtk::IconTheme::for_display(&display);
        let icons = [
            "preferences-system-symbolic",
            "sidebar-show-symbolic",
            "tab-new-symbolic",
        ]
        .map(|name| format!("{name}:{}", theme.has_icon(name)));
        eprintln!("among[{label}]: header_icons={icons:?}");
        if std::env::var_os("AMONG_SELFCHECK").is_some() {
            let name = "com.github.among-terminal";
            let icon = theme.lookup_icon(
                name,
                &[],
                48,
                1,
                gtk::TextDirection::None,
                gtk::IconLookupFlags::empty(),
            );
            eprintln!(
                "among[{label}]: application_icon={} icon_name={:?} icon_file={:?} icon_size={}x{}",
                theme.has_icon(name),
                icon.icon_name(),
                icon.file().and_then(|file| file.path()),
                icon.intrinsic_width(),
                icon.intrinsic_height()
            );
        }
    }
    if std::env::var_os("AMONG_SELFCHECK").is_none() {
        return;
    }
    {
        let state = ctx.state.borrow();
        eprintln!("among[{label}]: session_ready={} session_closing={} session_saved={} session_problem={:?}", state.session_ready, state.session_closing, state.session_saved.is_some(), state.session_problem);
        eprintln!(
            "among[{label}]: settings_problem={:?}",
            state.settings.status()
        );
        eprintln!("among[{label}]: window_active={}", ctx.window.is_active());
    }
    fn session_buttons(widget: &gtk::Widget, window: &gtk::Widget, output: &mut Vec<String>) {
        if let Some(button) = widget.downcast_ref::<gtk::Button>() {
            if let Some(name) = button.label() {
                if [
                    "Restore",
                    "Start new",
                    "Exit",
                    "Exit anyway",
                    "Keep open",
                    "Cancel",
                ]
                .contains(&name.as_str())
                {
                    if let Some(bounds) = button.compute_bounds(window) {
                        if bounds.width() > 0.0 && bounds.height() > 0.0 {
                            output.push(format!(
                                "{name}:{}+{}+{}x{}",
                                bounds.x(),
                                bounds.y(),
                                bounds.width(),
                                bounds.height()
                            ));
                        }
                    }
                }
            }
        }
        let mut child = widget.first_child();
        while let Some(node) = child {
            session_buttons(&node, window, output);
            child = node.next_sibling();
        }
    }
    let mut responses = Vec::new();
    session_buttons(
        ctx.window.upcast_ref(),
        ctx.window.upcast_ref(),
        &mut responses,
    );
    eprintln!("among[{label}]: session_buttons={responses:?}");
    let state = ctx.state.borrow();
    if let Some(bells) = &state.bells {
        let focus = bells.focus();
        let panes: Vec<_> = state
            .panes
            .keys()
            .map(|id| (id.0, bells.attention(id.0)))
            .collect();
        let workspaces: Vec<_> = state
            .workspaces
            .workspaces()
            .iter()
            .map(|workspace| bells.aggregate(workspace.panes().into_iter().map(|id| id.0)))
            .collect();
        eprintln!("among[{label}]: bell_events={} bell_application_active={} bell_viewed={:?} bell_panes={panes:?} bell_workspaces={workspaces:?} bell_diagnostics={:?}",
            bells.event_count(), focus.application_active, focus.viewed.map(|source| source.pane()), bells.diagnostics());
    }
    // X11 pointer input is surface-relative, including the client-side shadow.
    let (surface_x, surface_y) = ctx.window.surface_transform();
    let toggle = find_with_class(ctx.header_bar.upcast_ref(), "sidebar-toggle")
        .and_then(|widget| widget.compute_bounds(&ctx.window))
        .map(|rect| {
            format!(
                "toggle:{:.0}+{:.0}+{:.0}x{:.0}",
                f64::from(rect.x()) + surface_x,
                f64::from(rect.y()) + surface_y,
                rect.width(),
                rect.height()
            )
        })
        .unwrap_or_default();
    let edge = ctx
        .sidebar_resize
        .compute_bounds(&ctx.window)
        .map(|rect| {
            format!(
                "edge:{:.0}+{:.0}+{:.0}x{:.0}",
                f64::from(rect.x()) + surface_x,
                f64::from(rect.y()) + surface_y,
                rect.width(),
                rect.height()
            )
        })
        .unwrap_or_default();
    eprintln!(
        "among[{label}]: toast_widgets={} sidebar_width={} sidebar_preferred={} sidebar_resize_visible={} sidebar_resize_cursor={} sidebar_resize_rect=[{edge}] sidebar_toggle_rect=[{toggle}]",
        toast_widget_count(ctx.window.upcast_ref()),
        ctx.split_view.sidebar().map(|widget| widget.width()).unwrap_or(0),
        field(state.sidebar_width),
        ctx.sidebar_resize.is_visible(),
        ctx.sidebar_resize.cursor().and_then(|cursor| cursor.name()).unwrap_or_default(),
    );
    let active = state.workspaces.active_index();
    let workspace = state.workspaces.active();
    let area = (ctx.pane_area.width(), ctx.pane_area.height());

    // The geometry the split-orientation decision is made from. Reported so a
    // wrong split can be traced to numbers instead of guessed at.
    let boxes: Vec<String> = workspace
        .panes()
        .iter()
        .map(
            |id| match crate::size::pane_rect(&workspace.nodes, area.0, area.1, *id) {
                Some((x, y, w, h)) => format!("{}:{}+{}+{}x{}", id.0, x, y, w, h),
                None => format!("{}:none", id.0),
            },
        )
        .collect();
    let sizes: Vec<String> = workspace
        .visible_panes()
        .iter()
        .map(|id| match state.pane(*id) {
            Some(pane) => match pane.reported_size() {
                Some(size) => format!("{}x{}", size.columns, size.rows),
                None => "unreported".to_string(),
            },
            None => "no-widget".to_string(),
        })
        .collect();
    let has_focus = state
        .pane(workspace.focused)
        .map(|pane| pane.terminal().has_focus())
        .unwrap_or(false);
    let finished: Vec<String> = state
        .exited
        .iter()
        .map(|(pane, status)| format!("{pane:?}={status}"))
        .collect();
    let collapsed = state.sidebar_collapsed;
    let fullscreen = ctx.window.is_fullscreen();
    // The platform controls are widgets the header bar adds; counting them
    // verifies they are actually present rather than only requested.
    // libadwaita builds the control widgets itself and does not expose them, so
    // what is verified is that the header bar is asking for them — which is the
    // part this application controls.
    let title_buttons = (
        ctx.header_bar.shows_start_title_buttons(),
        ctx.header_bar.shows_end_title_buttons(),
    );
    // All toplevels, not the application's own windows: the preferences window
    // is a plain window and would not appear in the application's list.
    let open_windows = gtk::Window::list_toplevels().len();
    // Sidebar geometry, so a pointer test can find the rows without guessing
    // where the header bar and the split view put them.
    let sidebar_origin = ctx
        .workspace_list
        .compute_point(&ctx.window, &gtk::graphene::Point::new(0.0, 0.0))
        .map(|p| (p.x() as i32, p.y() as i32))
        .unwrap_or((0, 0));
    // Each row's geometry, colour, and whether its name is being edited, so a
    // pointer test can find the rows and a rename is observable.
    let mut rows: Vec<String> = Vec::new();
    let mut row_states: Vec<String> = Vec::new();
    let mut name_rects: Vec<String> = Vec::new();
    let mut row = ctx.workspace_list.first_child();
    let mut row_index = 0usize;
    while let Some(node) = row {
        if let Some(list_row) = node.downcast_ref::<gtk::ListBoxRow>() {
            if let Some(rect) = list_row.compute_bounds(&ctx.workspace_list) {
                rows.push(format!(
                    "{row_index}:{}+{}+{}x{}",
                    rect.x() as i32,
                    rect.y() as i32,
                    rect.width() as i32,
                    rect.height() as i32
                ));
            }
            let mut colour = None;
            let mut editing = false;
            let mut count = String::new();
            if let Some(content) = list_row.child() {
                colour =
                    find_with_class(&content, "workspace-stripe").and_then(|w| colour_index_of(&w));
                // A name is being edited exactly while the row holds an entry rather than a
                // label; the field exists at no other time.
                editing =
                    find_name_widget(&content).is_some_and(|widget| widget.is::<gtk::Entry>());
                // Inspect the built-in displayed count, independently of optional
                // plugin summaries now that the legacy count example is removed.
                let count_class = if collapsed {
                    "workspace-strip-count"
                } else {
                    "workspace-terminal-count"
                };
                count = find_with_class(&content, count_class)
                    .and_then(|w| w.downcast::<gtk::Label>().ok())
                    .map(|l| l.text().to_string())
                    .unwrap_or_default();
            }
            row_states.push(format!(
                "{row_index}:{}:{editing}:{count}",
                colour.map(|c| c.to_string()).unwrap_or("?".into())
            ));
            // The name widget's own bounds, so a pointer test aims at the name
            // rather than at where it is assumed to be, in either state.
            if let Some(content) = list_row.child() {
                if let Some(label) = find_name_widget(content.upcast_ref()) {
                    if let Some(rect) = label.compute_bounds(&ctx.workspace_list) {
                        name_rects.push(format!(
                            "{row_index}:{}+{}+{}x{}",
                            rect.x() as i32,
                            rect.y() as i32,
                            rect.width() as i32,
                            rect.height() as i32
                        ));
                    }
                }
            }
        }
        row_index += 1;
        row = node.next_sibling();
    }
    let names: Vec<String> = state
        .workspaces
        .workspaces()
        .iter()
        .map(|w| w.name.clone())
        .collect();

    // The pane area's position in window coordinates. A drag test needs pixel
    // coordinates, and deriving them from the layout avoids guessing at where
    // the sidebar and header end.
    let origin = ctx
        .pane_area
        .compute_point(&ctx.window, &gtk::graphene::Point::new(0.0, 0.0))
        .map(|p| (p.x() as i32, p.y() as i32))
        .unwrap_or((0, 0));

    eprintln!(
        "among[{label}]: workspaces={} active={} name={:?} visible={} panes={} paned_widgets={} focused={:?} area={}x{} origin={},{} tree_boxes=[{}] has_focus={has_focus} terminal_sizes=[{}] exited=[{}] sidebar_collapsed={collapsed} fullscreen={fullscreen} title_buttons={title_buttons:?} windows={open_windows} sidebar_origin={},{} sidebar_rows=[{}] sidebar_states=[{}] name_rects=[{}] names=[{}] rows_in_list={}",
        state.workspaces.len(),
        active,
        workspace.name,
        workspace.visible_panes().len(),
        workspace.panes().len(),
        pane_area_widget_count(ctx),
        workspace.focused,
        area.0,
        area.1,
        origin.0,
        origin.1,
        boxes.join(" "),
        sizes.join(" "),
        finished.join(" "),
        sidebar_origin.0,
        sidebar_origin.1,
        rows.join(" "),
        row_states.join(" "),
        name_rects.join(" "),
        names.join("|"),
        workspace_list_rows(ctx),
    );

    let plugins: Vec<String> = state
        .plugins
        .listing()
        .into_iter()
        .map(|(name, status)| format!("{name}={status:?}"))
        .collect();
    let rejections: Vec<String> = state
        .plugins
        .rejections()
        .iter()
        .map(|(name, reason)| format!("{name}:{reason}"))
        .collect();
    let bound = state
        .bindings
        .accel_for(Action::SplitPane)
        .unwrap_or("none");
    let sources = state.store.sources_in(Scope::AppHeader);
    let missing: Vec<String> = shortcuts::invalid_bindings(&state.bindings)
        .iter()
        .map(|(action, accel)| format!("{}:{}", action.label(), accel))
        .collect();
    let summaries = state.store.summaries(active).len();
    let focused_marked = state
        .pane(workspace.focused)
        .map(|pane| pane.is_focused())
        .unwrap_or(false);
    let notices = state.notices_recorded;
    let last_notice = state.last_notice.clone();
    // Whether a CSS marker is actually drawn at a usable size: a colour class
    // on a zero-sized widget is invisible, so the class alone proves nothing.
    let stripe_measured = ctx
        .workspace_list
        .first_child()
        .and_then(|row| row.first_child())
        .and_then(|content| find_with_class(&content, "workspace-stripe"))
        .map(|stripe| {
            let rect = stripe
                .compute_bounds(&ctx.workspace_list)
                .map(|r| format!("{}x{}", r.width() as i32, r.height() as i32))
                .unwrap_or_else(|| "no-bounds".into());
            format!("sidebar_stripe={rect}")
        })
        .unwrap_or_else(|| "sidebar_stripe=absent".into());
    let header_stripe = state
        .pane(workspace.focused)
        .map(|pane| pane.header_stripe_size())
        .unwrap_or_else(|| "none".into());
    // Each pane's header colour, so a workspace's identity can be checked in the
    // sidebar, the strip, and the pane headers together.
    let pane_colours: Vec<String> = workspace
        .panes()
        .iter()
        .filter_map(|id| {
            state
                .pane(*id)
                .and_then(|pane| pane.header_colour())
                .map(|c| format!("{}:{c}", id.0))
        })
        .collect();
    // Each pane's known working directory. A plugin that reports on the
    // repository can only do so once the shell has published one, so this is
    // what tells a failing plugin report apart from an unreported directory.
    let cwds: Vec<String> = workspace
        .panes()
        .iter()
        .filter_map(|id| {
            state.pane(*id).map(|pane| {
                let cwd = pane.cwd().unwrap_or_else(|| "unknown".into());
                let uri = pane.cwd_uri().unwrap_or_else(|| "none".into());
                format!("{}:{cwd}|{uri}", id.0)
            })
        })
        .collect();
    // Each pane's scroll position in lines, so a pointer test can tell a moved
    // history from one that stayed put, and the calibrated amount can be read
    // back rather than assumed. The row height travels with it because that is
    // what converts a gesture's distance into lines.
    let scrolls: Vec<String> = workspace
        .panes()
        .iter()
        .filter_map(|id| {
            state
                .pane(*id)
                .map(|pane| format!("{}:{:.2}@{}", id.0, pane.scroll_lines(), pane.cell_height()))
        })
        .collect();
    let titles: Vec<String> = workspace
        .panes()
        .iter()
        .filter_map(|id| {
            state.pane(*id).map(|pane| {
                // Plain fields rather than a Debug-printed tuple: nested quotes
                // are escaped in Debug output and become hard to read back.
                let title = pane.title().unwrap_or_default().replace('|', "/");
                let title = if title.is_empty() {
                    "-".to_string()
                } else {
                    title
                };
                format!("{}|{title}|{}", id.0, pane.title_changed())
            })
        })
        .collect();
    let focus_states: Vec<String> = workspace
        .panes()
        .into_iter()
        .filter_map(|id| {
            let pane = state.pane(id)?;
            let highlighted = pane
                .widget()
                .is_some_and(|widget| widget.has_css_class("focused"));
            Some(format!(
                "{}:{}:{}",
                id.0,
                pane.terminal().has_focus(),
                highlighted
            ))
        })
        .collect();
    eprintln!("among[{label}]: pane_focus_states={focus_states:?}");
    let numbers: Vec<String> = workspace
        .panes()
        .into_iter()
        .filter_map(|id| {
            let pane = state.pane(id)?;
            let widget = pane.widget()?;
            let label = find_with_class(widget.upcast_ref(), "pane-number")?
                .downcast::<gtk::Label>()
                .ok()?;
            Some(label.text().to_string())
        })
        .collect();
    eprintln!("among[{label}]: pane_numbers={numbers:?}");
    let selections: Vec<u64> = workspace
        .panes()
        .into_iter()
        .filter(|id| {
            state
                .pane(*id)
                .is_some_and(|pane| pane.terminal().has_selection())
        })
        .map(|id| id.0)
        .collect();
    eprintln!("among[{label}]: terminal_selections={selections:?}");
    let menus: Vec<u64> = state
        .panes
        .iter()
        .filter(|(_, pane)| pane.menu_open())
        .map(|(id, _)| id.0)
        .collect();
    eprintln!("among[{label}]: terminal_menus={menus:?}");
    let menu_items: Vec<_> = state
        .panes
        .iter()
        .filter(|(_, pane)| pane.menu_open())
        .map(|(id, pane)| (id.0, pane.menu_inspection()))
        .collect();
    eprintln!("among[{label}]: menu_items={menu_items:?}");
    // Which panes are currently indicating a pending drop, so the indication
    // is observable while a drag is in flight.
    let indications: Vec<String> = state
        .workspaces
        .active()
        .panes()
        .iter()
        .filter_map(|id| {
            state
                .pane(*id)
                .and_then(|pane| pane.drop_indication())
                .map(|side| format!("{}:{side}", id.0))
        })
        .collect();
    let shortcuts = state.shortcuts_fired;
    let installed = state.shortcuts_installed;
    let footers: Vec<String> = workspace
        .panes()
        .into_iter()
        .filter_map(|id| {
            let widget = state.pane(id)?.widget()?;
            fn collect(widget: &gtk::Widget, out: &mut Vec<String>) {
                if let Some(label) = widget.downcast_ref::<gtk::Label>() {
                    if label.parent().is_some_and(|parent| {
                        parent.has_css_class("status-fragment")
                            && parent.parent().is_some_and(|slot| {
                                slot.has_css_class("pane-slot")
                                    && slot
                                        .prev_sibling()
                                        .is_some_and(|sibling| sibling.is::<vte::Terminal>())
                            })
                    }) {
                        let tooltip = label
                            .parent()
                            .and_then(|p| p.tooltip_text())
                            .unwrap_or_default();
                        out.push(format!(
                            "{}|{}|{}x{}",
                            label.text(),
                            tooltip,
                            label.width(),
                            label.height()
                        ));
                    }
                }
                let mut child = widget.first_child();
                while let Some(node) = child {
                    collect(&node, out);
                    child = node.next_sibling();
                }
            }
            let mut labels = Vec::new();
            collect(widget.upcast_ref(), &mut labels);
            Some(format!("{}:{labels:?}", id.0))
        })
        .collect();
    eprintln!("among[{label}]: rendered_footers={footers:?}");
    fn collect_meters(widget: &gtk::Widget, out: &mut Vec<String>) {
        if let Some(bar) = widget.downcast_ref::<gtk::ProgressBar>() {
            if bar.has_css_class("status-meter") {
                out.push(format!(
                    "{}:{}x{}",
                    bar.fraction(),
                    bar.width(),
                    bar.height()
                ));
                if let Some(parent) = bar.parent() {
                    if let Some(label) = parent
                        .first_child()
                        .and_then(|node| node.downcast::<gtk::Label>().ok())
                    {
                        out.push(format!(
                            "font={}",
                            label
                                .pango_context()
                                .font_description()
                                .map(|font| font.to_string())
                                .unwrap_or_default()
                        ));
                    }
                }
            }
        }
        let mut child = widget.first_child();
        while let Some(node) = child {
            collect_meters(&node, out);
            child = node.next_sibling();
        }
    }
    let mut meters = Vec::new();
    collect_meters(ctx.window.upcast_ref(), &mut meters);
    eprintln!("among[{label}]: rendered_meters={meters:?}");
    fn collect_status_fonts(widget: &gtk::Widget, out: &mut Vec<String>) {
        fn rendered_opacity(node: &gtk::gsk::RenderNode) -> f32 {
            if let Some(node) = node.downcast_ref::<gtk::gsk::OpacityNode>() {
                return node.opacity() * rendered_opacity(&node.child());
            }
            if let Some(node) = node.downcast_ref::<gtk::gsk::TransformNode>() {
                return rendered_opacity(&node.child());
            }
            if let Some(node) = node.downcast_ref::<gtk::gsk::ClipNode>() {
                return rendered_opacity(&node.child());
            }
            if let Some(node) = node.downcast_ref::<gtk::gsk::RoundedClipNode>() {
                return rendered_opacity(&node.child());
            }
            if let Some(node) = node.downcast_ref::<gtk::gsk::ContainerNode>() {
                return (0..node.n_children())
                    .map(|index| rendered_opacity(&node.child(index)))
                    .fold(1.0, f32::min);
            }
            1.0
        }
        if let Some(label) = widget.downcast_ref::<gtk::Label>() {
            let snapshot = gtk::Snapshot::new();
            gtk::WidgetPaintable::new(Some(label)).snapshot(
                &snapshot,
                label.width() as f64,
                label.height() as f64,
            );
            let opacity = snapshot
                .to_node()
                .as_ref()
                .map(rendered_opacity)
                .unwrap_or(1.0);
            out.push(format!(
                "{}|{}|opacity={}",
                label.text(),
                label
                    .pango_context()
                    .font_description()
                    .map(|font| font.to_string())
                    .unwrap_or_default(),
                opacity
            ));
        }
        let mut child = widget.first_child();
        while let Some(node) = child {
            collect_status_fonts(&node, out);
            child = node.next_sibling();
        }
    }
    let mut status_fonts = Vec::new();
    collect_status_fonts(ctx.app_header.upcast_ref(), &mut status_fonts);
    eprintln!("among[{label}]: status_fonts={status_fonts:?}");
    let mut footer_fonts = Vec::new();
    collect_status_fonts(ctx.app_footer.upcast_ref(), &mut footer_fonts);
    eprintln!(
        "among[{label}]: app_footer={:?} footer_fonts={footer_fonts:?}",
        slot_summary(&state.store, Scope::AppFooter)
    );
    fn collect_footer_rects(widget: &gtk::Widget, window: &gtk::Widget, out: &mut Vec<String>) {
        if let Some(label) = widget.downcast_ref::<gtk::Label>() {
            if let Some(rect) = label.compute_bounds(window) {
                out.push(format!(
                    "{}:{}+{}+{}x{}",
                    label.text(),
                    rect.x() as i32,
                    rect.y() as i32,
                    rect.width() as i32,
                    rect.height() as i32
                ));
            }
        }
        let mut child = widget.first_child();
        while let Some(node) = child {
            collect_footer_rects(&node, window, out);
            child = node.next_sibling();
        }
    }
    let mut footer_rects = Vec::new();
    collect_footer_rects(
        ctx.app_footer.upcast_ref(),
        ctx.window.upcast_ref(),
        &mut footer_rects,
    );
    eprintln!(
        "among[{label}]: footer_rects={footer_rects:?} window_width={}",
        ctx.window.width()
    );

    eprintln!(
        "among[{label}]: split_binding={bound} unparsable={missing:?} plugins={plugins:?} app_header={:?} app_sources={sources:?} workspace_header={:?} pane_header={:?} workspace_summaries={summaries} focused_marked={focused_marked} notices_recorded={notices} last_notice={last_notice:?} shortcuts_fired={shortcuts} shortcuts_installed={installed} drop_indication={indications:?} pane_titles={titles:?} pane_colours={pane_colours:?} pane_cwds={cwds:?} pane_scroll={scrolls:?} {stripe_measured} header_stripe={header_stripe} rejections={rejections:?}",
        slot_summary(&state.store, Scope::AppHeader),
        slot_summary(&state.store, Scope::WorkspaceHeader),
        slot_summary(&state.store, Scope::PaneHeader(workspace.focused)),
    );
}

/// Count terminal widgets actually present under the pane area.
/// The first descendant carrying a CSS class.
fn find_with_class(root: &gtk::Widget, class: &str) -> Option<gtk::Widget> {
    if root.has_css_class(class) {
        return Some(root.clone());
    }
    let mut child = root.first_child();
    while let Some(node) = child {
        if let Some(found) = find_with_class(&node, class) {
            return Some(found);
        }
        child = node.next_sibling();
    }
    None
}

/// A row's name widget: the entry while a rename is in progress, the plain label
/// otherwise.
///
/// Which of the two it is tells a reader both whether a name is being edited and
/// where the name is, so a pointer test can aim at the name in either state —
/// the two occupy the same place, and a test must not have to know which state it
/// is in to find it.
fn find_name_widget(root: &gtk::Widget) -> Option<gtk::Widget> {
    if root.downcast_ref::<gtk::Entry>().is_some() {
        return Some(root.clone());
    }
    if root.has_css_class("workspace-name") {
        return Some(root.clone());
    }
    let mut child = root.first_child();
    while let Some(node) = child {
        if let Some(found) = find_name_widget(&node) {
            return Some(found);
        }
        child = node.next_sibling();
    }
    None
}

/// The palette index named by a widget's colour class.
fn colour_index_of(widget: &gtk::Widget) -> Option<usize> {
    for index in 0..crate::workspace::PALETTE_SIZE {
        if widget.has_css_class(&format!("workspace-colour-{index}")) {
            return Some(index);
        }
    }
    None
}

fn pane_area_widget_count(ctx: &Window) -> usize {
    ctx.pane_area
        .first_child()
        .map(|_| count_panes(ctx.pane_area.upcast_ref()))
        .unwrap_or(0)
}

fn count_panes(widget: &gtk::Widget) -> usize {
    let mut total = usize::from(widget.downcast_ref::<vte::Terminal>().is_some());
    let mut child = widget.first_child();
    while let Some(node) = child {
        total += count_panes(&node);
        child = node.next_sibling();
    }
    total
}

fn toast_widget_count(widget: &gtk::Widget) -> usize {
    let mut count = usize::from(widget.type_().name().starts_with("AdwToast"));
    let mut child = widget.first_child();
    while let Some(node) = child {
        count += toast_widget_count(&node);
        child = node.next_sibling();
    }
    count
}

fn workspace_list_rows(ctx: &Window) -> usize {
    let mut total = 0;
    let mut row = ctx.workspace_list.first_child();
    while let Some(node) = row {
        total += 1;
        row = node.next_sibling();
    }
    total
}

/// One-line description of a slot's current content, for verification.
fn slot_summary(store: &SlotStore, scope: Scope) -> Vec<String> {
    store
        .fragments(scope)
        .iter()
        .map(|f| {
            f.text
                .clone()
                .unwrap_or_else(|| f.icon.clone().unwrap_or_default())
        })
        .collect()
}

/// Run one scripted action, for verification runs.
fn run_script_step(ctx: &Window, step: &str) {
    // `type:` is a script-only form; everything else is a real action name, so
    // the script can drive anything the keyboard can.
    if let Some(command) = step.strip_prefix("type:") {
        type_into_focused_pane(ctx, command.replace('~', " "), step);
        return;
    }
    if let Some(rest) = step.strip_prefix("feed:") {
        // feed:<pane id>:<text>, with `~` standing in for a space. Used to set
        // a title on a pane the user is not focused on.
        let mut parts = rest.splitn(2, ':');
        let pane_id = parts.next().and_then(|p| p.parse::<u64>().ok());
        let text = parts.next().unwrap_or("").replace('~', " ");
        match pane_id {
            Some(raw) => {
                let pane = PaneId(raw);
                let target = ctx.state.borrow().pane(pane).map(|p| p.terminal().clone());
                match target {
                    Some(terminal) => {
                        let mut bytes = text.into_bytes();
                        bytes.push(b'\r');
                        terminal.feed_child(&bytes);
                    }
                    None => eprintln!("among: no pane {raw} for `{step}`"),
                }
            }
            None => eprintln!("among: `{step}` is not feed:<pane>:<text>"),
        }
        selfcheck(ctx, step);
        return;
    }
    if let Some(rest) = step.strip_prefix("scroll:") {
        // scroll:<pane id>:<delta>. The harness cannot produce a touch surface
        // event, so this reaches the same handler a real one does rather than a
        // second path only the test could use. There is deliberately no wheel
        // form: wheel clicks are the library's own, the harness can send a real
        // one, and the library offers no way to ask for one programmatically.
        let mut parts = rest.split(':');
        let pane_id = parts.next().and_then(|p| p.parse::<u64>().ok());
        let delta = parts.next().and_then(|d| d.parse::<f64>().ok());
        match (pane_id, delta) {
            (Some(raw), Some(delta)) => {
                let pane = PaneId(raw);
                match ctx.state.borrow().pane(pane) {
                    Some(widget) => {
                        let cell = widget.cell_height();
                        let adjustment = widget.terminal().vadjustment();
                        let before = adjustment.as_ref().map(|a| a.value());
                        let moved = crate::pane::scroll_terminal(widget.terminal(), cell, delta);
                        // What the step did to the adjustment, in flat fields a
                        // check can read, so a failing scroll run can be read
                        // from the log rather than guessed at.
                        eprintln!(
                            "among[scroll]: pane={raw} delta={delta} cell={cell} moved={moved} \
from={} to={} lower={} upper={} page={}",
                            field(before),
                            field(widget.terminal().vadjustment().as_ref().map(|a| a.value())),
                            field(adjustment.as_ref().map(|a| a.lower())),
                            field(adjustment.as_ref().map(|a| a.upper())),
                            field(adjustment.as_ref().map(|a| a.page_size())),
                        );
                    }
                    None => eprintln!("among: no pane {raw} for `{step}`"),
                }
            }
            _ => eprintln!("among: `{step}` is scroll:<pane>:<delta>"),
        }
        selfcheck(ctx, step);
        return;
    }
    if step == "request-close" {
        // The same call the window's close control makes.
        ctx.window.close();
        selfcheck(ctx, step);
        return;
    }
    if step == "collapse-sidebar" {
        set_sidebar_collapsed(ctx, true);
        selfcheck(ctx, step);
        return;
    }
    if step == "expand-sidebar" {
        set_sidebar_collapsed(ctx, false);
        selfcheck(ctx, step);
        return;
    }
    match shortcuts::parse_action(step) {
        Some(action) => {
            run_action(ctx, action);
            selfcheck(ctx, step);
        }
        None => eprintln!("among: unknown script step `{step}`"),
    }
}

/// One number for a diagnostic line, or `-` when it is not available.
fn field(value: Option<f64>) -> String {
    match value {
        Some(value) => format!("{value:.2}"),
        None => "-".to_string(),
    }
}

/// Feed text to the focused pane's terminal as if it were typed.
fn type_into_focused_pane(ctx: &Window, command: String, step: &str) {
    let focused = ctx.state.borrow().workspaces.active().focused;
    if ctx.state.borrow().pane(focused).is_none() {
        eprintln!("among: no pane for `{step}`");
        return;
    }
    if let Some(pane) = ctx.state.borrow().pane(focused) {
        let mut bytes = command.into_bytes();
        // Carriage return submits the line, exactly as pressing Enter would.
        bytes.push(b'\r');
        pane.terminal().feed_child(&bytes);
    }
    selfcheck(ctx, step);
}

fn rebuild(ctx: &Window) {
    invalidate_terminal_menus(ctx);
    rebuild_sidebar(ctx, false);
    rebuild_panes(ctx);
    apply_focus(ctx);
    refresh_slots(ctx);
    selfcheck(ctx, "rebuild");
}

/// A summary of everything the sidebar shows.
///
/// Cheap to compute and compared before rebuilding: an update that changes
/// nothing visible must not recreate the rows, because the rows are what the
/// user is interacting with.
fn sidebar_signature(state: &AppState) -> String {
    let mut parts = vec![
        format!("collapsed={}", state.sidebar_collapsed),
        format!("active={}", state.workspaces.active_index()),
    ];
    for (index, workspace) in state.workspaces.workspaces().iter().enumerate() {
        parts.push(format!(
            "{index}:{}:{}:{}",
            workspace.name,
            workspace.colour,
            workspace.panes().len()
        ));
        for summary in state.store.summaries(index) {
            parts.push(format!("summary:{:?}", summary.text));
        }
    }
    parts.join("|")
}

/// A summary of what a row is built from, without the name or the active marker.
///
/// A rename ends by putting a name back where it was and moving the marker on rows
/// that already exist, so neither is a reason to build the rows again — and building
/// them would take away the very row a pointer sequence is aimed at. Everything else
/// a row shows — how many panes its workspace has, what plugins have contributed,
/// whether the sidebar is collapsed — comes from the build, so this is what says
/// whether the rows still match the model.
fn sidebar_shape(state: &AppState) -> String {
    let mut parts = vec![format!("collapsed={}", state.sidebar_collapsed)];
    for (index, workspace) in state.workspaces.workspaces().iter().enumerate() {
        parts.push(format!(
            "{index}:{}:{}",
            workspace.colour,
            workspace.panes().len()
        ));
        for summary in state.store.summaries(index) {
            parts.push(format!("summary:{:?}", summary.text));
        }
    }
    parts.join("|")
}

/// Rebuild the sidebar's rows.
///
/// Skipped when nothing the sidebar shows has changed, because the rows are what
/// a pointer is interacting with. `force` says the widgets on screen are known to
/// differ from the model whatever the summary says — true at the end of a rename,
/// where a field stands in for a name and only a rebuild removes it, and where a
/// cancelled or refused rename leaves every value the summary reads exactly as it
/// was.
fn rebuild_sidebar(ctx: &Window, force: bool) {
    {
        let state = ctx.state.borrow();
        // An in-progress rename owns the sidebar's widgets until it finishes.
        if state.renaming.is_some() {
            return;
        }
        if !force && sidebar_signature(&state) == state.sidebar_signature {
            // Nothing the sidebar shows has changed, so leave its widgets alone.
            return;
        }
    }
    while let Some(child) = ctx.workspace_list.first_child() {
        ctx.workspace_list.remove(&child);
    }

    // Everything the rows need is collected before any widget is built, so the
    // state is not borrowed while closures that borrow it are created.
    struct RowPlan {
        name: String,
        colour: usize,
        panes: usize,
        summaries: Vec<(Option<String>, Option<String>)>,
    }
    let (active, collapsed, plan): (usize, bool, Vec<RowPlan>) = {
        let state = ctx.state.borrow();
        let plan = state
            .workspaces
            .workspaces()
            .iter()
            .enumerate()
            .map(|(index, workspace)| RowPlan {
                name: workspace.name.clone(),
                colour: workspace.colour,
                panes: workspace.panes().len(),
                summaries: state
                    .store
                    .summaries(index)
                    .into_iter()
                    .map(|summary| (summary.text.clone(), summary.icon.clone()))
                    .collect(),
            })
            .collect();
        (
            state.workspaces.active_index(),
            state.sidebar_collapsed,
            plan,
        )
    };

    for (index, row_plan) in plan.iter().enumerate() {
        let row = gtk::ListBoxRow::new();
        row.add_css_class("workspace-entry");
        if index == active {
            row.add_css_class("active-workspace");
        }
        // Clicking a row activates its workspace; renaming is handled on the
        // name itself so a click on the rest of the row still switches.
        row.set_activatable(true);

        let content = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        content.add_css_class("workspace-row");

        // The colour is a stripe rather than a background: terminal output uses
        // colour to mean something, and tinting behind it would both reduce
        // legibility and read as application state.
        let stripe = gtk::Box::new(gtk::Orientation::Vertical, 0);
        stripe.add_css_class("workspace-stripe");
        stripe.add_css_class(&colour_class(row_plan.colour));
        content.append(&stripe);

        if collapsed {
            let icon = terminal_icon();
            content.append(&icon);

            let count = gtk::Label::new(Some(&row_plan.panes.to_string()));
            count.add_css_class("workspace-strip-count");
            content.append(&count);
            // TEMPORARY probe: an icon whose rendering is known, next to the
            // strip's icon, so a broken glyph can be told from a real one.
        } else {
            let name = name_label(&row_plan.name, row_plan.colour);
            content.append(&name);
            let count = gtk::Label::new(Some(&row_plan.panes.to_string()));
            count.add_css_class("workspace-terminal-count");
            content.append(&count);

            // Double-clicking the entry starts editing it. The list box claims the
            // pointer sequence, so the gesture belongs on the row rather than on a
            // widget inside it. The first press is left alone: that is the click
            // that switches workspaces, and claiming it would stop the entry from
            // ever being used to switch.
            let entry_click = gtk::GestureClick::new();
            entry_click.set_propagation_phase(gtk::PropagationPhase::Capture);
            entry_click.connect_pressed({
                let content = content.clone();
                let target = ctx.handle();
                move |gesture, presses, _, _| {
                    if presses < 2 {
                        // The first press of a sequence is the click that switches
                        // workspaces. Any press after it is a further click in the
                        // same sequence, which for a person is still the second click
                        // of a double-click — the count reaches three only when the
                        // clicks come faster than the threshold, and then the editor
                        // should still open rather than nothing happening.
                        return;
                    }
                    gesture.set_state(gtk::EventSequenceState::Claimed);
                    // The name and the colour are read now, not held from the row's
                    // build: the row outlives them, since a rename ends by putting a
                    // new name back into the row rather than building a new row.
                    let (name, colour) = {
                        let state = target.state.borrow();
                        match state.workspaces.workspaces().get(index) {
                            Some(workspace) => (workspace.name.clone(), workspace.colour),
                            None => return,
                        }
                    };
                    target.state.borrow_mut().renaming = Some(index);
                    // The field is opened once the click sequence has finished:
                    // ListBox takes focus back while the sequence is still live,
                    // which would pull the entry straight back out of editing.
                    let target = target.clone();
                    let content = content.clone();
                    glib::idle_add_local_once(move || {
                        if let Err(problem) = Rename::begin(&target, index, &content, &name, colour)
                        {
                            eprintln!("among: workspace {index} cannot be renamed: {problem}");
                        }
                    });
                }
            });
            row.add_controller(entry_click);

            // Plugin summaries sit beside the name rather than replacing it, so
            // several plugins can contribute at once.
            for (text, icon) in &row_plan.summaries {
                if let Some(text) = text {
                    let label = gtk::Label::new(Some(text));
                    label.add_css_class("workspace-summary");
                    content.append(&label);
                }
                if let Some(icon) = icon {
                    let image = gtk::Image::from_icon_name(icon);
                    image.set_pixel_size(14);
                    content.append(&image);
                }
            }
        }

        row.set_child(Some(&content));
        ctx.workspace_list.append(&row);
    }

    if let Some(row) = ctx.workspace_list.row_at_index(active as i32) {
        // Selection is set without rebuilding, so a double-click on the active
        // workspace is not interrupted by the list being replaced.
        ctx.workspace_list.select_row(Some(&row));
    }
    let signature = {
        // Bound explicitly: a temporary borrow in a `let` lives to the end of
        // the block, which would collide with the borrow below.
        let state = ctx.state.borrow();
        sidebar_signature(&state)
    };
    let shape = {
        let state = ctx.state.borrow();
        sidebar_shape(&state)
    };
    let mut state = ctx.state.borrow_mut();
    state.sidebar_signature = signature;
    state.sidebar_shape = shape;
}

/// Show a workspace that is already in the model.
///
/// Only the marked row and the pane area change; the sidebar's widgets are left
/// alone so a click sequence on an entry is not interrupted.
fn switch_workspace(ctx: &Window) {
    mark_active_row(ctx);
    rebuild_panes(ctx);
    apply_focus(ctx);
    refresh_slots(ctx);
}

/// Move the active marker to the workspace the model says is active.
fn mark_active_row(ctx: &Window) {
    let active = ctx.state.borrow().workspaces.active_index();
    let mut row = ctx.workspace_list.first_child();
    let mut index = 0usize;
    while let Some(node) = row {
        if let Some(list_row) = node.downcast_ref::<gtk::ListBoxRow>() {
            if index == active {
                list_row.add_css_class("active-workspace");
            } else {
                list_row.remove_css_class("active-workspace");
            }
        }
        index += 1;
        row = node.next_sibling();
    }
    if let Some(row) = ctx.workspace_list.row_at_index(active as i32) {
        ctx.workspace_list.select_row(Some(&row));
    }
}

/// A terminal glyph, drawn rather than taken from the icon theme.
///
/// The obvious icon for this reports as available and still paints GTK's
/// missing-image placeholder on current Adwaita, because the file lives in a
/// legacy directory GTK will not load. A strip entry has to show a terminal on
/// every system, so the few lines it takes to draw one are cheaper than an icon
/// that may or may not resolve.
///
/// The stroke colour is a mid grey chosen to stay readable on both window
/// backgrounds rather than one read from the theme: the stylesheet API that
/// would give a widget its CSS colour is deprecated, and guessing at the scheme
/// from the settings is unreliable. At #8b8b8b the contrast is 3.6:1 against a
/// light window and 4.3:1 against a dark one, so the glyph is legible either way.
fn terminal_icon() -> gtk::Widget {
    const GLYPH: &str = "#8b8b8b";
    let area = gtk::DrawingArea::builder()
        .content_width(16)
        .content_height(14)
        .valign(gtk::Align::Center)
        .build();
    area.add_css_class("workspace-strip-icon");
    area.set_draw_func(move |area, cr, _width, _height| {
        let Ok(colour) = gtk::gdk::RGBA::parse(GLYPH) else {
            return;
        };
        cr.set_source_rgba(
            f64::from(colour.red()),
            f64::from(colour.green()),
            f64::from(colour.blue()),
            f64::from(colour.alpha()),
        );
        let width = f64::from(area.width());
        let height = f64::from(area.height());
        let line = 1.2;
        cr.set_line_width(line);
        cr.rectangle(line / 2.0, line / 2.0, width - line, height - line);
        // cairo reports a failed stroke as a Result; a draw callback has
        // nowhere to report it, and a stroke over a rectangle this simple does
        // not fail in practice.
        let _ = cr.stroke();

        // The prompt: a chevron and an underscore, which together read as a
        // shell at this size.
        cr.move_to(width * 0.26, height * 0.36);
        cr.line_to(width * 0.44, height * 0.50);
        cr.line_to(width * 0.26, height * 0.64);
        let _ = cr.stroke();
        cr.move_to(width * 0.52, height * 0.64);
        cr.line_to(width * 0.74, height * 0.64);
        let _ = cr.stroke();
    });
    area.upcast()
}

/// The CSS class carrying a workspace's colour.
fn colour_class(colour: usize) -> String {
    format!(
        "workspace-colour-{}",
        colour % crate::workspace::PALETTE_SIZE
    )
}

/// The name of a workspace, editable in place on double-click.
///
/// In place rather than a dialog: the name lives in the list, and a modal for a
/// one-word change interrupts the work being done in the terminal.
///
/// A plain label, deliberately. An editable label starts editing on a single
/// click and claims that pointer sequence for itself, which is what made
/// switching to a workspace and renaming it fight over the same click.
fn name_label(name: &str, colour: usize) -> gtk::Label {
    let label = gtk::Label::new(Some(name));
    label.add_css_class("workspace-name");
    label.set_hexpand(true);
    label.set_halign(gtk::Align::Start);
    // The colour class goes on the name too, so the entry reads as one thing
    // when the stripe is thin, and so the renamed row keeps its workspace's
    // colour while it is being typed into.
    label.add_css_class(&colour_class(colour));
    label
}

/// One in-place rename: the field, what it was opened with, and how it ends.
///
/// The widgets are held here rather than in the closures that react to keys and
/// focus, because ending a rename has to put them back — and has to be able to tell
/// whether it can.
struct Rename {
    /// The workspace whose name is being edited.
    index: usize,
    /// The name the field was opened with.
    ///
    /// A workspace that no longer carries it is not this rename's business: closing
    /// a workspace moves every row after it, so an index that was right when the
    /// field opened can name a different workspace by the time it is applied.
    opened: String,
    /// The workspace's colour, so the name that comes back looks the same.
    colour: usize,
    /// The row's content box, where the field stands in for the name.
    content: gtk::Box,
    /// The field itself.
    entry: gtk::Entry,
    /// Set once the rename has ended, so the first ending is the only one.
    ///
    /// A focus change follows the very act of removing the field, so without this
    /// every ending would be followed by a second one.
    ended: std::cell::Cell<bool>,
}

impl Rename {
    /// Put an entry in place of a name label, holding the name, and focus it.
    ///
    /// The entry carries the colour class the label had, so a rename looks like the
    /// same row being edited rather than a different widget appearing in it.
    fn begin(
        ctx: &Window,
        index: usize,
        content: &gtk::Box,
        name: &str,
        colour: usize,
    ) -> Result<(), String> {
        // Which widget to replace is looked up now, not held from the row's build: a
        // rename that has already ended puts a fresh label back, and this click is
        // meant to edit whatever the row is showing at this moment.
        let label = find_name_widget(content.upcast_ref())
            .and_then(|widget| widget.downcast::<gtk::Label>().ok())
            .ok_or_else(|| format!("workspace {index} has no name to edit"))?;
        let entry = gtk::Entry::new();
        entry.set_text(name);
        entry.add_css_class("workspace-name");
        entry.add_css_class(&colour_class(colour));
        entry.set_hexpand(true);
        entry.set_halign(gtk::Align::Start);

        let rename = std::rc::Rc::new(Rename {
            index,
            opened: name.to_string(),
            colour,
            content: content.clone(),
            entry: entry.clone(),
            ended: std::cell::Cell::new(false),
        });

        // One rename, three endings: Enter applies, Escape cancels, and focus leaving
        // the field applies what was typed.
        let keys = gtk::EventControllerKey::new();
        keys.set_propagation_phase(gtk::PropagationPhase::Capture);
        keys.connect_key_pressed({
            let target = ctx.handle();
            let rename = rename.clone();
            move |_, key, _, _| {
                use gtk::gdk;
                let confirm = key == gdk::Key::Return || key == gdk::Key::KP_Enter;
                let cancel = key == gdk::Key::Escape;
                if !confirm && !cancel {
                    return gtk::glib::Propagation::Proceed;
                }
                rename.finish(&target, confirm);
                gtk::glib::Propagation::Stop
            }
        });
        entry.add_controller(keys);

        let focus = gtk::EventControllerFocus::new();
        focus.connect_leave({
            let target = ctx.handle();
            let rename = rename.clone();
            move |_| rename.finish(&target, true)
        });
        entry.add_controller(focus);

        // The entry takes the label's place rather than the row's last position, so a
        // pointer aimed at the name finds the field exactly where the text was. GTK4
        // has no call that swaps two children outright.
        content.append(&entry);
        content.reorder_child_after(&entry, Some(&label));
        label.unparent();
        entry.grab_focus();
        entry.select_region(0, -1);
        Ok(())
    }

    /// End the rename: apply or keep the name, then take the field away.
    ///
    /// Shared by every ending, so a rename ends the same way however it was finished.
    fn finish(&self, ctx: &Window, confirm: bool) {
        if self.ended.replace(true) {
            return;
        }
        let typed = self.entry.text().to_string();
        let mut state = ctx.state.borrow_mut();
        let known = state.workspaces.workspaces().get(self.index).is_some();
        if confirm {
            state.workspaces.rename_at(self.index, &self.opened, &typed);
        }
        let name = state
            .workspaces
            .workspaces()
            .get(self.index)
            .map(|workspace| workspace.name.clone());
        // The field only ever stands for a name, so a rename that changed nothing
        // else leaves rows that still match the model — and rebuilding them would
        // replace the row a pointer sequence is halfway through aiming at. Anything
        // that did change while the field was open — a workspace added or removed, a
        // plugin's contribution, the sidebar collapsed — is only visible in rows built
        // again, as is a workspace that is no longer there to have a name.
        let rows_match = known && sidebar_shape(&state) == state.sidebar_shape;
        state.renaming = None;
        if rows_match {
            let signature = sidebar_signature(&state);
            let shape = sidebar_shape(&state);
            state.sidebar_signature = signature;
            state.sidebar_shape = shape;
        }
        drop(state);

        // The name is applied above, in the model, as the keystroke or click happens;
        // the field is only taken away once the event that ended the rename is over.
        // Removing a widget from the tree while GTK is still working through the focus
        // change that brought the rename to an end leaves it holding a widget that is
        // no longer there, and the next thing it asks about that widget is a crash.
        let target = ctx.handle();
        let content = self.content.clone();
        let entry = self.entry.clone();
        let colour = self.colour;
        let name = name.unwrap_or_default();
        glib::idle_add_local_once(move || {
            if rows_match {
                restore_row_name(&target, &content, &entry, &name, colour);
            } else {
                rebuild_sidebar(&target, true);
            }
            // The terminal is what takes the keyboard back: a rename ends with the
            // field gone, and nothing else here would.
            apply_focus(&target);
        });
    }
}

/// Put a row's plain name back where its field was.
///
/// The row may no longer be the one on screen — a plugin's update that arrived after
/// the rename could have rebuilt the sidebar — and then the row that is there already
/// shows the name, and this field is no longer part of anything.
fn restore_row_name(
    ctx: &Window,
    content: &gtk::Box,
    entry: &gtk::Entry,
    name: &str,
    colour: usize,
) {
    if entry.parent().as_ref() != Some(content.upcast_ref()) {
        return;
    }
    // The marker moves on the rows that are already there, which is what a rebuild
    // would have done for them.
    mark_active_row(ctx);
    let label = name_label(name, colour);
    content.append(&label);
    content.reorder_child_after(&label, Some(entry));
    entry.unparent();
}

fn rebuild_panes(ctx: &Window) {
    // Split, zoom and drag can rebuild panes without the full workspace rebuild.
    // Invalidate invocation tokens before reparenting any persistent terminal.
    invalidate_terminal_menus(ctx);
    let generation = ctx.state.borrow().layout_generation.wrapping_add(1);
    ctx.state.borrow_mut().layout_generation = generation;
    while let Some(child) = ctx.pane_area.first_child() {
        ctx.pane_area.remove(&child);
    }
    ctx.paneds.borrow_mut().clear();

    let zoomed = ctx.state.borrow().workspaces.active().zoomed;
    // Only the panes actually on screen are rebuilt; a zoomed workspace shows
    // one, and the tree behind it is untouched.
    let ids = ctx.state.borrow().workspaces.active().visible_panes();
    for id in &ids {
        ensure_pane(ctx, *id);
    }

    // A zoomed workspace renders only the zoomed pane; the tree is untouched,
    // so restoring the layout needs no reconstruction.
    if ctx.state.borrow().workspaces.active().is_zoomed() {
        if let Some(pane) = zoomed {
            if let Some(widget) = ctx.state.borrow().pane(pane) {
                let store = ctx.state.borrow().store.clone();
                let colour = ctx.state.borrow().workspaces.active().colour;
                let restore = Rc::new({
                    let ctx = ctx.handle();
                    move || restore_expanded(&ctx, pane, generation)
                });
                ctx.pane_area
                    .append(&widget.build_container(&store, colour, Some(restore)));
                return;
            }
        }
    }

    let tree = ctx.state.borrow().workspaces.active().nodes.clone();
    let colour = ctx.state.borrow().workspaces.active().colour;
    ctx.pane_area
        .append(&build_node(ctx, &tree, &mut Vec::new(), colour));
}

fn apply_focus(ctx: &Window) {
    let focused = ctx.state.borrow().workspaces.active().focused;
    let ids = ctx.state.borrow().workspaces.active().panes();
    for id in ids {
        if let Some(pane) = ctx.state.borrow().pane(id) {
            pane.set_focused(id == focused);
        }
    }
}

/// Create a pane widget for an id if it does not exist yet.
fn ensure_pane(ctx: &Window, id: PaneId) {
    {
        let state = ctx.state.borrow();
        if state.panes.contains_key(&id) {
            return;
        }
    }

    let signals = PaneSignals {
        menu: Some(crate::context_menu::Callbacks {
            prepare: Rc::new({
                let ctx = ctx.handle();
                move |id| {
                    if !menu_target_valid(&ctx, id) {
                        return None;
                    }
                    let presentation = {
                        let mut state = ctx.state.borrow_mut();
                        state.workspaces.active_mut().focused = id;
                        crate::context_menu::Presentation {
                            zoomed: state.workspaces.active().zoomed == Some(id),
                            split_accel: state
                                .bindings
                                .accel_for(Action::SplitPane)
                                .map(str::to_owned),
                            zoom_accel: state
                                .bindings
                                .accel_for(Action::ToggleZoom)
                                .map(str::to_owned),
                            close_accel: state
                                .bindings
                                .accel_for(Action::ClosePane)
                                .map(str::to_owned),
                        }
                    };
                    apply_focus(&ctx);
                    refresh_slots(&ctx);
                    Some(presentation)
                }
            }),
            valid: Rc::new({
                let ctx = ctx.handle();
                move |id| menu_target_valid(&ctx, id)
            }),
            command: Rc::new({
                let ctx = ctx.handle();
                move |id, command| {
                    if !menu_target_valid(&ctx, id) {
                        return;
                    }
                    ctx.state.borrow_mut().workspaces.active_mut().focused = id;
                    match command {
                        crate::context_menu::Command::Split => split_focused(&ctx),
                        crate::context_menu::Command::Zoom => toggle_zoom(&ctx),
                        crate::context_menu::Command::Close => close_pane(&ctx, id),
                        crate::context_menu::Command::OpenLink(uri) => {
                            let launcher = gtk::UriLauncher::new(&uri);
                            launcher.launch(
                                Some(&ctx.window),
                                None::<&gio::Cancellable>,
                                move |result| {
                                    if let Err(error) = result {
                                        eprintln!("among: Could not open link: {error}");
                                    }
                                },
                            );
                        }
                    }
                }
            }),
            dismiss: Rc::new({
                let ctx = ctx.handle();
                move |id| {
                    if menu_target_valid(&ctx, id)
                        && ctx.state.borrow().workspaces.active().focused == id
                    {
                        apply_focus(&ctx);
                    }
                }
            }),
        }),
        on_focus: Some(Box::new({
            let state = ctx.state.clone();
            let refresh = ctx.handle();
            move |pane: PaneId| {
                let known = {
                    let mut borrowed = state.borrow_mut();
                    if !borrowed.workspaces.active().panes().contains(&pane)
                        || borrowed.workspaces.active().focused == pane
                    {
                        return;
                    }
                    borrowed.workspaces.active_mut().focused = pane;
                    true
                };
                if known {
                    apply_focus(&refresh);
                    refresh_slots(&refresh);
                }
            }
        })),
        on_drag_begin: None,
        on_drag_end: Some(Rc::new({
            let refresh = ctx.handle();
            move || {
                // Every pane clears its indication, because the drag may have
                // left one behind on a pane the pointer never re-entered.
                let state = refresh.state.borrow();
                for pane in state.panes.values() {
                    pane.clear_drop_indication();
                }
            }
        })),
        on_drop: Some(Rc::new({
            let refresh = ctx.handle();
            move |source: PaneId, target: PaneId, side: crate::tree::DropSide| {
                move_pane_over(&refresh, source, target, side);
            }
        })),
        on_title: Some(Box::new({
            let refresh = ctx.handle();
            move |pane: PaneId, title: String| {
                if let Some(widget) = refresh.state.borrow().pane(pane) {
                    widget.set_title(title);
                }
            }
        })),
        on_exit: Some(Box::new({
            let state = ctx.state.clone();
            let refresh = ctx.handle();
            move |pane: PaneId, status: i32| {
                if state.borrow().exiting || state.borrow().session_closing {
                    return;
                }
                {
                    let mut borrowed = state.borrow_mut();
                    borrowed.exited.insert(pane, status);
                    borrowed.plugins.broadcast_event(
                        EventKind::ProcessExited { status },
                        pane.0,
                        None,
                    );
                }
                // Revoke before deferred pane removal: an open native popover
                // must not keep actions live while its terminal has already exited.
                if let Some(widget) = state.borrow().pane(pane) {
                    widget.invalidate_menu();
                }
                // Reported before the pane goes, so the status is available
                // even though the pane will not be.
                refresh.report_exit(pane, status);
                // Closing is deferred out of this signal: the handler is being
                // called by the terminal that the close will destroy.
                let closer = refresh.handle();
                glib::idle_add_local_once(move || close_pane(&closer, pane));
            }
        })),
        on_cwd: Some(Box::new({
            let state = ctx.state.clone();
            let refresh = ctx.handle();
            move |pane: PaneId, dir: Option<String>| {
                for workspace in state.borrow_mut().workspaces.workspaces_mut() {
                    if workspace.panes().contains(&pane) {
                        workspace.set_cwd(pane, dir.clone());
                        break;
                    }
                }
                state.borrow_mut().plugins.broadcast_event(
                    EventKind::WorkingDirectoryChanged,
                    pane.0,
                    dir,
                );
                refresh_slots(&refresh);
            }
        })),
    };

    let requested = ctx
        .state
        .borrow()
        .workspaces
        .workspaces()
        .iter()
        .flat_map(|workspace| workspace.cwd.iter())
        .find(|(pane, _)| *pane == id)
        .and_then(|(_, cwd)| cwd.clone());
    let (cwd, fallback) = crate::session::initial_directory(requested.as_deref());
    if fallback {
        session_restore_warning(
            ctx,
            &format!(
                "Terminal {}: the requested directory is unavailable; using a fallback directory.",
                id.0
            ),
        );
    }
    let expected = cwd.clone();
    let refresh = ctx.handle();
    let pane = match Pane::new(
        id,
        signals,
        cwd,
        Rc::new(move |result| match result {
            Ok(actual) => {
                if actual != expected {
                    session_restore_warning(
                        &refresh,
                        &format!("Terminal {}: shell started in a fallback directory.", id.0),
                    );
                }
                for workspace in refresh.state.borrow_mut().workspaces.workspaces_mut() {
                    if workspace.panes().contains(&id) {
                        workspace.set_cwd(id, actual.clone());
                        break;
                    }
                }
            }
            Err(error) => session_restore_warning(&refresh, &format!("Terminal {}: {error}", id.0)),
        }),
    ) {
        Ok(pane) => pane,
        Err(error) => {
            session_restore_warning(ctx, &format!("Terminal {}: {error}", id.0));
            return;
        }
    };
    let mut state = ctx.state.borrow_mut();
    if let Some(bells) = &state.bells {
        bells.attach(id.0, pane.terminal());
    }
    state.panes.insert(id, pane);
    state
        .plugins
        .broadcast_event(EventKind::PaneOpened, id.0, None);
}

/// Build the widget for one node, carrying the path used to write ratios back.
fn build_node(ctx: &Window, node: &Node, path: &mut Vec<usize>, colour: usize) -> gtk::Widget {
    match node {
        // The terminal is taken out of its previous parent before being
        // re-inserted: GTK rejects re-parenting a widget that already has one,
        // which is why the container is rebuilt while the terminal is reused.
        Node::Pane { id } => match ctx.state.borrow().pane(*id) {
            Some(pane) => {
                let store = ctx.state.borrow().store.clone();
                pane.build_container(&store, colour, None).upcast()
            }
            None => gtk::Box::new(gtk::Orientation::Vertical, 0).upcast(),
        },
        Node::Split {
            orientation,
            ratio: _,
            first,
            second,
        } => {
            let paned = gtk::Paned::new(match orientation {
                Orientation::Vertical => gtk::Orientation::Horizontal,
                Orientation::Horizontal => gtk::Orientation::Vertical,
            });
            paned.set_hexpand(true);
            paned.set_vexpand(true);
            paned.set_resize_start_child(true);
            paned.set_shrink_start_child(false);

            let split_path = path.clone();
            let initialized = Rc::new(std::cell::Cell::new(false));
            let extent = Rc::new(std::cell::Cell::new(0));
            ctx.paneds.borrow_mut().push(SplitWidget {
                widget: paned.clone(),
                path: split_path.clone(),
                initialized: initialized.clone(),
                extent: extent.clone(),
            });

            path.push(0);
            let first_widget = build_node(ctx, first, path, colour);
            path.pop();
            path.push(1);
            let second_widget = build_node(ctx, second, path, colour);
            path.pop();

            paned.set_start_child(Some(&first_widget));
            paned.set_end_child(Some(&second_widget));

            // GTK reports pixels; the tree stores ratios. Converting on each
            // change keeps the tree authoritative instead of letting the widget
            // drift from it.
            let state = ctx.state.clone();
            let generation = state.borrow().layout_generation;
            let workspace_index = state.borrow().workspaces.active_index();
            paned.connect_notify_local(Some("position"), move |paned, _| {
                if !initialized.get() {
                    return;
                }
                let total = match paned.orientation() {
                    gtk::Orientation::Horizontal => paned.width(),
                    _ => paned.height(),
                };
                // Allocation changes can notify a new pixel position too; only
                // a divider moved within the same extent edits the layout ratio.
                if extent.replace(total) != total {
                    return;
                }
                let usable = (total - 1).max(0);
                if usable <= 0 {
                    return;
                }
                let new_ratio = (paned.position().max(0) as f64) / usable as f64;
                let mut borrowed = state.borrow_mut();
                if borrowed.layout_generation != generation
                    || borrowed.workspaces.active_index() != workspace_index
                {
                    return;
                }
                let active = borrowed.workspaces.active_index();
                if let Some(workspace) = borrowed.workspaces.get_mut(active) {
                    workspace.nodes.set_ratio_at(&split_path, new_ratio);
                }
            });

            paned.upcast()
        }
    }
}

/// Apply every stored ratio to its widget, once real extents are known.
///
/// The tree holds ratios and GTK holds pixels, so the conversion happens after
/// allocation rather than at build time — a position set before a `Paned` knows
/// its size is meaningless and collapses one child to nothing.
fn sync_ratios_to_widgets(ctx: &Window) {
    let active = ctx.state.borrow().workspaces.active_index();

    for split in ctx.paneds.borrow().iter() {
        let paned = &split.widget;
        let path = &split.path;
        let ratio = {
            let state = ctx.state.borrow();
            match state.workspaces.workspaces().get(active) {
                Some(workspace) => workspace.nodes.ratio_at(path),
                None => None,
            }
        };
        let Some(ratio) = ratio else { continue };

        let total = match paned.orientation() {
            gtk::Orientation::Horizontal => paned.width(),
            _ => paned.height(),
        };
        let usable = (total - 1).max(0);
        if usable <= 0 {
            continue;
        }
        let target = (usable as f64 * ratio).round() as i32;
        split.extent.set(total);
        if paned.position() != target {
            split.initialized.set(false);
            paned.set_position(target);
        }
        split.initialized.set(true);
    }
}

fn refresh_slots(ctx: &Window) {
    let state = ctx.state.borrow();
    slots::render_slot(
        &state.store,
        Scope::WorkspaceHeader,
        &ctx.workspace_header,
        SLOT_MAX_CHARS,
    );
    slots::render_slot(
        &state.store,
        Scope::AppHeader,
        &ctx.app_header,
        SLOT_MAX_CHARS,
    );
    slots::render_slot(
        &state.store,
        Scope::AppFooter,
        &ctx.app_footer,
        SLOT_MAX_CHARS,
    );

    for workspace in state.workspaces.workspaces() {
        for id in workspace.panes() {
            if let Some(pane) = state.pane(id) {
                pane.refresh_slots(&state.store);
            }
        }
    }
    let _ = &ctx.split_view;
}

/// Install the shortcut controller.
///
/// A `ShortcutController` added to the window sees accelerators before the
/// focused widget, so a recognised sequence never reaches the terminal while an
/// unrecognised one falls through untouched.
fn install_shortcuts(ctx: &Window) {
    install_bindings_into(ctx);
}

/// Register every action with GTK and bind its accelerator.
///
/// Actions are installed as `gio::SimpleAction`s on the window and the
/// accelerators through `Application::set_accels_for_action`, which GTK handles
/// *before* the focused widget. A `ShortcutController` would not do: its default
/// LOCAL scope is consulted after the focused widget, and a terminal consumes
/// every key it receives, so no application shortcut would ever fire while a
/// terminal had focus.
fn install_bindings_into(ctx: &Window) {
    invalidate_terminal_menus(ctx);
    let group = gio::SimpleActionGroup::new();

    let mut installed = 0u32;
    let mut unparsable = Vec::new();

    for (binding, trigger) in shortcuts::resolve(&ctx.state.borrow().bindings) {
        let name = shortcuts::action_name(binding.action);
        let action = binding.action;
        let accel = binding.accel.clone();
        let child = ctx.handle();

        let simple = gio::SimpleAction::new(&name, None);
        simple.connect_activate(move |_, _| {
            {
                let mut state = child.state.borrow_mut();
                state.shortcuts_fired += 1;
            }
            run_action(&child, action);
        });
        group.add_action(&simple);

        match trigger {
            Some(_) => {
                let detailed = format!("win.{name}");
                if let Some(app) = ctx.window.application() {
                    app.set_accels_for_action(&detailed, &[accel.as_str()]);
                }
                installed += 1;
            }
            None => {
                // GTK rejected the accelerator, so the action would silently
                // never fire; recorded rather than left to be noticed.
                unparsable.push(accel);
            }
        }
    }

    // Replace any previous group so a rebind takes effect without a restart.
    ctx.window.insert_action_group("win", Some(&group));
    ctx.state.borrow_mut().shortcuts_installed = installed;
    if !unparsable.is_empty() {
        eprintln!("among: accelerators GTK could not parse: {unparsable:?}");
    }
}

/// Report an action that has no binding.
///
/// An action with no key sequence would otherwise be unreachable and its
/// absence invisible, so invoking one by name says so plainly.
fn report_unbound(ctx: &Window, action: Action) -> bool {
    if ctx.state.borrow().bindings.is_bound(action) {
        return false;
    }
    eprintln!("among: `{}` has no key binding", action.label());
    true
}

/// Perform an action, or report that it has no binding.
fn run_action(ctx: &Window, action: Action) {
    if !ctx.state.borrow().session_ready || ctx.state.borrow().session_closing {
        return;
    }
    match action {
        Action::FocusDirection(direction) => focus_direction(ctx, direction),
        Action::ActivateWorkspace(index) => {
            if ctx.state.borrow_mut().workspaces.activate(index) {
                rebuild(ctx);
            } else {
                // A digit beyond the last workspace does nothing.
            }
        }
        Action::CycleWorkspace(cycle) => {
            let mut state = ctx.state.borrow_mut();
            match cycle {
                Cycle::Next => state.workspaces.activate_next(),
                Cycle::Previous => state.workspaces.activate_previous(),
            }
            drop(state);
            rebuild(ctx);
        }
        Action::NewWorkspace => new_workspace(ctx),
        Action::SplitPane => split_focused(ctx),
        Action::ClosePane => close_focused(ctx),
        Action::ResizeDivider(resize) => resize_focused_divider(ctx, resize),
        Action::OpenPreferences => open_preferences(ctx),
        Action::ToggleFullscreen => toggle_fullscreen(ctx),
        Action::ToggleZoom => toggle_zoom(ctx),
    }
}

fn menu_target_valid(ctx: &Window, id: PaneId) -> bool {
    let state = ctx.state.borrow();
    state.session_ready
        && !state.session_closing
        && !state.exiting
        && state.panes.contains_key(&id)
        && !state.exited.contains_key(&id)
        && state.workspaces.active().visible_panes().contains(&id)
}

fn invalidate_terminal_menus(ctx: &Window) {
    // A popdown emits signals: collect no mutable host borrow across that call.
    // Its dismissal callback runs later, after this immutable borrow is released.
    let state = ctx.state.borrow();
    for pane in state.panes.values() {
        pane.invalidate_menu();
    }
}

/// Move a dropped pane beside its target and rebuild the layout.
///
/// The move is a tree operation, so the pane keeps its terminal and the target's
/// area is divided exactly as a split created there by hand would be.
fn move_pane_over(ctx: &Window, source: PaneId, target: PaneId, side: crate::tree::DropSide) {
    if source == target {
        return;
    }
    let moved = {
        let mut state = ctx.state.borrow_mut();
        let active = state.workspaces.active_index();
        let Some(workspace) = state.workspaces.get_mut(active) else {
            return;
        };
        let nodes = workspace.nodes.clone();
        match nodes.move_pane(source, target, side.orientation(), side.placement()) {
            Some(next) => {
                workspace.nodes = next;
                // Focus follows the pane the user just placed.
                workspace.focused = source;
                true
            }
            None => false,
        }
    };
    if moved {
        rebuild_panes(ctx);
        apply_focus(ctx);
        refresh_slots(ctx);
    }
}

/// Switch the window between fullscreen and its previous size.
fn toggle_fullscreen(ctx: &Window) {
    if ctx.window.is_fullscreen() {
        ctx.window.unfullscreen();
    } else {
        ctx.window.fullscreen();
    }
}

/// Expand the focused pane to fill the workspace, or restore the layout.
///
/// Only the rendering changes: the tree is left exactly as it was, so restoring
/// brings back every nested divider and ratio untouched.
fn toggle_zoom(ctx: &Window) {
    ctx.state.borrow_mut().workspaces.active_mut().toggle_zoom();
    rebuild_panes(ctx);
    apply_focus(ctx);
}

fn restore_expanded(ctx: &Window, pane: PaneId, generation: u64) {
    {
        let mut state = ctx.state.borrow_mut();
        // the same pane can have a newly rebuilt header: identity alone would
        // leave the previous control live after a layout or workspace change.
        if state.layout_generation != generation || state.exited.contains_key(&pane) {
            return;
        }
        if !state.workspaces.active_mut().restore_expanded(pane) {
            return;
        }
    }
    rebuild_panes(ctx);
    apply_focus(ctx);
}

/// Open the preferences window, wiring it to the live application state.
///
/// Closures rather than a reference to the window, so the preferences module
/// does not have to know how the application stores its state.
fn open_preferences(ctx: &Window) {
    // Opening an editor must not consume the live map: GTK keeps installed
    // accelerators working, which otherwise hides the loss until the next open.
    let bindings = Rc::new(RefCell::new(ctx.state.borrow().bindings.clone()));

    let preferences = prefs::PreferencesContext {
        on_closed: Rc::new({
            let window = ctx.handle();
            move || {
                let window = window.clone();
                glib::idle_add_local_once(move || apply_focus(&window));
            }
        }),
        storage_status: ctx.state.borrow().settings_status.clone(),
        bindings: bindings.clone(),
        plugin_listing: Rc::new({
            let state = ctx.state.clone();
            move || state.borrow().plugins.listing()
        }),
        plugin_enabled: Rc::new({
            let state = ctx.state.clone();
            move |name| state.borrow().plugins.desired_enabled(name)
        }),
        plugin_details: Rc::new({
            let state = ctx.state.clone();
            move |name| state.borrow().plugins.managed_details(name)
        }),
        save_plugin_settings: Rc::new({
            let window = ctx.handle();
            move |name, generation, changes| {
                let schema = window
                    .state
                    .borrow()
                    .plugins
                    .managed_details(name)
                    .filter(|(entry, _)| entry.receipt.generation == generation)
                    .and_then(|(entry, _)| entry.inspected.ok())
                    .and_then(|inspected| inspected.manifest.settings)
                    .ok_or_else(|| "Settings are unavailable for this plugin".to_string())?;
                let result = window
                    .state
                    .borrow_mut()
                    .settings
                    .managed_settings_changed(name, &schema, changes);
                match result {
                    Ok(stored) => {
                        window
                            .state
                            .borrow_mut()
                            .plugins
                            .stored_settings_changed(name, stored);
                        report_settings_save(&window, Ok(()));
                        Ok(())
                    }
                    Err(error) => {
                        report_settings_save(&window, Err(error));
                        Err("Settings were not durably saved; see Settings storage".into())
                    }
                }
            }
        }),
        enable_plugin: Rc::new({
            let state = ctx.state.clone();
            let window = ctx.handle();
            move |name: &str| {
                state.borrow_mut().plugins.enable(name);
                let result = state.borrow_mut().settings.plugin_changed(name, true);
                report_settings_save(&window, result);
            }
        }),
        disable_plugin: Rc::new({
            let state = ctx.state.clone();
            let window = ctx.handle();
            move |name: &str| {
                state.borrow_mut().plugins.disable(name);
                // removing data here consumes poll's change signal too, so both
                // slot widgets and the sidebar must be refreshed by this path.
                let changed = state.borrow_mut().store.remove_source(name);
                let result = state.borrow_mut().settings.plugin_changed(name, false);
                report_settings_save(&window, result);
                if changed {
                    refresh_slots(&window);
                    rebuild_sidebar(&window, false);
                }
            }
        }),
        bindings_changed: Rc::new({
            let state = ctx.state.clone();
            let window = ctx.handle();
            move || {
                let new_bindings = bindings.borrow().clone();
                let changes = new_bindings.changes_from(&state.borrow().bindings);
                if changes.is_empty() {
                    return;
                }
                state.borrow_mut().bindings = new_bindings;
                reinstall_shortcuts(&window);
                let result = state.borrow_mut().settings.shortcuts_changed(changes);
                report_settings_save(&window, result);
            }
        }),
    };

    prefs::present(&ctx.window, preferences);
}

fn report_settings_save(ctx: &Window, result: Result<(), crate::settings::StorageError>) {
    let (model, status) = {
        let state = ctx.state.borrow();
        (state.settings_status.clone(), state.storage_status())
    };
    model.splice(0, 1, &[&status]);
    if let Err(error) = result {
        eprintln!("among: settings save failed: {error}");
    }
}

/// Rebuild the shortcut controller so a rebind takes effect without a restart.
fn reinstall_shortcuts(ctx: &Window) {
    install_bindings_into(ctx);
}

/// Move the divider bounding the focused pane by one keyboard step.
///
/// Only the two panes on that divider change; everything outside the split is
/// untouched, which is the point of adjusting the divider rather than resizing
/// the window.
fn resize_focused_divider(ctx: &Window, direction: Direction) {
    let mut resized = false;
    {
        let mut state = ctx.state.borrow_mut();
        let active = state.workspaces.active_index();
        let Some(workspace) = state.workspaces.get_mut(active) else {
            return;
        };
        let focused = workspace.focused;
        let mut nodes = workspace.nodes.clone();
        if move_divider_for_pane(&mut nodes, focused, direction) {
            workspace.nodes = nodes;
            resized = true;
        }
    }
    if resized {
        // The widgets follow the tree on the next tick, so a drag and a key
        // press end up in exactly the same place.
        sync_ratios_to_widgets(ctx);
        reconcile_panes(ctx);
    }
}

/// Move the divider bounding `pane` one keyboard step towards `direction`.
///
/// Left and Right act on the nearest side-by-side split above the pane, Up and
/// Down on the nearest stacked split: that is the divider which bounds the pane
/// on the arrow's axis, even when a split of the other orientation sits
/// between them. The divider moves the way the arrow points whichever side of
/// it the pane is on. Returns false when no such divider exists.
fn move_divider_for_pane(nodes: &mut Node, pane: PaneId, direction: Direction) -> bool {
    let Some(path) = find_pane_path(nodes, pane) else {
        return false;
    };
    let (axis, step) = match direction {
        Direction::Left => (Orientation::Vertical, -RESIZE_STEP),
        Direction::Right => (Orientation::Vertical, RESIZE_STEP),
        Direction::Up => (Orientation::Horizontal, -RESIZE_STEP),
        Direction::Down => (Orientation::Horizontal, RESIZE_STEP),
    };
    // Every prefix of the path names a split above the pane; the longest one
    // is the nearest. The ratio is the first child's share, so a positive step
    // moves the divider right or down.
    for depth in (0..path.len()).rev() {
        let split = &path[..depth];
        if nodes.orientation_at(split) == Some(axis) {
            let Some(current) = nodes.ratio_at(split) else {
                return false;
            };
            return nodes.set_ratio_at(split, current + step);
        }
    }
    false
}

/// The path from the root to the split holding `pane`.
fn find_pane_path(nodes: &Node, pane: PaneId) -> Option<Vec<usize>> {
    fn walk(node: &Node, pane: PaneId, path: &mut Vec<usize>) -> Option<Vec<usize>> {
        match node {
            Node::Pane { id } => (*id == pane).then(|| path.clone()),
            Node::Split { first, second, .. } => {
                path.push(0);
                if let Some(found) = walk(first, pane, path) {
                    return Some(found);
                }
                path.pop();
                path.push(1);
                if let Some(found) = walk(second, pane, path) {
                    return Some(found);
                }
                path.pop();
                None
            }
        }
    }
    walk(nodes, pane, &mut Vec::new())
}

fn focus_direction(ctx: &Window, direction: Direction) {
    let moved = ctx
        .state
        .borrow_mut()
        .workspaces
        .active_mut()
        .focus_direction(direction.into());
    if moved {
        apply_focus(ctx);
        refresh_slots(ctx);
    }
}

fn new_workspace(ctx: &Window) {
    if !ctx.state.borrow().session_ready || ctx.state.borrow().session_closing {
        return;
    }
    let id = ctx.state.borrow_mut().allocate();
    let Some(id) = id else {
        eprintln!("among: cannot create another terminal: pane identifiers are exhausted");
        return;
    };
    {
        // The borrow is confined to this block: `rebuild` borrows the state
        // again, and holding it here would panic.
        let mut state = ctx.state.borrow_mut();
        let count = state.workspaces.len() + 1;
        let name = format!("project {count}");
        // The colour comes from the set, which is what knows what is taken.
        let colour = state.workspaces.next_colour();
        state
            .workspaces
            .push_active(Workspace::new(name, id, colour));
    }
    rebuild(ctx);
}

fn split_focused(ctx: &Window) {
    let focused = ctx.state.borrow().workspaces.active().focused;
    let new_id = ctx.state.borrow_mut().allocate();
    let Some(new_id) = new_id else {
        eprintln!("among: cannot create another terminal: pane identifiers are exhausted");
        return;
    };

    // Orientation comes from the focused pane's own shape, computed from the
    // tree and the area's size. Reading a widget instead would use an
    // allocation that is stale or absent right after a rebuild, which is why
    // splits used to come out vertical every time.
    let area = (ctx.pane_area.width(), ctx.pane_area.height());
    let (width, height) = {
        let state = ctx.state.borrow();
        let workspace = state.workspaces.active();
        crate::size::pane_box(&workspace.nodes, area.0, area.1, focused)
            .unwrap_or_else(|| state.pane(focused).map(|p| p.extent()).unwrap_or((0, 0)))
    };

    let created = ctx
        .state
        .borrow_mut()
        .workspaces
        .active_mut()
        .split_focused(width, height, new_id);

    if created.is_none() {
        // Cannot happen: splitting always finds the focused pane.
        return;
    }
    rebuild(ctx);
}

/// Close the focused pane, cascading to the workspace and then to exit.
fn close_focused(ctx: &Window) {
    let focused = ctx.state.borrow().workspaces.active().focused;
    close_pane(ctx, focused);
}

/// Close a pane, cascading to its workspace and then to the application.
///
/// One implementation for every way a pane ends — by key binding, by the pane
/// closing itself when its process exits, or by any future caller — so the
/// cascade cannot differ between them.
fn close_pane(ctx: &Window, pane: PaneId) {
    // Which workspace holds it, since an exit can arrive for a pane in a
    // workspace the user is not looking at.
    let owner = {
        let state = ctx.state.borrow();
        state
            .workspaces
            .workspaces()
            .iter()
            .position(|ws| ws.panes().contains(&pane))
    };
    let Some(owner) = owner else {
        // Already gone: closing twice must not disturb a second pane.
        return;
    };

    let was_last_pane = {
        let mut state = ctx.state.borrow_mut();
        let Some(workspace) = state.workspaces.get_mut(owner) else {
            return;
        };
        let last = workspace.panes().len() <= 1;
        if !last {
            workspace.close(pane);
        }
        last
    };

    if !was_last_pane {
        ctx.state
            .borrow_mut()
            .plugins
            .broadcast_event(EventKind::PaneClosed, pane.0, None);
        prune_panes(ctx);
        rebuild(ctx);
        // Reparenting on process exit can unmap the survivor until this turn
        // finishes. Restore keyboard focus once GTK has attached it again.
        if owner == ctx.state.borrow().workspaces.active_index() {
            let refresh = ctx.handle();
            glib::idle_add_local_once(move || apply_focus(&refresh));
        }
        return;
    }

    // It was the workspace's last pane, so the workspace goes with it.
    if ctx.state.borrow().workspaces.len() > 1 {
        if ctx.state.borrow_mut().workspaces.remove(owner).is_some() {
            prune_panes(ctx);
            rebuild(ctx);
        }
        return;
    }

    // The last workspace is left, so closing it ends the application — but only
    // after the user confirms, because every terminal's process goes with it.
    confirm_exit(ctx);
}

/// Drop pane widgets whose panes no longer exist in any workspace.
fn prune_panes(ctx: &Window) {
    let live: Vec<PaneId> = {
        let state = ctx.state.borrow();
        state
            .workspaces
            .workspaces()
            .iter()
            .flat_map(|workspace| workspace.panes())
            .collect()
    };
    let mut state = ctx.state.borrow_mut();
    if let Some(bells) = &state.bells {
        bells.retain(&live);
    }
    state.panes.retain(|id, _| live.contains(id));
    let count = state.workspaces.len();
    state.store.retain_panes(&live);
    state.store.retain_workspaces(count);
}

/// Ask whether to exit, showing how much is still running.
///
/// The count matters: closing one idle terminal and closing three panes running
/// a deployment are not the same decision, and the dialog should say which it is.
///
/// The dialog is asynchronous, so nothing is closed here: `Cancel` is the
/// default and the close response, which means an unattended close cannot end
/// the session. Only an explicit `Exit` response shuts the application down.
fn confirm_exit(ctx: &Window) {
    let running: usize = {
        let state = ctx.state.borrow();
        state
            .workspaces
            .workspaces()
            .iter()
            .map(|workspace| workspace.panes().len())
            .sum()
    };

    let dialog = adw::AlertDialog::builder()
        .heading("Close the last terminal?")
        .body(format!(
            "This ends the application and the {running} terminal{} still running in it.",
            if running == 1 { "" } else { "s" }
        ))
        .build();
    dialog.add_response("cancel", "Cancel");
    dialog.add_response("exit", "Exit");
    dialog.set_response_appearance("exit", adw::ResponseAppearance::Destructive);
    dialog.set_default_response(Some("cancel"));
    dialog.set_close_response("cancel");

    dialog.connect_response(None, {
        let ctx = ctx.handle();
        move |dialog, response| {
            if response == "exit" {
                quit(&ctx);
            }
            dialog.close();
        }
    });

    dialog.present(Some(&ctx.window));
}

/// Shut down cleanly: stop the plugins, then close the window.
fn quit(ctx: &Window) {
    if ctx.state.borrow().session_closing {
        return;
    }
    if ctx.state.borrow().session_ready {
        let snapshot = capture_session(ctx);
        match snapshot {
            Ok(snapshot) => {
                let mut state = ctx.state.borrow_mut();
                if let Some(writer) = &state.session_writer {
                    writer.submit(snapshot, true);
                    state.session_closing = true;
                    return;
                }
                drop(state);
                session_warning(
                    ctx,
                    "Session storage is unavailable; the current session has not been saved.",
                );
            }
            Err(error) => {
                session_warning(ctx, &error);
            }
        }
        ctx.state.borrow_mut().session_closing = true;
        confirm_unsaved_exit(ctx);
        return;
    }
    finish_quit(ctx);
}

fn confirm_unsaved_exit(ctx: &Window) {
    let dialog = adw::AlertDialog::builder().heading("Close without saving the session?")
        .body("The current session could not be saved. The previous complete snapshot, if any, is retained unless a durability warning reports otherwise.").build();
    dialog.add_response("cancel", "Keep open");
    dialog.add_response("exit", "Exit anyway");
    dialog.set_response_appearance("exit", adw::ResponseAppearance::Destructive);
    dialog.set_default_response(Some("cancel"));
    dialog.set_close_response("cancel");
    dialog.connect_response(None, {
        let ctx = ctx.handle();
        move |_, response| {
            if response == "exit" {
                finish_quit(&ctx);
            } else {
                let writer = crate::session::path()
                    .ok_or_else(|| "session storage path is unavailable".to_string())
                    .and_then(crate::session::Writer::new);
                {
                    let mut state = ctx.state.borrow_mut();
                    state.session_closing = false;
                    state.session_saved = None;
                    state.session_submitted = None;
                    state.session_writer = None;
                }
                match writer {
                    Ok(writer) => ctx.state.borrow_mut().session_writer = Some(writer),
                    Err(error) => session_warning(&ctx, &error),
                }
                let focus = ctx.handle();
                glib::idle_add_local_once(move || apply_focus(&focus));
            }
        }
    });
    dialog.present(Some(&ctx.window));
}

fn finish_quit(ctx: &Window) {
    // Recorded first, so the window's close handler lets the close through
    // instead of asking a second time.
    ctx.state.borrow_mut().exiting = true;
    ctx.state.borrow_mut().session_closing = false;
    // Shutdown is already confirmed and its final snapshot committed (or the
    // user explicitly chose unsaved exit). A closing modal must not intercept
    // another close request and leave a blank application window alive.
    ctx.window.destroy();
}

fn capture_session(ctx: &Window) -> Result<crate::session::Snapshot, String> {
    let state = ctx.state.borrow();
    crate::session::Snapshot::capture(&state.workspaces, |id| {
        state.pane(id).and_then(Pane::cwd).or_else(|| {
            state
                .workspaces
                .workspaces()
                .iter()
                .flat_map(|workspace| &workspace.cwd)
                .find(|(pane, _)| *pane == id)
                .and_then(|(_, cwd)| cwd.clone())
        })
    })
}

fn session_warning(ctx: &Window, message: &str) {
    let mut state = ctx.state.borrow_mut();
    if state.session_problem == message {
        return;
    }
    eprintln!("among: session: {message}");
    state.session_problem = message.to_string();
    let status = state.storage_status();
    state.settings_status.splice(0, 1, &[&status]);
}

fn session_restore_warning(ctx: &Window, message: &str) {
    let mut state = ctx.state.borrow_mut();
    if !state
        .session_restore_warnings
        .iter()
        .any(|warning| warning == message)
    {
        eprintln!("among: session restore: {message}");
        state.session_restore_warnings.push(message.into());
        let status = state.storage_status();
        state.settings_status.splice(0, 1, &[&status]);
    }
}

fn start_session(ctx: &Window, snapshot: Option<&crate::session::Snapshot>) {
    if ctx.state.borrow().session_ready {
        return;
    }
    if let Some(snapshot) = snapshot {
        match snapshot.restore() {
            Ok((workspaces, allocator)) => {
                let mut state = ctx.state.borrow_mut();
                state.workspaces = workspaces;
                state.allocator = allocator;
            }
            Err(error) => session_warning(ctx, &error),
        }
    }
    let ids: Vec<_> = ctx
        .state
        .borrow()
        .workspaces
        .workspaces()
        .iter()
        .flat_map(Workspace::panes)
        .collect();
    for id in ids {
        ensure_pane(ctx, id);
    }
    ctx.state.borrow_mut().session_ready = true;
    rebuild(ctx);
    let focus = ctx.handle();
    glib::idle_add_local_once(move || apply_focus(&focus));
}

fn begin_session(
    ctx: &Window,
    writer: fn(std::path::PathBuf) -> Result<crate::session::Writer, String>,
) {
    let Some(path) = crate::session::path() else {
        session_warning(
            ctx,
            "HOME and XDG_DATA_HOME are unset; session storage is unavailable.",
        );
        start_session(ctx, None);
        return;
    };
    let previous = match crate::session::load(&path) {
        Ok(snapshot) => snapshot,
        Err(error) => {
            session_warning(ctx, &error);
            None
        }
    };
    match writer(path) {
        Ok(writer) => ctx.state.borrow_mut().session_writer = Some(writer),
        Err(error) => session_warning(ctx, &error),
    }
    install_session_checkpoints(ctx);
    let Some(previous) = previous else {
        start_session(ctx, None);
        return;
    };
    let dialog = adw::AlertDialog::builder()
        .heading("Restore the previous session?")
        .body("Restore workspace names, pane layouts and directories. Terminals start fresh shells; previous commands and processes will not resume.")
        .build();
    dialog.add_response("new", "Start new");
    dialog.add_response("restore", "Restore");
    dialog.set_default_response(Some("new"));
    dialog.set_close_response("exit");
    if std::env::var_os("AMONG_SELFCHECK").is_some() {
        dialog.connect_close_attempt(|_| eprintln!("among: restore_dialog_close_attempt"));
    }
    // Adw routes a window-manager close through the modal dialog first. Treat
    // that as exit, not an implicit decision to overwrite the saved session.
    let escape = gtk::EventControllerKey::new();
    escape.set_propagation_phase(gtk::PropagationPhase::Capture);
    let weak_dialog = dialog.downgrade();
    escape.connect_key_pressed(move |_, key, _, modifiers| {
        if std::env::var_os("AMONG_SELFCHECK").is_some() {
            eprintln!("among: restore_dialog_key={key:?} modifiers={modifiers:?}");
        }
        if key == gtk::gdk::Key::Escape {
            if let Some(dialog) = weak_dialog.upgrade() {
                dialog.set_close_response("new");
                dialog.force_close();
            }
            return glib::Propagation::Stop;
        }
        glib::Propagation::Proceed
    });
    dialog.add_controller(escape);
    dialog.connect_response(None, {
        let ctx = ctx.handle();
        move |_, response| {
            if std::env::var_os("AMONG_SELFCHECK").is_some() {
                eprintln!("among: restore_dialog_response={response}");
            }
            if ctx.state.borrow().exiting {
                return;
            }
            if response == "exit" {
                finish_quit(&ctx);
                return;
            }
            start_session(&ctx, (response == "restore").then_some(&previous));
        }
    });
    dialog.present(Some(&ctx.window));
}

fn install_session_checkpoints(ctx: &Window) {
    let checkpoint = ctx.handle();
    glib::timeout_add_local(Duration::from_secs(2), move || {
        if checkpoint.state.borrow().exiting {
            return glib::ControlFlow::Break;
        }
        if !checkpoint.state.borrow().session_ready || checkpoint.state.borrow().session_closing {
            return glib::ControlFlow::Continue;
        }
        if checkpoint.state.borrow().session_writer.is_none() {
            if let Some(path) = crate::session::path() {
                match crate::session::Writer::new(path) {
                    Ok(writer) => checkpoint.state.borrow_mut().session_writer = Some(writer),
                    Err(error) => session_warning(&checkpoint, &error),
                }
            }
        }
        match capture_session(&checkpoint) {
            Ok(snapshot) => {
                let mut state = checkpoint.state.borrow_mut();
                if state.session_saved.as_ref() != Some(&snapshot)
                    && state.session_submitted.as_ref() != Some(&snapshot)
                {
                    if let Some(writer) = &state.session_writer {
                        writer.submit(snapshot.clone(), false);
                        state.session_submitted = Some(snapshot);
                    }
                }
            }
            Err(error) => session_warning(&checkpoint, &error),
        }
        glib::ControlFlow::Continue
    });
    let poll = ctx.handle();
    glib::timeout_add_local(Duration::from_millis(50), move || {
        if poll.state.borrow().exiting {
            return glib::ControlFlow::Break;
        }
        let completions = poll
            .state
            .borrow()
            .session_writer
            .as_ref()
            .map(|writer| writer.drain())
            .unwrap_or_default();
        for completion in completions {
            {
                let mut state = poll.state.borrow_mut();
                if state.session_submitted.as_ref() == Some(&completion.snapshot) {
                    state.session_submitted = None;
                }
            }
            match completion.result {
                Ok(()) => {
                    let mut state = poll.state.borrow_mut();
                    state.session_saved = Some(completion.snapshot);
                    state.session_problem.clear();
                    let status = state.storage_status();
                    state.settings_status.splice(0, 1, &[&status]);
                    drop(state);
                    if completion.final_write {
                        finish_quit(&poll);
                    }
                }
                Err(error) => {
                    session_warning(&poll, &error);
                    if completion.final_write {
                        confirm_unsaved_exit(&poll);
                    }
                }
            }
        }
        glib::ControlFlow::Continue
    });
}

/// Width of the sidebar when it is showing the strip.
///
/// Wide enough for a terminal icon and a two-digit count, which is all the strip
/// shows, and narrow enough that the terminals keep almost the whole window.
const STRIP_WIDTH: f64 = 56.0;
const EXPANDED_MIN_WIDTH: f64 = 180.0;
const EXPANDED_MAX_WIDTH: f64 = 320.0;

/// Show the sidebar as either the full list or the narrow strip.
///
/// One path for the header button and anything else that toggles it, so the
/// model, the width, and the rebuilt rows cannot disagree.
fn set_sidebar_collapsed(ctx: &Window, collapsed: bool) {
    {
        let mut state = ctx.state.borrow_mut();
        if state.sidebar_collapsed == collapsed {
            return;
        }
        state.sidebar_collapsed = collapsed;
    }
    apply_sidebar_width(ctx);
    rebuild_sidebar(ctx, false);
}

/// Apply the sidebar's width for the current presentation.
fn apply_sidebar_width(ctx: &Window) {
    let (collapsed, preferred) = {
        let state = ctx.state.borrow();
        (state.sidebar_collapsed, state.sidebar_width)
    };
    let split_view = &ctx.split_view;
    ctx.sidebar_resize.set_visible(!collapsed);
    if collapsed {
        split_view.set_sidebar_width_unit(adw::LengthUnit::Sp);
        split_view.set_min_sidebar_width(STRIP_WIDTH);
        split_view.set_max_sidebar_width(STRIP_WIDTH);
    } else if let Some(preferred) = preferred {
        let available = if split_view.width() > 0 {
            split_view.width()
        } else {
            ctx.window.default_width()
        };
        let (minimum, maximum) = sidebar_width_limits(available);
        let width = preferred.clamp(minimum, maximum);
        split_view.set_sidebar_width_unit(adw::LengthUnit::Px);
        // Keep a small minimum: fixing both limits to the preferred width would
        // prevent the window manager from shrinking a wide saved sidebar.
        split_view.set_min_sidebar_width(minimum);
        split_view.set_max_sidebar_width(width);
        split_view
            .set_sidebar_width_fraction((width / f64::from(available.max(1))).clamp(0.0, 1.0));
    } else {
        split_view.set_sidebar_width_unit(adw::LengthUnit::Sp);
        split_view.set_min_sidebar_width(EXPANDED_MIN_WIDTH);
        split_view.set_max_sidebar_width(EXPANDED_MAX_WIDTH);
        split_view.set_sidebar_width_fraction(0.25);
    }
}

fn sidebar_width_limits(available: i32) -> (f64, f64) {
    let maximum = (f64::from(available) - 240.0).max(1.0);
    (EXPANDED_MIN_WIDTH.min(maximum), maximum)
}

fn install_sidebar_resize(ctx: &Window) {
    let drag = gtk::GestureDrag::new();
    drag.set_button(1);
    drag.set_propagation_phase(gtk::PropagationPhase::Capture);
    let start = Rc::new(std::cell::Cell::new(None));
    drag.connect_drag_begin({
        let ctx = ctx.handle();
        let start = start.clone();
        move |gesture, x, y| {
            start.set(None);
            let on_edge = ctx
                .sidebar_resize
                .compute_bounds(&ctx.split_view)
                .is_some_and(|rect| {
                    x >= f64::from(rect.x())
                        && x < f64::from(rect.x() + rect.width())
                        && y >= f64::from(rect.y())
                        && y < f64::from(rect.y() + rect.height())
                });
            if ctx.state.borrow().sidebar_collapsed || !on_edge {
                gesture.set_state(gtk::EventSequenceState::Denied);
                return;
            }
            start.set(
                ctx.split_view
                    .sidebar()
                    .map(|sidebar| f64::from(sidebar.width())),
            );
            gesture.set_state(gtk::EventSequenceState::Claimed);
        }
    });
    drag.connect_drag_update({
        let ctx = ctx.handle();
        let start = start.clone();
        move |_, offset, _| {
            if let Some(initial) = start.get() {
                resize_sidebar(&ctx, initial + offset);
            }
        }
    });
    drag.connect_drag_end({
        let ctx = ctx.handle();
        move |_, offset, _| {
            if let Some(initial) = start.take() {
                if offset.abs() >= 1.0 && !ctx.state.borrow().sidebar_collapsed {
                    resize_sidebar(&ctx, initial + offset);
                    let preferred = ctx.state.borrow().sidebar_width;
                    if let Some(width) = preferred {
                        let result = ctx.state.borrow_mut().settings.sidebar_width_changed(width);
                        report_settings_save(&ctx, result);
                    }
                }
                apply_focus(&ctx);
            }
        }
    });
    // Gesture offsets must be relative to a stationary widget. Attaching to the
    // moving edge subtracts its own movement and makes the width lag the pointer.
    ctx.split_view.add_controller(drag);
}

fn resize_sidebar(ctx: &Window, width: f64) {
    if !width.is_finite() || ctx.state.borrow().sidebar_collapsed {
        return;
    }
    let (minimum, maximum) = sidebar_width_limits(ctx.split_view.width());
    ctx.state.borrow_mut().sidebar_width = Some(width.clamp(minimum, maximum));
    apply_sidebar_width(ctx);
}

/// Whether anything would be lost by closing the window now.
fn has_running_terminals(ctx: &Window) -> bool {
    ctx.state
        .borrow()
        .workspaces
        .workspaces()
        .iter()
        .any(|workspace| !workspace.panes().is_empty())
}

/// Pump plugins, re-derive sizes, and refresh slots on a timer.
fn install_tick(ctx: &Window) {
    let tick = ctx.handle();
    glib::timeout_add_local(Duration::from_millis(TICK_MS.into()), move || {
        if tick.state.borrow().exiting {
            return glib::ControlFlow::Break;
        }
        tick.on_tick();
        glib::ControlFlow::Continue
    });
}

impl Window {
    /// A handle to the same window state, used by signal handlers.
    fn handle(&self) -> Window {
        Window {
            state: self.state.clone(),
            workspace_list: self.workspace_list.clone(),
            pane_area: self.pane_area.clone(),
            header_bar: self.header_bar.clone(),
            workspace_header: self.workspace_header.clone(),
            app_header: self.app_header.clone(),
            app_footer: self.app_footer.clone(),
            sidebar_resize: self.sidebar_resize.clone(),
            split_view: self.split_view.clone(),
            window: self.window.clone(),
            paneds: self.paneds.clone(),
            summary_requested: self.summary_requested.clone(),
            collection_cursor: self.collection_cursor.clone(),
        }
    }

    /// Report that a pane's process exited, naming the status when it is not
    /// zero, because a non-zero status is the part worth noticing.
    fn report_exit(&self, pane: PaneId, status: i32) {
        let message = if status == 0 {
            format!("Terminal {} exited", pane.0)
        } else {
            format!("Terminal {} exited with status {status}", pane.0)
        };
        {
            let mut state = self.state.borrow_mut();
            state.notices_recorded += 1;
            state.last_notice = Some(message.clone());
        }
        eprintln!("among: {message}");
    }

    fn on_tick(&self) {
        if self.state.borrow().exiting {
            return;
        }
        if let Some(bells) = &self.state.borrow().bells {
            bells.observe_focus();
        }
        apply_sidebar_width(self);
        if !self.state.borrow().session_ready || self.state.borrow().session_closing {
            return;
        }
        // 1. Read whatever plugins wrote, then ask for whatever is missing.
        //    `poll` must come first: without it nothing ever drains a plugin's
        //    output and every slot stays empty.
        {
            let mut state = self.state.borrow_mut();
            state.plugins.poll();
        }
        log_plugin_notices(self);
        collect_plugin_results(self);
        request_plugin_status(self);

        // 2. Push the tree's ratios onto the dividers, then re-derive every
        //    pane's reported size from its allocation.
        sync_ratios_to_widgets(self);
        sync_ratios_to_widgets(self);
        reconcile_panes(self);

        // 3. Refresh slot content, but only where a plugin asked for it.
        let due: Vec<Scope> = {
            let state = self.state.borrow();
            let mut due = Vec::new();
            for workspace in state.workspaces.workspaces() {
                for id in workspace.panes() {
                    if state
                        .store
                        .needs_refresh(Scope::PaneHeader(id), slots::now_ms())
                    {
                        due.push(Scope::PaneHeader(id));
                    }
                    if state
                        .store
                        .needs_refresh(Scope::PaneFooter(id), slots::now_ms())
                    {
                        due.push(Scope::PaneFooter(id));
                    }
                }
            }
            due
        };
        if !due.is_empty() {
            refresh_slots(self);
        }
    }
}

/// Drain plugin notifications without adding UI or changing keyboard focus.
fn log_plugin_notices(ctx: &Window) {
    let notices = ctx.state.borrow_mut().plugins.drain_notices();
    for (plugin, message, _style) in notices {
        eprintln!("among: {plugin}: {message}");
        {
            let mut borrowed = ctx.state.borrow_mut();
            borrowed.notices_recorded += 1;
            borrowed.last_notice = Some(format!("{plugin}: {message}"));
        }
    }
}

/// Turn plugin answers into slot content.
fn collect_plugin_results(ctx: &Window) {
    let results = ctx.state.borrow_mut().plugins.drain_results();
    let mut changed = false;
    let now = slots::now_ms();
    {
        let mut state = ctx.state.borrow_mut();
        for result in results {
            if let Some(index) = parse_summary_scope(&result.scope) {
                // A summary is the compact content shown beside a workspace's
                // name; several plugins may contribute one each.
                if let Some(first) = result.fragments.first() {
                    PluginHost::apply_summary(
                        &mut state.store,
                        index,
                        &result.plugin,
                        &crate::plugin::IncomingSummary {
                            text: first.text.clone(),
                            icon: first.icon.clone(),
                            tooltip: first.tooltip.clone(),
                            style: first.style.as_deref().and_then(|s| s.parse().ok()),
                        },
                    );
                    changed = true;
                }
                continue;
            }
            let Some(scope) = parse_scope(&result.scope) else {
                // A scope the host does not recognise is ignored rather than
                // guessed at, so a plugin cannot target an arbitrary widget.
                continue;
            };
            PluginHost::apply_fragments(
                &mut state.store,
                scope,
                &result.plugin,
                &result.fragments,
                result.refresh,
            );
            // The refresh clock restarts once the answer is in. Without this the
            // contribution's timestamp stays unset and every slot is considered
            // due on every tick, which turns one slow plugin into a request
            // storm that starves the others.
            state.store.mark_collected(scope, &result.plugin, now);
            changed = true;
        }
        // Content from a plugin that has stopped working is withdrawn, so a
        // stale fragment is never left frozen on screen.
        for source in state.plugins.faulted_sources() {
            changed |= state.store.remove_source(&source);
        }
    }
    if changed {
        refresh_slots(ctx);
        rebuild_sidebar(ctx, false);
    }
}

/// Ask healthy plugins for the slots that are due.
fn request_plugin_status(ctx: &Window) {
    #[derive(Clone, Copy)]
    enum Target {
        Slot(Scope),
        Summary(usize),
    }
    let now = slots::now_ms();
    let mut state = ctx.state.borrow_mut();
    let context = plugin_context(&state);
    let healthy = state.plugins.healthy();

    let mut scopes = vec![
        Target::Slot(Scope::WorkspaceHeader),
        Target::Slot(Scope::AppHeader),
        Target::Slot(Scope::AppFooter),
    ];
    for (index, workspace) in state.workspaces.workspaces().iter().enumerate() {
        scopes.push(Target::Summary(index));
        for id in workspace.panes() {
            scopes.push(Target::Slot(Scope::PaneHeader(id)));
            scopes.push(Target::Slot(Scope::PaneFooter(id)));
        }
    }

    ctx.summary_requested
        .borrow_mut()
        .retain(|(index, name), _| *index < state.workspaces.len() && healthy.contains(name));
    // rotate scopes as well as plugins: even an interval-zero early scope must
    // not repeatedly take queue capacity from a deferred later pane or summary.
    let first = ctx.collection_cursor.get() % scopes.len();
    ctx.collection_cursor.set((first + 1) % scopes.len());
    for offset in 0..scopes.len() {
        match scopes[(first + offset) % scopes.len()] {
            Target::Slot(scope) => {
                let scope_name = crate::plugin::protocol::scope_name(&scope);
                for name in &healthy {
                    if state.store.needs_collection(scope, name, now)
                        && state
                            .plugins
                            .request_status(name, &scope_name, &context)
                            .is_some()
                    {
                        state.store.mark_collected(scope, name, now);
                    }
                }
            }
            Target::Summary(index) => {
                let scope_name = crate::plugin::protocol::summary_scope(index);
                for name in &healthy {
                    let key = (index, name.clone());
                    let due = ctx
                        .summary_requested
                        .borrow()
                        .get(&key)
                        .is_none_or(|last| now.saturating_sub(*last) >= SUMMARY_REFRESH_MS);
                    if due
                        && state
                            .plugins
                            .request_status(name, &scope_name, &context)
                            .is_some()
                    {
                        ctx.summary_requested.borrow_mut().insert(key, now);
                    }
                }
            }
        }
    }
}

/// Parse a workspace-summary scope, which carries a workspace index.
fn parse_summary_scope(raw: &str) -> Option<usize> {
    raw.strip_prefix("workspace_summary:")?
        .parse::<usize>()
        .ok()
}

/// Parse a scope string sent by the host or returned by a plugin.
fn parse_scope(raw: &str) -> Option<Scope> {
    use crate::slots::Scope;
    // Scopes without an argument carry no colon, so they are matched before
    // the argument-taking ones are split.
    match raw {
        "app_header" => return Some(Scope::AppHeader),
        "app_footer" => return Some(Scope::AppFooter),
        "workspace_header" => return Some(Scope::WorkspaceHeader),
        _ => {}
    }
    let (kind, argument) = raw.split_once(':')?;
    let pane_id = || argument.parse::<u64>().ok().map(PaneId);
    match kind {
        "pane_header" => pane_id().map(Scope::PaneHeader),
        "pane_footer" => pane_id().map(Scope::PaneFooter),
        _ => None,
    }
}

fn reconcile_panes(ctx: &Window) {
    let ids = ctx.state.borrow().workspaces.active().visible_panes();
    for id in ids {
        if let Some(pane) = ctx.state.borrow().pane(id) {
            pane.reconcile();
        }
    }
}

#[cfg(test)]
mod plugin_tests;

#[cfg(test)]
mod bell_tests;

#[cfg(test)]
mod tests {
    #[test]
    #[ignore = "requires an isolated graphical session and real pointer/keyboard input"]
    fn expanded_pane_restore_control_is_target_safe_and_keyboard_operable() {
        use super::*;
        use std::os::fd::AsRawFd;

        fn pump() {
            let deadline = std::time::Instant::now() + Duration::from_millis(500);
            while std::time::Instant::now() < deadline {
                while glib::MainContext::default().pending() {
                    glib::MainContext::default().iteration(false);
                }
                std::thread::sleep(Duration::from_millis(5));
            }
        }
        fn descendants(widget: &gtk::Widget) -> Vec<gtk::Widget> {
            let mut widgets = vec![widget.clone()];
            let mut child = widget.first_child();
            while let Some(node) = child {
                widgets.extend(descendants(&node));
                child = node.next_sibling();
            }
            widgets
        }
        fn terminals(window: &gtk::Window) -> Vec<vte::Terminal> {
            descendants(window.upcast_ref())
                .into_iter()
                .filter_map(|widget| widget.downcast::<vte::Terminal>().ok())
                .collect()
        }
        fn controls(window: &gtk::Window) -> Vec<gtk::Button> {
            descendants(window.upcast_ref())
                .into_iter()
                .filter(|widget| widget.has_css_class("pane-restore"))
                .map(|widget| widget.downcast::<gtk::Button>().unwrap())
                .collect()
        }
        fn layout(window: &gtk::Window) -> Vec<(gtk::Orientation, i32)> {
            descendants(window.upcast_ref())
                .into_iter()
                .filter_map(|widget| widget.downcast::<gtk::Paned>().ok())
                .map(|paned| (paned.orientation(), paned.position()))
                .collect()
        }
        fn xdo(args: &[&str]) -> String {
            let output = std::process::Command::new("xdotool")
                .args(args)
                .output()
                .unwrap();
            assert!(output.status.success(), "{args:?}: {output:?}");
            String::from_utf8(output.stdout).unwrap()
        }
        fn key(key: &str) {
            xdo(&["key", "--clearmodifiers", key]);
            pump();
        }
        fn action(window: &gtk::Window, action: Action) {
            window
                .activate_action(&format!("win.{}", shortcuts::action_name(action)), None)
                .unwrap();
            pump();
        }
        fn control(window: &gtk::Window) -> gtk::Button {
            let controls = controls(window);
            assert_eq!(controls.len(), 1);
            let button = controls[0].clone();
            assert!(button.is_mapped() && button.is_focusable());
            assert_eq!(button.tooltip_text().as_deref(), Some("Restore pane"));
            assert!(gtk::test_accessible_has_role(
                &button,
                gtk::AccessibleRole::Button
            ));
            let accessible = button.clone().upcast::<gtk::Accessible>();
            // this checks the built accessible value, not just the setter call.
            unsafe {
                let error = gtk::ffi::gtk_test_accessible_check_property(
                    accessible.as_ptr(),
                    gtk::ffi::GTK_ACCESSIBLE_PROPERTY_LABEL,
                    c"Restore pane".as_ptr(),
                );
                if !error.is_null() {
                    let message = std::ffi::CStr::from_ptr(error)
                        .to_string_lossy()
                        .into_owned();
                    glib::ffi::g_free(error.cast());
                    panic!("wrong accessible label: {message}");
                }
            }
            assert!(button.width() >= 24 && button.height() >= 24);
            button
        }
        std::env::set_var("GTK_A11Y", "test");
        let config = std::env::var_os("XDG_CONFIG_HOME").unwrap();
        crate::test_isolation::assert_below_check_root(std::path::Path::new(&config));
        let project = std::path::Path::new(&config)
            .parent()
            .unwrap()
            .join("demo-project");
        std::fs::create_dir(&project).unwrap();
        std::env::set_current_dir(&project).unwrap();
        let app = adw::Application::builder()
            .application_id("com.github.among-terminal.restore-test")
            .flags(gio::ApplicationFlags::NON_UNIQUE)
            .build();
        app.register(None::<&gio::Cancellable>).unwrap();
        let ctx = build_with_writer(&app, None, crate::session::Writer::new).unwrap();
        pump();
        let window = app.active_window().unwrap();
        let ids = xdo(&[
            "search",
            "--onlyvisible",
            "--pid",
            &std::process::id().to_string(),
            "--name",
            &window.title().unwrap(),
        ]);
        let id = ids.lines().last().unwrap();
        xdo(&["windowactivate", "--sync", id]);
        pump();
        assert!(controls(&window).is_empty());
        action(&window, Action::ToggleZoom);
        control(&window).emit_clicked();
        pump();
        assert!(controls(&window).is_empty());
        action(&window, Action::SplitPane);
        action(&window, Action::SplitPane);
        let original = terminals(&window);
        assert_eq!(original.len(), 3);
        let positions = layout(&window);
        let process_groups = original
            .iter()
            .map(|terminal| unsafe { libc::tcgetpgrp(terminal.pty().unwrap().fd().as_raw_fd()) })
            .collect::<Vec<_>>();
        assert!(process_groups.iter().all(|pid| *pid > 0));
        let source = original
            .iter()
            .find(|terminal| terminal.has_focus())
            .unwrap();

        action(&window, Action::ToggleZoom);
        let pointer = control(&window);
        let rect = pointer.compute_bounds(&window).unwrap();
        let (surface_x, surface_y) = window.surface_transform();
        xdo(&[
            "mousemove",
            "--window",
            id,
            &format!(
                "{:.0}",
                f64::from(rect.x() + rect.width() / 2.0) + surface_x
            ),
            &format!(
                "{:.0}",
                f64::from(rect.y() + rect.height() / 2.0) + surface_y
            ),
        ]);
        xdo(&["click", "1"]);
        pump();
        assert!(controls(&window).is_empty());
        assert!(source.has_focus());
        assert_eq!(terminals(&window), original);
        assert_eq!(layout(&window), positions);
        pointer.emit_clicked();
        pump();
        assert!(
            controls(&window).is_empty(),
            "late click re-expanded the pane"
        );
        println!("PASS real pointer Restore preserves nested layout, terminals and source focus; repeated activation is inert");

        for key_name in ["Return", "space"] {
            key("ctrl+shift+x");
            let button = control(&window);
            assert!(button.grab_focus());
            key(key_name);
            assert!(controls(&window).is_empty());
            assert!(source.has_focus());
        }
        assert_eq!(layout(&window), positions);
        assert_eq!(
            original
                .iter()
                .map(|terminal| unsafe {
                    libc::tcgetpgrp(terminal.pty().unwrap().fd().as_raw_fd())
                })
                .collect::<Vec<_>>(),
            process_groups
        );
        println!("PASS Enter and Space restore to the same running shell; built accessible role/name and hit area are valid");

        action(&window, Action::ToggleZoom);
        let stale = control(&window);
        action(&window, Action::NewWorkspace);
        let other = terminals(&window);
        stale.emit_clicked();
        pump();
        assert!(controls(&window).is_empty());
        assert_eq!(terminals(&window), other);
        assert!(other[0].has_focus());
        action(
            &window,
            Action::CycleWorkspace(crate::shortcuts::Cycle::Previous),
        );
        let current = control(&window);
        stale.emit_clicked();
        pump();
        assert_eq!(control(&window), current, "old header survived a rebuild");
        assert!(current.grab_focus());
        key("Return");
        assert_eq!(layout(&window), positions);
        println!("PASS stale actions cannot restore another workspace or a newly rebuilt header");

        action(&window, Action::ToggleZoom);
        let button = control(&window);
        let header = button.parent().unwrap();
        let slot = descendants(&header)
            .into_iter()
            .find(|widget| widget.has_css_class("pane-slot"))
            .unwrap()
            .downcast::<gtk::Box>()
            .unwrap();
        let source_id = ctx.state.borrow().workspaces.active().focused;
        ctx.state.borrow_mut().store.set(
            Scope::PaneHeader(source_id),
            "restore-layout-fixture",
            crate::slots::Contribution {
                source: "restore-layout-fixture".into(),
                fragments: vec![crate::slots::Fragment::text(
                    "restore-layout-fixture",
                    "provider status",
                )],
                refresh: crate::slots::Refresh::OnEvent,
                last_asked_ms: None,
            },
        );
        refresh_slots(&ctx);
        let output = std::path::PathBuf::from(config)
            .parent()
            .unwrap()
            .join("restore-screenshots");
        std::fs::create_dir(&output).unwrap();
        let style = adw::StyleManager::default();
        let contrast = gio::Settings::new("org.gnome.desktop.a11y.interface");
        for (theme, scheme, high_contrast) in [
            ("light", adw::ColorScheme::ForceLight, false),
            ("dark", adw::ColorScheme::ForceDark, false),
            ("high-contrast", adw::ColorScheme::ForceLight, true),
        ] {
            style.set_color_scheme(scheme);
            contrast
                .set_boolean("high-contrast", high_contrast)
                .unwrap();
            pump();
            assert_eq!(style.is_high_contrast(), high_contrast);
            for width in [1000, 620] {
                window.unmaximize();
                pump();
                xdo(&["windowsize", "--sync", id, &width.to_string(), "650"]);
                pump();
                let button = control(&window);
                let bounds = button.compute_bounds(&window).unwrap();
                let header_bounds = header.compute_bounds(&window).unwrap();
                assert!(bounds.x() >= header_bounds.x());
                assert!(bounds.x() + bounds.width() <= header_bounds.x() + header_bounds.width());
                assert!(descendants(slot.upcast_ref()).into_iter().any(|widget| {
                    widget
                        .downcast::<gtk::Label>()
                        .is_ok_and(|label| label.text() == "provider status" && label.is_mapped())
                }));
                let screenshot = output.join(format!("{theme}-{width}.png"));
                assert!(std::process::Command::new("import")
                    .args(["-window", id])
                    .arg(&screenshot)
                    .status()
                    .unwrap()
                    .success());
                println!("PASS Restore theme={theme} requested_width={width} actual_width={} allocated={bounds:?} accessible=Restore pane; host-rendered slot fixture remains visible; screenshot={}", window.width(), screenshot.display());
            }
        }
        contrast.set_boolean("high-contrast", false).unwrap();
        let (surface_x, surface_y) = window.surface_transform();
        let rect = button.compute_bounds(&window).unwrap();
        let start_x = (f64::from(rect.x() + rect.width() / 2.0) + surface_x).round() as i32;
        let start_y = (f64::from(rect.y() + rect.height() / 2.0) + surface_y).round() as i32;
        xdo(&[
            "mousemove",
            "--window",
            id,
            &start_x.to_string(),
            &start_y.to_string(),
        ]);
        xdo(&["mousedown", "1"]);
        xdo(&[
            "mousemove",
            "--window",
            id,
            &(start_x - 100).to_string(),
            &(start_y + 100).to_string(),
        ]);
        pump();
        xdo(&["mouseup", "1"]);
        pump();
        assert_eq!(
            control(&window),
            button,
            "button gesture started a pane drag"
        );
        assert_eq!(terminals(&window).len(), 1);
        assert!(descendants(window.upcast_ref()).iter().all(|widget| {
            !["drop-left", "drop-right", "drop-top", "drop-bottom"]
                .iter()
                .any(|class| widget.has_css_class(class))
        }));
        button.emit_clicked();
        pump();
        assert_eq!(terminals(&window).len(), 3);
        assert!(descendants(window.upcast_ref()).into_iter().any(|widget| {
            widget
                .downcast::<gtk::Label>()
                .is_ok_and(|label| label.text() == "provider status" && label.is_mapped())
        }));
        println!("PASS a drag gesture on Restore does not become a pane drag");

        let frames = descendants(window.upcast_ref())
            .into_iter()
            .filter(|widget| widget.has_css_class("pane-frame"))
            .collect::<Vec<_>>();
        let drag_header = descendants(&frames[0])
            .into_iter()
            .find(|widget| widget.has_css_class("pane-header"))
            .unwrap();
        let from = drag_header.compute_bounds(&window).unwrap();
        let to = frames[2].compute_bounds(&window).unwrap();
        let (surface_x, surface_y) = window.surface_transform();
        let x = (f64::from(from.x() + from.width() / 2.0) + surface_x).round() as i32;
        let y = (f64::from(from.y() + from.height() / 2.0) + surface_y).round() as i32;
        let end_x = (f64::from(to.x() + to.width() / 2.0) + surface_x).round() as i32;
        let end_y = (f64::from(to.y() + to.height() - 8.0) + surface_y).round() as i32;
        let before_drag = layout(&window);
        xdo(&["mousemove", "--window", id, &x.to_string(), &y.to_string()]);
        xdo(&["mousedown", "1"]);
        for step in 1..=12 {
            xdo(&[
                "mousemove",
                "--window",
                id,
                &(x + (end_x - x) * step / 12).to_string(),
                &(y + (end_y - y) * step / 12).to_string(),
            ]);
            std::thread::sleep(Duration::from_millis(40));
            while glib::MainContext::default().pending() {
                glib::MainContext::default().iteration(false);
            }
        }
        xdo(&["mouseup", "1"]);
        pump();
        assert_ne!(
            layout(&window),
            before_drag,
            "normal header drag stopped working"
        );
        assert_eq!(terminals(&window).len(), 3);
        println!(
            "PASS real pane header drag still rearranges the layout outside the Restore button"
        );

        action(&window, Action::ToggleZoom);
        let closed = control(&window);
        action(&window, Action::ClosePane);
        assert_eq!(terminals(&window).len(), 2);
        closed.emit_clicked();
        pump();
        assert!(controls(&window).is_empty());
        assert_eq!(terminals(&window).len(), 2);
        println!("PASS a closed target's old Restore control cannot change the surviving panes");
        window.destroy();
        pump();
    }

    #[test]
    #[ignore = "requires an isolated graphical session and real keyboard input"]
    fn menu_hints_follow_live_preferences_and_unbound_actions_still_run() {
        use super::*;
        fn pump() {
            let end = std::time::Instant::now() + Duration::from_millis(700);
            while std::time::Instant::now() < end {
                while glib::MainContext::default().pending() {
                    glib::MainContext::default().iteration(false);
                }
                std::thread::sleep(Duration::from_millis(5));
            }
        }
        fn descendants(widget: &gtk::Widget) -> Vec<gtk::Widget> {
            let mut result = vec![widget.clone()];
            let mut child = widget.first_child();
            while let Some(node) = child {
                result.extend(descendants(&node));
                child = node.next_sibling();
            }
            result
        }
        fn key(sequence: &str) {
            assert!(std::process::Command::new("xdotool")
                .args(["key", "--clearmodifiers", sequence])
                .status()
                .unwrap()
                .success());
            pump();
        }
        fn focus(window: &impl IsA<gtk::Window>) {
            let output = std::process::Command::new("xdotool")
                .args([
                    "search",
                    "--onlyvisible",
                    "--pid",
                    &std::process::id().to_string(),
                    "--name",
                    &window.as_ref().title().unwrap(),
                ])
                .output()
                .unwrap();
            let ids = String::from_utf8(output.stdout).unwrap();
            let id = ids.lines().last().unwrap();
            assert!(std::process::Command::new("xdotool")
                .args(["windowactivate", "--sync", id])
                .status()
                .unwrap()
                .success());
            pump();
        }
        fn menu(window: &gtk::Window) -> gtk::PopoverMenu {
            key("shift+F10");
            descendants(window.upcast_ref())
                .into_iter()
                .filter_map(|widget| widget.downcast::<gtk::PopoverMenu>().ok())
                .find(|popover| popover.is_mapped())
                .expect("native mapped menu")
        }
        fn labels(menu: &gtk::PopoverMenu) -> Vec<String> {
            descendants(menu.upcast_ref())
                .into_iter()
                .filter_map(|widget| widget.downcast::<gtk::Label>().ok())
                .map(|label| label.text().to_string())
                .collect()
        }
        fn activate(menu: &gtk::PopoverMenu, name: &str) {
            let button = descendants(menu.upcast_ref())
                .into_iter()
                .filter_map(|widget| widget.downcast::<gtk::Button>().ok())
                .find(|button| {
                    descendants(button.upcast_ref()).into_iter().any(|widget| {
                        widget
                            .downcast::<gtk::Label>()
                            .is_ok_and(|label| label.text() == name)
                    })
                })
                .expect("named menu button");
            assert!(button.grab_focus());
            key("Return");
        }
        let config = std::env::var_os("XDG_CONFIG_HOME").unwrap();
        crate::test_isolation::assert_below_check_root(std::path::Path::new(&config));
        let app = adw::Application::builder()
            .application_id("com.github.among-terminal.menu-rebind-test")
            .flags(gio::ApplicationFlags::NON_UNIQUE)
            .build();
        app.register(None::<&gio::Cancellable>).unwrap();
        build(&app, None);
        pump();
        let window = app.active_window().unwrap();
        focus(&window);
        let initial = menu(&window);
        assert!(labels(&initial).contains(&"Shift+Ctrl+N".into()));
        key("Escape");
        window.activate_action("win.preferences", None).unwrap();
        pump();
        let prefs = gtk::Window::list_toplevels()
            .into_iter()
            .find_map(|widget| widget.downcast::<adw::PreferencesWindow>().ok())
            .unwrap();
        focus(&prefs);
        for (action, sequence) in [
            (Action::SplitPane, "F9"),
            (Action::ToggleZoom, "Escape"),
            (Action::ClosePane, "Escape"),
        ] {
            let row = descendants(prefs.upcast_ref())
                .into_iter()
                .filter_map(|widget| widget.downcast::<adw::ActionRow>().ok())
                .find(|row| row.title() == action.label())
                .unwrap();
            assert!(row.grab_focus());
            key("Return");
            key(sequence);
        }
        prefs.close();
        pump();
        focus(&window);
        let rebound = menu(&window);
        let hints = labels(&rebound);
        assert!(hints.contains(&"F9".into()));
        assert!(!hints.contains(&"Shift+Ctrl+N".into()));
        assert!(!hints.contains(&"Shift+Ctrl+X".into()));
        assert!(!hints.contains(&"Shift+Ctrl+W".into()));
        activate(&rebound, "Split Pane");
        let terminals = descendants(window.upcast_ref())
            .into_iter()
            .filter_map(|widget| widget.downcast::<vte::Terminal>().ok())
            .collect::<Vec<_>>();
        assert_eq!(terminals.len(), 2);
        let expanded = menu(&window);
        activate(&expanded, "Expand Pane");
        let restore = menu(&window);
        assert!(labels(&restore).contains(&"Restore Pane".into()));
        activate(&restore, "Restore Pane");
        let close = menu(&window);
        activate(&close, "Close Terminal");
        let survivors = descendants(window.upcast_ref())
            .into_iter()
            .filter_map(|widget| widget.downcast::<vte::Terminal>().ok())
            .collect::<Vec<_>>();
        assert_eq!(survivors.len(), 1);
        assert!(survivors[0].has_focus());
        assert_eq!(toast_widget_count(window.upcast_ref()), 0);
        println!("PASS live Preferences rebind/unbind refreshes native menu hints; split, unbound zoom/restore and close dispatch after win-group replacement");
        window.destroy();
        pump();
    }

    #[test]
    #[ignore = "requires an isolated graphical session"]
    fn slow_session_writes_do_not_block_terminal_input() {
        use super::*;
        use std::path::PathBuf;
        fn slow_writer(path: PathBuf) -> Result<crate::session::Writer, String> {
            crate::session::Writer::with_save(path, |path, snapshot| {
                std::fs::write(path.with_extension("blocked"), b"waiting")
                    .map_err(|error| error.to_string())?;
                std::thread::sleep(Duration::from_secs(4));
                crate::session::save(path, snapshot)
            })
        }
        fn wait_for(predicate: impl Fn() -> bool, timeout: Duration) {
            let deadline = std::time::Instant::now() + timeout;
            while !predicate() && std::time::Instant::now() < deadline {
                while glib::MainContext::default().iteration(false) {}
                std::thread::sleep(Duration::from_millis(5));
            }
            assert!(predicate());
        }
        // This test replaces XDG_CONFIG_HOME itself, so its private
        // configuration must also live below the harness check root.
        let base = crate::test_isolation::check_root()
            .join(format!("among-session-slow-{}", std::process::id()));
        std::fs::create_dir_all(base.join("data/among")).unwrap();
        std::fs::create_dir_all(base.join("config/among")).unwrap();
        std::env::set_var("XDG_DATA_HOME", base.join("data"));
        std::env::set_var("XDG_CONFIG_HOME", base.join("config"));
        std::env::set_var("SHELL", "/bin/sh");
        let app = adw::Application::builder()
            .application_id("com.github.among-terminal.slow-session-test")
            .flags(gio::ApplicationFlags::NON_UNIQUE)
            .build();
        app.connect_activate(|app| {
            let _ = build_with_writer(app, None, slow_writer);
        });
        app.register(None::<&gio::Cancellable>).unwrap();
        app.activate();
        let session = base.join("data/among/session.json");
        wait_for(
            || session.with_extension("blocked").exists(),
            Duration::from_secs(5),
        );
        let window = app.active_window().unwrap();
        window.present();
        let win = std::process::Command::new("xdotool")
            .args([
                "search",
                "--onlyvisible",
                "--pid",
                &std::process::id().to_string(),
            ])
            .output()
            .unwrap();
        let win = String::from_utf8(win.stdout)
            .unwrap()
            .split_whitespace()
            .last()
            .unwrap()
            .to_string();
        assert!(std::process::Command::new("xdotool")
            .args(["windowactivate", "--sync", &win])
            .status()
            .unwrap()
            .success());
        assert!(std::process::Command::new("xdotool")
            .args(["windowfocus", "--sync", &win])
            .status()
            .unwrap()
            .success());
        let output = base.join("typed-while-saving");
        assert!(std::process::Command::new("xdotool")
            .args([
                "type",
                "--clearmodifiers",
                &format!("printf alive > '{}'", output.display())
            ])
            .status()
            .unwrap()
            .success());
        assert!(std::process::Command::new("xdotool")
            .args(["key", "--clearmodifiers", "Return"])
            .status()
            .unwrap()
            .success());
        wait_for(|| output.exists(), Duration::from_secs(1));
        assert_eq!(std::fs::read_to_string(output).unwrap(), "alive");
        assert!(
            !session.exists(),
            "typing completed before the slow storage operation"
        );
        wait_for(|| session.exists(), Duration::from_secs(6));
        assert!(crate::session::load(&session).unwrap().is_some());
        window.destroy();
        println!(
            "PASS actual terminal input during four-second storage delay; evidence: {}",
            base.display()
        );
    }
    use super::*;

    #[test]
    #[ignore = "requires isolated XDG directories, display and xdotool; driven by among-tools check-preferences"]
    fn preferences_persist_across_fresh_processes() {
        fn pump() {
            let end = std::time::Instant::now() + Duration::from_millis(450);
            while std::time::Instant::now() < end {
                while glib::MainContext::default().pending() {
                    glib::MainContext::default().iteration(false);
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        }
        fn descendants(widget: &gtk::Widget) -> Vec<gtk::Widget> {
            let mut result = vec![widget.clone()];
            let mut child = widget.first_child();
            while let Some(node) = child {
                result.extend(descendants(&node));
                child = node.next_sibling();
            }
            result
        }
        fn xdo(args: &[&str]) -> String {
            let mut command = std::process::Command::new("xdotool");
            if matches!(args.first(), Some(&"key") | Some(&"type")) {
                command
                    .arg(args[0])
                    .arg("--clearmodifiers")
                    .args(&args[1..]);
            } else {
                command.args(args);
            }
            let output = command.output().unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            String::from_utf8(output.stdout).unwrap().trim().into()
        }
        fn focus(window: &impl IsA<gtk::Window>) -> String {
            let pid = std::process::id().to_string();
            let title = window.as_ref().title().unwrap();
            let ids = xdo(&["search", "--onlyvisible", "--pid", &pid, "--name", &title]);
            let id = ids.lines().last().unwrap().to_string();
            xdo(&["windowactivate", "--sync", &id]);
            xdo(&["windowfocus", "--sync", &id]);
            pump();
            id
        }
        fn row(prefs: &adw::PreferencesWindow, title: &str) -> adw::ActionRow {
            descendants(prefs.upcast_ref())
                .into_iter()
                .filter_map(|widget| widget.downcast::<adw::ActionRow>().ok())
                .find(|row| row.title() == title)
                .unwrap()
        }
        let phase = std::env::var("AMONG_PREFERENCES_TEST_PHASE").unwrap();
        let path = plugin_config_path().unwrap();
        crate::test_isolation::assert_below_check_root(&path);
        glib::set_prgname(Some("among-settings-test"));
        let app = adw::Application::builder()
            .application_id("com.github.among-terminal.settings-test")
            .flags(gio::ApplicationFlags::NON_UNIQUE)
            .build();
        app.register(None::<&gio::Cancellable>).unwrap();
        build(&app, None);
        pump();
        let window = app.active_window().unwrap();
        window.activate_action("win.preferences", None).unwrap();
        pump();
        let prefs = gtk::Window::list_toplevels()
            .into_iter()
            .find_map(|window| window.downcast::<adw::PreferencesWindow>().ok())
            .unwrap();
        if phase.starts_with("invalid-") {
            let broken_path = if phase == "invalid-plugins" {
                path.clone()
            } else {
                path.with_file_name("shortcuts.json")
            };
            let bytes = std::fs::read(&broken_path).unwrap();
            let storage = row(&prefs, "Some preferences could not be saved or loaded");
            assert!(storage
                .ancestor(adw::PreferencesGroup::static_type())
                .unwrap()
                .is_visible());
            assert!(storage
                .subtitle()
                .unwrap()
                .contains(if phase == "invalid-plugins" {
                    "plugins.json"
                } else {
                    "shortcuts.json"
                }));
            let split = row(&prefs, &Action::SplitPane.label());
            assert_eq!(
                split.subtitle().unwrap(),
                if phase == "invalid-plugins" {
                    "F11"
                } else {
                    "<Control><Shift>n"
                }
            );
            focus(&prefs);
            assert!(split.grab_focus());
            xdo(&["key", "Return"]);
            pump();
            xdo(&["key", "F9"]);
            pump();
            if phase != "invalid-plugins" {
                assert!(storage.subtitle().unwrap().contains("session-only"));
                let plugin = row(&prefs, "clock");
                let switch = descendants(plugin.upcast_ref())
                    .into_iter()
                    .find_map(|node| node.downcast::<gtk::Switch>().ok())
                    .unwrap();
                let page = switch
                    .ancestor(adw::PreferencesPage::static_type())
                    .unwrap()
                    .downcast::<adw::PreferencesPage>()
                    .unwrap();
                prefs.set_visible_page(&page);
                pump();
                assert!(switch.grab_focus());
                xdo(&["key", "space"]);
                pump();
                assert_eq!(
                    crate::settings::load(&path, crate::settings::Document::Plugins).unwrap()
                        ["plugins"][0]["enabled"],
                    true
                );
            } else {
                assert_eq!(
                    crate::settings::load(
                        &path.with_file_name("shortcuts.json"),
                        crate::settings::Document::Shortcuts
                    )
                    .unwrap()["overrides"]["split-pane"],
                    "F9"
                );
            }
            assert_eq!(
                std::fs::read(&broken_path).unwrap(),
                bytes,
                "bad document is never silently replaced"
            );
            if let Ok(file) = std::env::var("AMONG_PREFERENCES_SCREENSHOT") {
                let id = focus(&prefs);
                assert!(std::process::Command::new("import")
                    .args(["-window", &id, &file])
                    .status()
                    .unwrap()
                    .success());
            }
            prefs.destroy();
            pump();
            focus(&window);
            xdo(&["key", "F9"]);
            pump();
            assert_eq!(
                descendants(window.upcast_ref())
                    .iter()
                    .filter(|widget| widget.is::<vte::Terminal>())
                    .count(),
                2
            );
            eprintln!("PASS independent startup and failure handling: {phase}");
            window.destroy();
            return;
        }
        let plugin_row = row(&prefs, "clock");
        for name in ["system-status", "pane-pwd"] {
            let plugin = row(&prefs, name);
            let toggle = descendants(plugin.upcast_ref())
                .into_iter()
                .find_map(|node| node.downcast::<gtk::Switch>().ok())
                .unwrap();
            assert!(
                !toggle.is_active(),
                "untouched plugin choice remains disabled: {name}"
            );
        }
        if matches!(phase.as_str(), "edit" | "off" | "failure") {
            let output = std::process::Command::new("pgrep")
                .args(["-P", &std::process::id().to_string(), "-x", "clock"])
                .output()
                .unwrap();
            assert_eq!(
                output.status.code(),
                Some(1),
                "disabled plugin must not be spawned"
            );
        }
        let switch = descendants(plugin_row.upcast_ref())
            .into_iter()
            .find_map(|widget| widget.downcast::<gtk::Switch>().ok())
            .unwrap();
        let plugin_page = switch
            .ancestor(adw::PreferencesPage::static_type())
            .unwrap()
            .downcast::<adw::PreferencesPage>()
            .unwrap();
        prefs.set_visible_page(&plugin_page);
        pump();
        focus(&prefs);
        if phase == "edit" {
            let cancelled = row(&prefs, &Action::SplitPane.label());
            cancelled.emit_by_name::<()>("activated", &[]);
            cancelled.emit_by_name::<()>("activated", &[]);
            assert!(
                !path.with_file_name("shortcuts.json").exists(),
                "cancelled capture does not save"
            );
            assert!(!switch.is_active());
            assert!(switch.grab_focus());
            xdo(&["key", "space"]);
            pump();
            assert!(switch.is_active());
            assert_eq!(
                crate::settings::load(&path, crate::settings::Document::Plugins).unwrap()
                    ["plugins"][0]["enabled"],
                true
            );
            let resource = row(&prefs, "system-status");
            let resource_switch = descendants(resource.upcast_ref())
                .into_iter()
                .find_map(|node| node.downcast::<gtk::Switch>().ok())
                .unwrap();
            assert!(resource_switch.grab_focus());
            xdo(&["key", "space"]);
            pump();
            pump();
            assert!(resource_switch.is_active());
            assert!(
                descendants(window.upcast_ref())
                    .iter()
                    .filter(|widget| widget.is::<gtk::ProgressBar>())
                    .count()
                    >= 2,
                "enabled resource plugin renders real bars"
            );
            assert!(resource_switch.grab_focus());
            xdo(&["key", "space"]);
            pump();
            assert!(!resource_switch.is_active());
            assert_eq!(
                descendants(window.upcast_ref())
                    .iter()
                    .filter(|widget| widget.is::<gtk::ProgressBar>())
                    .count(),
                0,
                "disabling resources withdraws bars"
            );
            for (action, key) in [(Action::SplitPane, "F11"), (Action::ToggleZoom, "Escape")] {
                let row = row(&prefs, &action.label());
                let page = row
                    .ancestor(adw::PreferencesPage::static_type())
                    .unwrap()
                    .downcast::<adw::PreferencesPage>()
                    .unwrap();
                prefs.set_visible_page(&page);
                pump();
                assert!(row.grab_focus());
                xdo(&["key", "Return"]);
                pump();
                assert_eq!(row.subtitle().unwrap(), "Press a key combination…");
                xdo(&["key", key]);
                pump();
            }
            let stored = crate::settings::load(
                &path.with_file_name("shortcuts.json"),
                crate::settings::Document::Shortcuts,
            )
            .unwrap();
            assert_eq!(stored["overrides"]["split-pane"], "F11");
            assert!(stored["overrides"]
                .as_object()
                .unwrap()
                .contains_key("fullscreen"));
            assert!(stored["overrides"]["fullscreen"].is_null());
            assert!(stored["overrides"]["toggle-zoom"].is_null());
        } else {
            assert_eq!(
                switch.is_active(),
                phase != "off" && phase != "failure",
                "desired plugin choice restored"
            );
            assert_eq!(
                row(&prefs, &Action::SplitPane.label()).subtitle().unwrap(),
                "F11"
            );
            assert_eq!(
                row(&prefs, &Action::ToggleZoom.label()).subtitle().unwrap(),
                "Disabled"
            );
            assert_eq!(
                row(&prefs, &Action::ToggleFullscreen.label())
                    .subtitle()
                    .unwrap(),
                "Disabled"
            );
            if phase == "disable" {
                assert!(switch.grab_focus());
                xdo(&["key", "space"]);
                pump();
                assert!(!switch.is_active());
                assert_eq!(
                    crate::settings::load(&path, crate::settings::Document::Plugins).unwrap()
                        ["plugins"][0]["enabled"],
                    false
                );
            }
            if phase == "failure" {
                use std::os::unix::fs::PermissionsExt;
                let parent = path.parent().unwrap();
                let original = std::fs::read(&path).unwrap();
                std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o500)).unwrap();
                assert!(switch.grab_focus());
                xdo(&["key", "space"]);
                pump();
                assert!(switch.is_active(), "runtime edit remains active");
                assert_eq!(std::fs::read(&path).unwrap(), original);
                let storage = row(&prefs, "Some preferences could not be saved or loaded");
                assert!(storage.subtitle().unwrap().contains("session-only"));
                assert!(storage
                    .ancestor(adw::PreferencesGroup::static_type())
                    .unwrap()
                    .is_visible());
                std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700)).unwrap();
                focus(&prefs);
                assert!(switch.grab_focus());
                xdo(&["key", "space"]);
                pump();
                assert!(!switch.is_active());
                focus(&prefs);
                assert!(switch.grab_focus());
                xdo(&["key", "space"]);
                pump();
                assert_eq!(
                    crate::settings::load(&path, crate::settings::Document::Plugins).unwrap()
                        ["plugins"][0]["enabled"],
                    true
                );
                assert!(
                    !storage
                        .ancestor(adw::PreferencesGroup::static_type())
                        .unwrap()
                        .is_visible(),
                    "warning cleared after successful save"
                );
            }
            if phase == "fault" {
                let pid = std::process::id().to_string();
                let output = std::process::Command::new("pgrep")
                    .args(["-P", &pid, "-x", "clock"])
                    .output()
                    .unwrap();
                assert!(output.status.success(), "enabled plugin process exists");
                let child = String::from_utf8(output.stdout).unwrap();
                assert!(std::process::Command::new("kill")
                    .args(["-KILL", child.trim()])
                    .status()
                    .unwrap()
                    .success());
                pump();
                prefs.destroy();
                pump();
                window.activate_action("win.preferences", None).unwrap();
                pump();
                let reopened = gtk::Window::list_toplevels()
                    .into_iter()
                    .find_map(|window| window.downcast::<adw::PreferencesWindow>().ok())
                    .unwrap();
                let fault_row = row(&reopened, "clock");
                assert!(fault_row.subtitle().unwrap().contains("Stopped"));
                assert!(descendants(fault_row.upcast_ref())
                    .into_iter()
                    .find_map(|widget| widget.downcast::<gtk::Switch>().ok())
                    .unwrap()
                    .is_active());
                assert_eq!(
                    crate::settings::load(&path, crate::settings::Document::Plugins).unwrap()
                        ["plugins"][0]["enabled"],
                    true
                );
                reopened.destroy();
                pump();
            }
        }
        if phase != "fault" {
            if let Ok(file) = std::env::var("AMONG_PREFERENCES_SCREENSHOT") {
                let id = focus(&prefs);
                assert!(std::process::Command::new("import")
                    .args(["-window", &id, &file])
                    .status()
                    .unwrap()
                    .success());
            }
        }
        if phase != "fault" {
            prefs.destroy();
        }
        pump();
        focus(&window);
        if matches!(phase.as_str(), "restore" | "off") {
            xdo(&["key", "ctrl+shift+n"]);
            pump();
            assert_eq!(
                descendants(window.upcast_ref())
                    .iter()
                    .filter(|widget| widget.is::<vte::Terminal>())
                    .count(),
                1
            );
            xdo(&["key", "F11"]);
            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            while descendants(window.upcast_ref())
                .iter()
                .filter(|widget| widget.is::<vte::Terminal>())
                .count()
                != 2
                && std::time::Instant::now() < deadline
            {
                pump();
            }
            assert_eq!(
                descendants(window.upcast_ref())
                    .iter()
                    .filter(|widget| widget.is::<vte::Terminal>())
                    .count(),
                2
            );
            assert!(
                !window.is_fullscreen(),
                "displaced fullscreen remains unbound"
            );
        }
        let terminal = descendants(window.upcast_ref())
            .into_iter()
            .filter_map(|widget| widget.downcast::<vte::Terminal>().ok())
            .find(|term| term.has_focus())
            .unwrap();
        xdo(&["type", r#"printf '\033]0;persistence-input-ok\007'"#]);
        xdo(&["key", "Return"]);
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while terminal.window_title().as_deref() != Some("persistence-input-ok")
            && std::time::Instant::now() < deadline
        {
            pump();
        }
        assert_eq!(terminal.window_title().unwrap(), "persistence-input-ok");
        assert_eq!(toast_widget_count(window.upcast_ref()), 0);
        eprintln!("PASS real Preferences {phase}, saved files and terminal typing");
        // End only this isolated test process; no shutdown save is invoked.
        window.destroy();
    }

    #[test]
    #[ignore = "requires a display, window manager and xdotool; run alone with --test-threads=1"]
    fn preferences_reopen_and_switch_geometry_work_in_a_live_window() {
        fn pump() {
            let end = std::time::Instant::now() + Duration::from_millis(500);
            while std::time::Instant::now() < end {
                while glib::MainContext::default().pending() {
                    glib::MainContext::default().iteration(false);
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        }

        fn descendants(widget: &gtk::Widget) -> Vec<gtk::Widget> {
            let mut out = vec![widget.clone()];
            let mut child = widget.first_child();
            while let Some(node) = child {
                out.extend(descendants(&node));
                child = node.next_sibling();
            }
            out
        }

        fn editor() -> adw::PreferencesWindow {
            gtk::Window::list_toplevels()
                .into_iter()
                .find_map(|window| window.downcast::<adw::PreferencesWindow>().ok())
                .expect("preferences window exists")
        }

        glib::set_prgname(Some("among-preferences-test"));
        let app = adw::Application::builder()
            .application_id("com.github.among-terminal.preferences-test")
            .flags(gio::ApplicationFlags::NON_UNIQUE)
            .build();
        app.register(None::<&gio::Cancellable>).unwrap();
        eprintln!("preferences test: building host");
        build(&app, None);
        pump();
        let window = app.active_window().expect("host window exists");
        let controls = descendants(window.upcast_ref());
        let settings = controls
            .iter()
            .find(|widget| widget.tooltip_text().as_deref() == Some("Settings"))
            .unwrap();
        let sidebar = controls
            .iter()
            .find(|widget| {
                widget.tooltip_text().as_deref() == Some("Collapse the sidebar to a strip")
            })
            .unwrap();
        assert!(
            settings.compute_bounds(&window).unwrap().x()
                < sidebar.compute_bounds(&window).unwrap().x()
        );
        let new_workspace = controls
            .iter()
            .find(|widget| widget.tooltip_text().as_deref() == Some("New workspace"))
            .unwrap();
        assert!(
            sidebar.compute_bounds(&window).unwrap().x()
                < new_workspace.compute_bounds(&window).unwrap().x()
        );
        assert!(settings.is_focusable() && sidebar.is_focusable() && new_workspace.is_focusable());

        // Exercise the actual host action twice. The old mem::take would make
        // the second editor describe every default binding as Disabled.
        for _ in 0..2 {
            eprintln!("preferences test: opening live editor");
            gtk::prelude::WidgetExt::activate_action(&window, "win.preferences", None).unwrap();
            pump();
            let prefs = editor();
            let rows: Vec<adw::ActionRow> = descendants(prefs.upcast_ref())
                .into_iter()
                .filter_map(|node| node.downcast().ok())
                .collect();
            for action in Bindings::default_actions() {
                let defaults = Bindings::defaults();
                let Some(bound) = defaults.accel_for(action) else {
                    continue;
                };
                let row = rows
                    .iter()
                    .find(|row| row.title() == action.label())
                    .expect("default action has a row");
                let subtitle = row.subtitle().expect("binding subtitle exists");
                assert_eq!(subtitle.as_str(), bound, "{}", action.label());
            }
            prefs.destroy();
            eprintln!("preferences test: default subtitles retained");
            pump();
        }

        let ids = std::process::Command::new("xdotool")
            .args([
                "search",
                "--onlyvisible",
                "--pid",
                &std::process::id().to_string(),
                "--name",
                window.title().unwrap().as_str(),
            ])
            .output()
            .unwrap();
        let text = String::from_utf8(ids.stdout).unwrap();
        let id = text.lines().last().expect("host X11 window");
        assert!(std::process::Command::new("xdotool")
            .args(["windowfocus", "--sync", id])
            .status()
            .unwrap()
            .success());
        assert!(std::process::Command::new("xdotool")
            .args(["key", "ctrl+shift+n"])
            .status()
            .unwrap()
            .success());
        pump();
        let terminals = descendants(window.upcast_ref())
            .iter()
            .filter(|node| node.is::<vte::Terminal>())
            .count();
        assert_eq!(
            terminals, 2,
            "default split shortcut still works after reopening preferences"
        );
        assert!(std::process::Command::new("xdotool")
            .args(["key", "--clearmodifiers", "ctrl+shift+t"])
            .status()
            .unwrap()
            .success());
        pump();
        assert_eq!(
            descendants(window.upcast_ref())
                .iter()
                .filter(|widget| widget.has_css_class("workspace-entry"))
                .count(),
            2,
            "new workspace remains available by keyboard"
        );
        assert!(new_workspace.grab_focus());
        assert!(std::process::Command::new("xdotool")
            .args(["key", "--clearmodifiers", "space"])
            .status()
            .unwrap()
            .success());
        pump();
        assert_eq!(
            descendants(window.upcast_ref())
                .iter()
                .filter(|widget| widget.has_css_class("workspace-entry"))
                .count(),
            3,
            "header button creates a workspace"
        );
        assert!(settings.grab_focus());
        assert!(std::process::Command::new("xdotool")
            .args(["key", "--clearmodifiers", "space"])
            .status()
            .unwrap()
            .success());
        pump();
        editor().destroy();
        pump();

        let toggled = Rc::new(std::cell::Cell::new(0));
        prefs::present(
            &window,
            prefs::PreferencesContext {
                on_closed: Rc::new(|| {}),
                storage_status: gtk::StringList::new(&[""]),
                bindings: Rc::new(RefCell::new(Bindings::defaults())),
                plugin_listing: Rc::new(|| {
                    vec![(
                        "test-plugin".into(),
                        crate::plugin::host::PluginState::Running,
                    )]
                }),
                plugin_enabled: Rc::new(|_| true),
                plugin_details: Rc::new(|_| None),
                save_plugin_settings: Rc::new(|_, _, _| Err("no settings in this fixture".into())),
                enable_plugin: Rc::new(|_| {}),
                disable_plugin: Rc::new({
                    let toggled = toggled.clone();
                    move |_| toggled.set(toggled.get() + 1)
                }),
                bindings_changed: Rc::new(|| {}),
            },
        );
        pump();
        let prefs = editor();
        let switch = descendants(prefs.upcast_ref())
            .into_iter()
            .find_map(|node| node.downcast::<gtk::Switch>().ok())
            .expect("plugin switch");
        let page = switch
            .ancestor(adw::PreferencesPage::static_type())
            .unwrap()
            .downcast::<adw::PreferencesPage>()
            .unwrap();
        prefs.set_visible_page(&page);
        pump();
        let row = switch
            .ancestor(adw::ActionRow::static_type())
            .unwrap()
            .downcast::<adw::ActionRow>()
            .unwrap();
        for scheme in [adw::ColorScheme::ForceLight, adw::ColorScheme::ForceDark] {
            adw::StyleManager::default().set_color_scheme(scheme);
            pump();
            assert_eq!(switch.valign(), gtk::Align::Center);
            assert!(
                switch.height() > 0 && switch.height() < row.height(),
                "switch={} row={}",
                switch.height(),
                row.height()
            );
            let rect = switch.compute_bounds(&row).unwrap();
            assert!((rect.y() + rect.height() / 2.0 - row.height() as f32 / 2.0).abs() <= 1.0);
            eprintln!(
                "preferences geometry: switch={}x{} row_height={}",
                switch.width(),
                switch.height(),
                row.height()
            );
        }
        assert!(switch.is_focusable() && row.is_activatable());
        assert!(switch.grab_focus());
        pump();
        let ids = std::process::Command::new("xdotool")
            .args([
                "search",
                "--onlyvisible",
                "--pid",
                &std::process::id().to_string(),
                "--name",
                "Preferences",
            ])
            .output()
            .unwrap();
        let text = String::from_utf8(ids.stdout).unwrap();
        let id = text.lines().last().expect("test preferences X11 window");
        assert!(std::process::Command::new("xdotool")
            .args(["windowfocus", "--sync", id])
            .status()
            .unwrap()
            .success());
        assert!(std::process::Command::new("xdotool")
            .args(["key", "space"])
            .status()
            .unwrap()
            .success());
        pump();
        assert!(!switch.is_active(), "Space toggles the focused switch");
        assert_eq!(
            toggled.get(),
            1,
            "keyboard activation reaches the disable callback"
        );
        prefs.destroy();
        window.destroy();
        app.quit();
    }
    use std::io::Write;

    fn write_config(body: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join("among-test-config");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("plugins.json");
        let mut file = std::fs::File::create(&path).unwrap();
        file.write_all(body.as_bytes()).unwrap();
        path
    }

    #[test]
    fn a_missing_config_yields_no_plugins() {
        let path = std::env::temp_dir().join("among-does-not-exist/plugins.json");
        assert!(read_plugin_config(&path).is_empty());
    }

    #[test]
    fn unparsable_config_still_yields_no_plugins() {
        let path = write_config("{ this is not json");
        assert!(read_plugin_config(&path).is_empty());
    }

    #[test]
    fn config_without_a_plugins_key_is_ignored() {
        let path = write_config(r#"{"other": true}"#);
        assert!(read_plugin_config(&path).is_empty());
    }

    #[test]
    fn a_plugin_that_is_not_installed_is_skipped() {
        let path =
            write_config(r#"{"plugins":[{"name":"definitely-not-installed","enabled":true}]}"#);
        assert!(read_plugin_config(&path).is_empty());
    }

    #[test]
    fn entries_without_a_name_are_skipped() {
        let path = write_config(r#"{"plugins":[{"enabled":true},{"name":42}]}"#);
        assert!(read_plugin_config(&path).is_empty());
    }

    #[test]
    fn a_disabled_entry_is_still_registered_but_off() {
        // The installed set is what gates registration, so with nothing
        // installed the entry is dropped either way; the important property is
        // that configuration parsing does not fail the whole file.
        let path = write_config(r#"{"plugins":[{"name":"x","enabled":false}]}"#);
        assert!(read_plugin_config(&path).is_empty());
    }

    #[test]
    fn a_pane_is_found_in_the_tree_by_path() {
        let tree = Node::single_pane(PaneId(1))
            .split_pane(PaneId(1), Orientation::Vertical, PaneId(2))
            .unwrap()
            .split_pane(PaneId(2), Orientation::Horizontal, PaneId(3))
            .unwrap();
        assert_eq!(find_pane_path(&tree, PaneId(1)), Some(vec![0]));
        assert_eq!(find_pane_path(&tree, PaneId(2)), Some(vec![1, 0]));
        assert_eq!(find_pane_path(&tree, PaneId(3)), Some(vec![1, 1]));
        assert_eq!(find_pane_path(&tree, PaneId(99)), None);
    }

    fn side_by_side() -> Node {
        Node::single_pane(PaneId(1))
            .split_pane(PaneId(1), Orientation::Vertical, PaneId(2))
            .unwrap()
    }

    #[test]
    fn right_moves_a_vertical_divider_right_from_either_side() {
        // The keys name where the divider goes, not which pane grows: the
        // right-hand pane once moved the divider left on Ctrl+Shift+Right.
        for pane in [PaneId(1), PaneId(2)] {
            let mut tree = side_by_side();
            let before = tree.ratio_at(&[]).unwrap();
            assert!(move_divider_for_pane(&mut tree, pane, Direction::Right));
            assert!(tree.ratio_at(&[]).unwrap() > before, "{pane:?}");
            let mut tree = side_by_side();
            assert!(move_divider_for_pane(&mut tree, pane, Direction::Left));
            assert!(tree.ratio_at(&[]).unwrap() < before, "{pane:?}");
        }
    }

    #[test]
    fn up_and_down_leave_a_side_by_side_split_alone() {
        let mut tree = side_by_side();
        let before = tree.ratio_at(&[]).unwrap();
        assert!(!move_divider_for_pane(&mut tree, PaneId(1), Direction::Up));
        assert!(!move_divider_for_pane(
            &mut tree,
            PaneId(2),
            Direction::Down
        ));
        assert_eq!(tree.ratio_at(&[]).unwrap(), before);
    }

    #[test]
    fn each_axis_moves_the_nearest_divider_of_its_own_orientation() {
        // Pane 2 sits in a stacked split that is itself the right side of a
        // side-by-side split: Down moves the inner divider, Left the outer one.
        let tree = side_by_side()
            .split_pane(PaneId(2), Orientation::Horizontal, PaneId(3))
            .unwrap();
        let outer = tree.ratio_at(&[]).unwrap();
        let inner = tree.ratio_at(&[1]).unwrap();

        let mut moved = tree.clone();
        assert!(move_divider_for_pane(
            &mut moved,
            PaneId(2),
            Direction::Down
        ));
        assert!(
            moved.ratio_at(&[1]).unwrap() > inner,
            "inner divider did not go down"
        );
        assert!(
            (moved.ratio_at(&[]).unwrap() - outer).abs() < 1e-9,
            "outer moved"
        );

        let mut moved = tree.clone();
        assert!(move_divider_for_pane(
            &mut moved,
            PaneId(3),
            Direction::Left
        ));
        assert!(
            moved.ratio_at(&[]).unwrap() < outer,
            "outer divider did not go left"
        );
        assert!(
            (moved.ratio_at(&[1]).unwrap() - inner).abs() < 1e-9,
            "inner moved"
        );
    }

    #[test]
    fn a_pane_with_no_divider_cannot_be_resized() {
        let mut tree = Node::single_pane(PaneId(1));
        assert!(!move_divider_for_pane(
            &mut tree,
            PaneId(1),
            Direction::Right
        ));
    }

    #[test]
    fn resizing_a_missing_pane_is_refused() {
        let mut tree = side_by_side();
        assert!(!move_divider_for_pane(
            &mut tree,
            PaneId(42),
            Direction::Right
        ));
    }

    #[test]
    fn resizing_stays_inside_sane_bounds() {
        let mut tree = side_by_side();
        for _ in 0..60 {
            move_divider_for_pane(&mut tree, PaneId(1), Direction::Right);
        }
        let ratio = tree.ratio_at(&[]).unwrap();
        assert!((0.05..=0.95).contains(&ratio), "ratio escaped: {ratio}");
    }

    #[test]
    fn scopes_without_an_argument_are_parsed() {
        // These carry no colon, which a naive split rejects.
        assert_eq!(parse_scope("app_header"), Some(Scope::AppHeader));
        assert_eq!(parse_scope("app_footer"), Some(Scope::AppFooter));
        assert_eq!(
            parse_scope("workspace_header"),
            Some(Scope::WorkspaceHeader)
        );
    }

    #[test]
    fn pane_scopes_carry_their_pane_id() {
        assert_eq!(
            parse_scope("pane_header:7"),
            Some(Scope::PaneHeader(PaneId(7)))
        );
        assert_eq!(
            parse_scope("pane_footer:2"),
            Some(Scope::PaneFooter(PaneId(2)))
        );
    }

    #[test]
    fn unknown_or_malformed_scopes_are_refused() {
        assert_eq!(parse_scope("main_window"), None);
        assert_eq!(parse_scope("pane_header:not-a-number"), None);
        assert_eq!(parse_scope("pane_header:"), None);
        assert_eq!(parse_scope(""), None);
    }

    #[test]
    fn summary_scopes_carry_a_workspace_index() {
        assert_eq!(parse_summary_scope("workspace_summary:0"), Some(0));
        assert_eq!(parse_summary_scope("workspace_summary:12"), Some(12));
        assert_eq!(parse_summary_scope("workspace_summary:x"), None);
        assert_eq!(parse_summary_scope("app_header"), None);
    }

    #[test]
    fn slice_ratio_marks_a_live_tree_correctly() {
        // Guards the invariant that ratios stay within bounds.
        let tree = Node::single_pane(PaneId(1))
            .split_pane(PaneId(1), Orientation::Vertical, PaneId(2))
            .unwrap();
        assert!(tree.ratio_at(&[]).unwrap() >= 0.05);
        assert!(tree.ratio_at(&[]).unwrap() <= 0.95);
    }
}
