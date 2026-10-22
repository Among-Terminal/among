//! Launching plugins, talking to them, and surviving their failures.

use super::process::{Failure, Process, SendError};
use super::transport::{MAX_POLL_FRAMES, MAX_QUEUE_FRAMES};
use std::cell::Cell;
use std::collections::{BTreeSet, HashMap};
use std::process::Command;
use std::rc::Rc;
use std::time::Duration;

use super::inventory;
use super::managed;
use super::protocol::{
    self, Context, EventKind, IncomingFragment, IncomingSummary, Rejection, Request, Response,
    PROTOCOL_MAJOR, PROTOCOL_MINOR,
};
use crate::slots::{Contribution, Fragment, Refresh, SlotStore, Style, Summary};

mod managed_runtime;
pub use managed_runtime::Configuration;
mod overview;
pub use overview::Overview;
mod notification_backend;

/// How long a plugin has to answer before it is treated as faulted.
///
/// Generous enough for a slow `git status` on a cold cache, short enough that a
/// wedged plugin is noticed rather than silently poisoning a slot forever.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_PROCESSES: usize = 32;
const MAX_RESTARTS: u32 = 3;
// one reply per accepted status request must fit the bounded stdout queue even
// if the plugin answers every request before GTK next gets a chance to poll.
const MAX_INFLIGHT: usize = MAX_QUEUE_FRAMES;

/// A plugin's description as read from configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginSpec {
    pub name: String,
    /// The command to run; `argv[0]` is the program.
    pub command: Vec<String>,
    /// Whether the plugin starts with the application.
    pub enabled: bool,
}

/// Where a plugin currently stands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PluginState {
    Unavailable {
        reason: String,
    },
    Waiting,
    Starting,
    Stopping {
        reason: String,
        enabled: bool,
    },
    /// Configured and running.
    Running,
    /// Configured but switched off by the user.
    Disabled,
    /// The program could not be started.
    Missing {
        reason: String,
    },
    /// The program started but speaks an incompatible protocol.
    Incompatible {
        plugin_major: u32,
        host_major: u32,
    },
    /// The plugin stopped, hung, or was restarted after a fault.
    Faulted {
        reason: String,
    },
}

impl PluginState {
    /// Whether the plugin's data can be trusted as current.
    pub fn is_healthy(&self) -> bool {
        matches!(self, PluginState::Running)
    }
}

/// retained until the supervisor has really reaped this generation.
struct PluginIo {
    process: Process,
    started: Instant,
    /// Set once the plugin has announced a compatible version.
    handshake_done: bool,
    generation: String,
    notification_live: Option<among::plugin_notifications::OriginLease>,
    package_version: Option<String>,
}

/// One plugin under management.
struct PluginEntry {
    spec: PluginSpec,
    state: PluginState,
    io: Option<PluginIo>,
    /// Requests sent and not yet answered, for timeout accounting.
    inflight: HashMap<u64, Instant>,
    /// The scope each outstanding request asked about.
    scopes: HashMap<u64, String>,
    next_request_id: u64,
    /// The refresh policy the plugin last declared.
    refresh: Refresh,
    restart_at: Option<Instant>,
    restart_attempts: u32,
    after_stop: Option<PluginState>,
    managed: Option<inventory::Entry>,
    settings: Option<serde_json::Value>,
    runtime: Option<managed_runtime::Runtime>,
    observed: Option<super::manifest::Api>,
    removing: bool,
    removed: bool,
}

use std::time::Instant;

/// Hosts every configured plugin and pumps their output on the main loop.
pub struct PluginHost {
    plugins: Vec<PluginEntry>,
    /// Failures recorded for the current update, for diagnostics.
    last_rejections: Vec<(String, Rejection)>,
    /// Answers collected during the last [`PluginHost::poll`], for the host to
    /// turn into slot content.
    results: Vec<PluginResult>,
    /// Notifications raised since the last poll.
    notices: Vec<(String, String, Option<String>)>,
    invalidated_sources: BTreeSet<String>,
    next_poll: usize,
    starts_allowed: Rc<Cell<bool>>,
    world: managed::World,
    notifications: Option<Rc<std::cell::RefCell<among::plugin_notifications::Backend>>>,
}

/// One plugin's answer, ready to become slot content.
#[derive(Debug, Clone)]
pub struct PluginResult {
    /// Which plugin answered.
    pub plugin: String,
    /// The scope that was asked about.
    pub scope: String,
    pub fragments: Vec<IncomingFragment>,
    /// `None` means the plugin declared event-driven refresh.
    pub refresh: Refresh,
}

fn stop_io(plugin: &mut PluginEntry) {
    if let Some(io) = plugin.io.as_ref() {
        if let Some(live) = &io.notification_live {
            live.revoke();
        }
        io.process.stop();
    }
    plugin.inflight.clear();
    plugin.scopes.clear();
    if let Some(runtime) = plugin.runtime.as_mut() {
        runtime.reset();
    }
}

impl Default for PluginHost {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for PluginHost {
    fn drop(&mut self) {
        self.shutdown();
    }
}

impl PluginHost {
    pub fn new() -> Self {
        Self::with_start_gate(Rc::new(Cell::new(true)))
    }

    pub fn with_start_gate(starts_allowed: Rc<Cell<bool>>) -> Self {
        Self::with_notification_backend(starts_allowed, None)
    }

    pub fn with_notification_backend(
        starts_allowed: Rc<Cell<bool>>,
        notifications: Option<Rc<std::cell::RefCell<among::plugin_notifications::Backend>>>,
    ) -> Self {
        let mut host = Self {
            plugins: Vec::new(),
            last_rejections: Vec::new(),
            results: Vec::new(),
            notices: Vec::new(),
            invalidated_sources: Default::default(),
            next_poll: 0,
            starts_allowed,
            world: managed::World {
                session: glib::uuid_string_random().to_string(),
                ..Default::default()
            },
            notifications: None,
        };
        host.notifications = notifications.filter(|backend| {
            backend
                .try_borrow_mut()
                .is_ok_and(|mut backend| backend.bind(&host.world.session))
        });
        host
    }

    pub fn drain_invalidated_sources(&mut self) -> Vec<String> {
        std::mem::take(&mut self.invalidated_sources)
            .into_iter()
            .collect()
    }

    /// Take the notifications raised since the last poll.
    ///
    /// Returned separately from slot content because a notification is for the
    /// user rather than a description of the interface state, and must not be
    /// bound to a scope.
    pub fn drain_notices(&mut self) -> Vec<(String, String, Option<String>)> {
        std::mem::take(&mut self.notices)
    }

    /// Take the answers collected since the last poll.
    pub fn drain_results(&mut self) -> Vec<PluginResult> {
        std::mem::take(&mut self.results)
    }

    /// Register plugins from configuration.
    ///
    /// A plugin whose file is missing is recorded as `Missing` rather than
    /// aborting: one broken entry must not stop the application from starting.
    pub fn register_all(&mut self, specs: Vec<PluginSpec>) {
        for spec in specs {
            self.register(spec);
        }
    }

    /// Register a single plugin and start it if enabled.
    pub fn register(&mut self, spec: PluginSpec) {
        let name = spec.name.clone();
        self.plugins.push(PluginEntry {
            spec,
            state: PluginState::Disabled,
            io: None,
            inflight: HashMap::new(),
            scopes: HashMap::new(),
            next_request_id: 1,
            refresh: Refresh::IntervalMs(2000),
            restart_at: None,
            restart_attempts: 0,
            after_stop: None,
            managed: None,
            settings: None,
            runtime: None,
            observed: None,
            removing: false,
            removed: false,
        });
        self.start_by_name(name);
    }

    pub fn register_managed(&mut self, entry: inventory::Entry, local: Option<&serde_json::Value>) {
        let name = entry.key();
        if self.find(&name).is_some() {
            return;
        }
        let removing = entry.removal_pending();
        let desired = !removing
            && local
                .and_then(|local| local.get("enabled"))
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false)
            && entry
                .receipt
                .installation
                .as_ref()
                .is_none_or(|installation| {
                    local
                        .and_then(|local| local.get("installation"))
                        .and_then(serde_json::Value::as_str)
                        == Some(installation.as_str())
                });
        // never pass a managed receipt through legacy registration: its static
        // compatibility must be checked before any executable can start.
        self.register(PluginSpec {
            name: name.clone(),
            command: Vec::new(),
            enabled: false,
        });
        if let Some(index) = self.find(&name) {
            self.plugins[index].removing = removing;
            self.plugins[index].managed = Some(entry);
            self.plugins[index].settings = local.and_then(|local| local.get("settings")).cloned();
            self.plugins[index].runtime = Some(managed_runtime::Runtime::new(
                local.and_then(|local| local.get("review")).cloned(),
            ));
            self.plugins[index].spec.enabled = desired;
            self.declare_notification_channels(index);
            self.start_by_name(name);
        }
    }

    pub fn reconcile_managed(
        &mut self,
        entry: inventory::Entry,
        local: Option<&serde_json::Value>,
    ) {
        let key = entry.key();
        let Some(index) = self.find(&key) else {
            self.register_managed(entry, local);
            return;
        };
        if self.plugins[index].removing {
            if entry.removal_pending() {
                return;
            }
            self.plugins[index].removing = false;
        }
        let Some(current) = &self.plugins[index].managed else {
            return;
        };
        if current.receipt == entry.receipt {
            return;
        }
        // an observer can miss absence between removal and a fresh install.
        // only the same installation authority may inherit its live desire.
        let fresh_installation = current.receipt.installation != entry.receipt.installation;
        if fresh_installation {
            self.plugins[index].spec.enabled = !entry.removal_pending()
                && entry
                    .receipt
                    .installation
                    .as_ref()
                    .is_some_and(|installation| {
                        local.is_some_and(|local| {
                            local.get("enabled").and_then(serde_json::Value::as_bool) == Some(true)
                                && local
                                    .get("installation")
                                    .and_then(serde_json::Value::as_str)
                                    == Some(installation.as_str())
                        })
                    });
        }
        self.stop_as(
            index,
            PluginState::Disabled,
            if fresh_installation {"installation authority changed; revoking the previous runtime before fresh activation"}
            else {"applying explicitly approved package update; waiting for process exit"},
        );
        if fresh_installation {
            self.plugins[index].runtime = Some(managed_runtime::Runtime::new(
                local.and_then(|local| local.get("review")).cloned(),
            ));
            self.plugins[index].removing = entry.removal_pending();
            self.plugins[index].removed = false;
            self.invalidated_sources.insert(key.clone());
        }
        self.plugins[index].managed = Some(entry);
        self.plugins[index].observed = None;
        self.plugins[index].restart_at = None;
        self.plugins[index].restart_attempts = 0;
        if self.plugins[index].io.is_none() {
            self.start_by_name(key);
        }
    }

    pub fn desired_enabled(&self, name: &str) -> bool {
        self.find(name)
            .is_some_and(|index| self.plugins[index].spec.enabled)
    }

    pub fn begin_removal(
        &mut self,
        expected: &inventory::Receipt,
    ) -> Result<(), super::data::Problem> {
        let key = expected.identity()?.key();
        let index = self
            .find(&key)
            .ok_or(super::data::Problem("plugin is no longer registered"))?;
        if self.plugins[index]
            .managed
            .as_ref()
            .is_none_or(|entry| entry.receipt != *expected)
        {
            return Err(super::data::Problem(
                "uninstall confirmation is stale; selected generation changed",
            ));
        }
        self.plugins[index].removing = true;
        self.disable(&key);
        Ok(())
    }

    pub fn supervised_stopped(&self, key: &str) -> bool {
        self.find(key)
            .is_none_or(|index| self.plugins[index].io.is_none())
    }

    pub fn removal_failed(&mut self, key: &str) {
        if let Some(index) = self.find(key) {
            self.plugins[index].removing = false;
        }
    }

    pub fn reconcile_removed(&mut self, selected: &BTreeSet<String>) {
        let absent = self
            .plugins
            .iter()
            .filter(|plugin| plugin.managed.is_some() && !selected.contains(&plugin.spec.name))
            .map(|plugin| plugin.spec.name.clone())
            .collect::<Vec<_>>();
        for key in absent {
            self.disable(&key);
            if let Some(index) = self.find(&key) {
                self.plugins[index].removing = true;
                self.plugins[index].removed = true;
            }
        }
        self.plugins
            .retain(|plugin| !plugin.removed || plugin.io.is_some());
    }

    pub fn managed_details(
        &self,
        name: &str,
    ) -> Option<(inventory::Entry, Option<serde_json::Value>)> {
        let entry = &self.plugins[self.find(name)?];
        Some((entry.managed.clone()?, entry.settings.clone()))
    }

    pub fn stored_settings_changed(&mut self, name: &str, stored: serde_json::Value) {
        if let Some(index) = self
            .find(name)
            .filter(|index| self.plugins[*index].managed.is_some())
        {
            self.plugins[index].settings = Some(stored);
            self.settings_saved(index);
        }
    }

    pub fn update_context(&mut self, workspaces: Vec<u64>, legacy: Context) {
        let previous = self
            .world
            .workspaces
            .get(self.world.legacy.active_workspace)
            .copied();
        self.world.workspaces = workspaces;
        self.world.legacy = legacy;
        self.world
            .legacy
            .terminals
            .sort_by_key(|terminal| terminal.pane);
        self.revoke_stale_targets();
        self.reconcile_notifications();
        if previous.is_some()
            && previous
                != self
                    .world
                    .workspaces
                    .get(self.world.legacy.active_workspace)
                    .copied()
        {
            if let Some(pane) = self.world.legacy.focused_pane {
                self.broadcast_event(EventKind::WorkspaceActivated, pane, None);
            }
        }
    }

    fn find(&self, name: &str) -> Option<usize> {
        self.plugins.iter().position(|p| p.spec.name == name)
    }

    /// Names and states of every configured plugin, for listing.
    pub fn listing(&self) -> Vec<(String, PluginState)> {
        self.plugins
            .iter()
            .map(|p| (p.spec.name.clone(), p.state.clone()))
            .collect()
    }

    /// Switch a plugin on, starting it if necessary.
    pub fn enable(&mut self, name: &str) {
        let Some(index) = self.find(name) else { return };
        if self.plugins[index].removing {
            return;
        }
        if self.plugins[index].spec.enabled {
            return;
        }
        self.plugins[index].spec.enabled = true;
        if let PluginState::Stopping { enabled, .. } = &mut self.plugins[index].state {
            *enabled = true;
        }
        self.plugins[index].restart_attempts = 0;
        self.plugins[index].restart_at = Some(Instant::now());
        if self.plugins[index].io.is_none() {
            self.start_by_name(name.to_string());
        }
    }

    /// Switch a plugin off and stop its process.
    pub fn disable(&mut self, name: &str) {
        let Some(index) = self.find(name) else { return };
        self.plugins[index].spec.enabled = false;
        self.plugins[index].restart_at = None;
        self.stop_as(
            index,
            PluginState::Disabled,
            "disabled; waiting for process exit",
        );
    }

    /// Stop every plugin, for shutdown.
    pub fn shutdown(&mut self) {
        for index in 0..self.plugins.len() {
            let name = self.plugins[index].spec.name.clone();
            self.disable(&name);
        }
    }

    pub fn is_stopped(&self) -> bool {
        self.plugins.iter().all(|plugin| plugin.io.is_none())
    }

    #[cfg(test)]
    pub fn supervised_count(&self) -> usize {
        self.plugins
            .iter()
            .filter(|plugin| plugin.io.is_some())
            .count()
    }

    #[cfg(test)]
    pub fn supervised_pids(&self) -> Vec<u32> {
        self.plugins
            .iter()
            .filter_map(|plugin| plugin.io.as_ref())
            .map(|io| io.process.pid())
            .filter(|pid| *pid != 0)
            .collect()
    }

    /// Spawn a plugin process.
    ///
    /// Failure to start is not an error for the host: it is recorded as the
    /// plugin's state and every other plugin continues.
    fn start_by_name(&mut self, name: String) {
        let Some(index) = self.find(&name) else {
            return;
        };
        if !self.plugins[index].spec.enabled
            || self.plugins[index].io.is_some()
            || self.plugins[index].removing
        {
            return;
        }
        if self.plugins[index].managed.is_some() {
            if self.plugins[index]
                .managed
                .as_ref()
                .is_some_and(|entry| entry.removal_pending())
            {
                self.plugins[index].state = PluginState::Unavailable {
                    reason: "uninstall is still active for this installation".into(),
                };
                self.plugins[index].restart_at = None;
                return;
            }
            if let Err(problem) = self.prepare_managed(index) {
                self.plugins[index].state = PluginState::Unavailable {
                    reason: problem.to_string(),
                };
                self.plugins[index].restart_at = None;
                return;
            }
        }
        if !self.starts_allowed.get() {
            self.plugins[index].state = PluginState::Waiting;
            self.plugins[index].restart_at = None;
            return;
        }
        if self
            .plugins
            .iter()
            .filter(|plugin| plugin.io.is_some())
            .count()
            >= MAX_PROCESSES
        {
            self.plugins[index].state = PluginState::Faulted {
                reason: "32 supervised plugins already active".into(),
            };
            self.plugins[index].restart_at = None;
            return;
        }

        self.plugins[index].restart_at = None;

        let command = self.plugins[index]
            .managed
            .as_ref()
            .and_then(|entry| entry.inspected.as_ref().ok())
            .map(|inspected| vec![inspected.executable.to_string_lossy().into_owned()])
            .unwrap_or_else(|| self.plugins[index].spec.command.clone());
        if command.is_empty() {
            self.plugins[index].state = PluginState::Missing {
                reason: "no command configured".into(),
            };
            return;
        }

        let mut program = if let Some(entry) = &self.plugins[index].managed {
            match entry.executable_files() {
                Ok((directory, executable)) => {
                    super::process::pinned_program(directory, executable, entry.generation_lease())
                }
                Err(problem) => {
                    self.plugins[index].state = PluginState::Unavailable {
                        reason: problem.to_string(),
                    };
                    return;
                }
            }
        } else {
            let mut program = Command::new(&command[0]);
            program.args(&command[1..]);
            program
        };
        if self.plugins[index].managed.is_some() {
            program
                .env_clear()
                .env("PATH", "/usr/local/bin:/usr/bin:/bin")
                .env("LANG", "C.UTF-8");
            for variable in [
                "HOME",
                "XDG_DATA_HOME",
                "XDG_CONFIG_HOME",
                "XDG_CACHE_HOME",
                "XDG_RUNTIME_DIR",
            ] {
                if let Some(value) = std::env::var_os(variable) {
                    program.env(variable, value);
                }
            }
        }
        let spawned = Process::start(program);

        match spawned {
            Ok(process) => {
                self.plugins[index].io = Some(PluginIo {
                    process,
                    started: Instant::now(),
                    handshake_done: false,
                    generation: glib::uuid_string_random().to_string(),
                    notification_live: None,
                    package_version: self.plugins[index]
                        .managed
                        .as_ref()
                        .and_then(|entry| entry.inspected.as_ref().ok())
                        .map(|inspected| inspected.manifest.version.clone()),
                });
                self.plugins[index].restart_at = None;
                self.plugins[index].after_stop = None;
                self.plugins[index].state = PluginState::Starting;
                self.plugins[index].refresh = Refresh::IntervalMs(2000);

                if self.plugins[index].managed.is_some() {
                    self.send_managed_hello(index);
                    return;
                }

                let hello = Request::Hello {
                    major: PROTOCOL_MAJOR,
                    minor: PROTOCOL_MINOR,
                };
                self.send_to(index, &hello);
            }
            Err(err) => {
                self.plugins[index].io = None;
                let reason = format!("{err}");
                eprintln!(
                    "among: plugin `{}` could not be started: {reason}",
                    self.plugins[index].spec.name
                );
                self.plugins[index].state = PluginState::Missing { reason };
            }
        }
    }

    fn send_to(&mut self, index: usize, request: &Request) -> bool {
        let Some(io) = self.plugins[index].io.as_ref() else {
            return false;
        };
        match io.process.send(request) {
            Ok(()) => true,
            Err(error) => {
                let reason = match error {
                    SendError::Full => "plugin stdin queue exceeds 16 frames",
                    SendError::Fault(reason) => reason,
                };
                self.fault(index, reason);
                false
            }
        }
    }

    fn stop_as(&mut self, index: usize, final_state: PluginState, reason: &str) {
        self.revoke_notifications(index);
        stop_io(&mut self.plugins[index]);
        let name = &self.plugins[index].spec.name;
        self.results.retain(|result| &result.plugin != name);
        self.notices.retain(|(source, _, _)| source != name);
        if self.plugins[index].io.is_some() {
            self.plugins[index].after_stop = Some(final_state);
            self.plugins[index].state = PluginState::Stopping {
                reason: reason.into(),
                enabled: self.plugins[index].spec.enabled,
            };
        } else {
            self.plugins[index].state = final_state;
        }
    }

    fn fault(&mut self, index: usize, reason: &str) {
        if self.plugins[index].after_stop.is_some() {
            return;
        }
        self.stop_as(
            index,
            PluginState::Faulted {
                reason: reason.to_string(),
            },
            reason,
        );
        let attempts = self.plugins[index].restart_attempts;
        self.plugins[index].restart_at = (attempts < MAX_RESTARTS
            && self.plugins[index].spec.enabled)
            .then(|| Instant::now() + Duration::from_secs(2u64 << attempts));
    }

    /// Ask a plugin for a scope's content.
    pub fn request_status(&mut self, name: &str, scope: &str, context: &Context) -> Option<u64> {
        let index = self.find(name)?;
        if !self.plugins[index].state.is_healthy() {
            return None;
        }
        if self.plugins[index].managed.is_some() {
            return self.request_managed_status(index, scope);
        }
        if self.plugins[index].inflight.len() >= MAX_INFLIGHT
            || self.plugins[index]
                .scopes
                .values()
                .any(|pending| pending == scope)
        {
            return None;
        }
        let io = self.plugins[index].io.as_ref()?;
        // backpressure is not a plugin fault. check before cloning a possibly
        // large context, and acquire request authority only after queue acceptance.
        if !io.process.has_capacity() {
            return None;
        }
        let request_id = self.plugins[index].next_request_id;
        let next_id = request_id.checked_add(1)?;

        let request = Request::Status {
            request_id,
            scope: scope.to_string(),
            context: context.clone(),
        };
        match io.process.send(&request) {
            Ok(()) => {
                self.plugins[index].next_request_id = next_id;
                self.plugins[index]
                    .inflight
                    .insert(request_id, Instant::now());
                self.plugins[index]
                    .scopes
                    .insert(request_id, scope.to_string());
                Some(request_id)
            }
            Err(SendError::Full) => None,
            Err(SendError::Fault(reason)) => {
                self.fault(index, reason);
                None
            }
        }
    }

    /// Tell every healthy plugin about a terminal event.
    pub fn broadcast_event(&mut self, event: EventKind, pane: u64, detail: Option<String>) {
        let request = Request::Event {
            event,
            pane,
            detail,
        };
        for index in 0..self.plugins.len() {
            if self.plugins[index].state.is_healthy() {
                if self.plugins[index].managed.is_some() {
                    self.send_managed_event(index, &request);
                    continue;
                }
                self.send_to(index, &request);
            }
        }
    }

    /// Rejections recorded during the last [`PluginHost::poll`].
    pub fn rejections(&self) -> &[(String, Rejection)] {
        &self.last_rejections
    }

    /// Read whatever plugins have sent and reconcile timeouts.
    ///
    /// Called from the main loop. Any fault found here is contained: the plugin
    /// is stopped and marked, and its content is withdrawn by the caller via
    /// [`PluginHost::faulted_sources`].
    pub fn poll(&mut self) {
        self.last_rejections.clear();
        self.reconcile_notifications();

        let now = Instant::now();
        let count = self.plugins.len();
        let started = Instant::now();
        let mut processed = 0;
        let first = self.next_poll;
        self.next_poll = if count == 0 { 0 } else { (first + 1) % count };
        for offset in 0..count {
            let index = (first + offset) % count;
            if let Some(failure) = self.plugins[index]
                .io
                .as_ref()
                .and_then(|io| io.process.failure())
            {
                if self.plugins[index].after_stop.is_none() {
                    match failure {
                        Failure::Missing(reason) => self.stop_as(
                            index,
                            PluginState::Missing { reason },
                            "could not start plugin",
                        ),
                        Failure::Fault(reason) => self.fault(index, reason),
                    }
                }
            }
            if self.plugins[index]
                .io
                .as_ref()
                .is_some_and(|io| io.process.finished())
            {
                self.plugins[index].io = None;
                self.plugins[index].state =
                    self.plugins[index]
                        .after_stop
                        .take()
                        .unwrap_or_else(|| PluginState::Faulted {
                            reason: "plugin supervisor exited".into(),
                        });
                if self.plugins[index].state == PluginState::Disabled
                    && self.plugins[index].spec.enabled
                {
                    let name = self.plugins[index].spec.name.clone();
                    self.start_by_name(name);
                }
            }
            if matches!(self.plugins[index].state, PluginState::Stopping { .. }) {
                continue;
            }
            if self.plugins[index].io.is_none() {
                if self.plugins[index].state == PluginState::Waiting
                    && self.plugins[index].spec.enabled
                    && self.starts_allowed.get()
                {
                    let name = self.plugins[index].spec.name.clone();
                    self.start_by_name(name);
                } else {
                    self.restart_if_due(index, now);
                }
                continue;
            }

            for _ in 0..MAX_QUEUE_FRAMES {
                if processed >= MAX_POLL_FRAMES || started.elapsed() >= Duration::from_millis(5) {
                    break;
                }
                let line = self.plugins[index]
                    .io
                    .as_ref()
                    .and_then(|io| io.process.frames.try_recv().ok());
                let Some(line) = line else { break };
                processed += 1;
                self.handle_line(index, &line);
                if self.plugins[index].after_stop.is_some() {
                    break;
                }
            }

            if self.plugins[index].after_stop.is_some() {
                continue;
            }
            if self.plugins[index]
                .io
                .as_ref()
                .is_some_and(|io| !io.handshake_done && io.started.elapsed() >= REQUEST_TIMEOUT)
            {
                self.fault(
                    index,
                    "plugin did not complete handshake within five seconds",
                );
                continue;
            }

            // Expire requests the plugin never answered.
            let timed_out: Vec<u64> = self.plugins[index]
                .inflight
                .iter()
                .filter(|(_, sent_at)| now.duration_since(**sent_at) > REQUEST_TIMEOUT)
                .map(|(id, _)| *id)
                .collect();
            if !timed_out.is_empty() {
                for id in timed_out {
                    self.plugins[index].inflight.remove(&id);
                }
                self.fault(index, "plugin did not answer within the timeout");
            }
            self.configure_if_due(index);
            self.enumerate_if_due(index);
        }
        self.poll_notifications();
        self.plugins
            .retain(|plugin| !plugin.removed || plugin.io.is_some());
    }

    fn restart_if_due(&mut self, index: usize, now: Instant) {
        let due = self.plugins[index]
            .restart_at
            .map(|at| now >= at)
            .unwrap_or(false);
        if due
            && self.plugins[index].spec.enabled
            && self.starts_allowed.get()
            && self.plugins[index].restart_attempts < MAX_RESTARTS
        {
            self.plugins[index].restart_attempts += 1;
            let name = self.plugins[index].spec.name.clone();
            self.start_by_name(name);
        }
    }

    fn handle_line(&mut self, index: usize, line: &str) {
        if self.plugins[index].managed.is_some() {
            self.handle_managed_line(index, line);
            return;
        }
        let name = self.plugins[index].spec.name.clone();
        match protocol::parse_response(line) {
            Err(rejection) => {
                // Malformed output is discarded; the plugin keeps running so a
                // single bad line does not cost the user their status.
                self.last_rejections.push((name, rejection));
            }
            Ok(Response::Hello { major, minor }) => {
                self.plugins[index].observed = Some(super::manifest::Api { major, minor });
                if protocol::version_is_compatible(major, minor) {
                    if let Some(io) = self.plugins[index]
                        .io
                        .as_mut()
                        .filter(|io| io.process.started())
                    {
                        io.handshake_done = true;
                        self.plugins[index].state = PluginState::Running;
                    }
                } else {
                    self.stop_as(
                        index,
                        PluginState::Incompatible {
                            plugin_major: major,
                            host_major: PROTOCOL_MAJOR,
                        },
                        "incompatible plugin; waiting for process exit",
                    );
                    let rejection = Rejection::IncompatibleVersion {
                        plugin_major: major,
                        host_major: PROTOCOL_MAJOR,
                    };
                    // Reported once, at handshake: the per-poll rejection list
                    // is cleared on every tick, so a message kept only there
                    // would scroll past before anyone saw it.
                    eprintln!("among: plugin `{name}` was skipped: {rejection}");
                    self.last_rejections.push((name, rejection));
                }
            }
            Ok(response @ Response::Notify { .. }) => match protocol::validate_notice(response) {
                Ok(notice) if self.notices.len() < MAX_POLL_FRAMES => {
                    self.notices.push((name, notice.message, notice.style))
                }
                Ok(_) => self.fault(index, "plugin notification backlog exceeded"),
                Err(rejection) => self.last_rejections.push((name, rejection)),
            },
            Ok(Response::Error { message }) => {
                self.last_rejections.push((name, Rejection::Malformed));
                let _ = message;
            }
            Ok(response @ Response::Status { .. }) => match protocol::validate_status(response) {
                Ok(valid) => {
                    if self.plugins[index]
                        .inflight
                        .get(&valid.request_id)
                        .is_some_and(|sent| sent.elapsed() >= REQUEST_TIMEOUT)
                    {
                        self.fault(index, "plugin answered after the request deadline");
                        return;
                    }
                    if !self.plugins[index].state.is_healthy()
                        || self.plugins[index]
                            .inflight
                            .remove(&valid.request_id)
                            .is_none()
                    {
                        return;
                    }
                    let Some(scope) = self.plugins[index].scopes.remove(&valid.request_id) else {
                        return;
                    };
                    if self.results.len() >= MAX_POLL_FRAMES {
                        self.fault(index, "plugin result backlog exceeded");
                        return;
                    }
                    if let Some(interval) = valid.refresh_ms {
                        self.plugins[index].refresh = Refresh::IntervalMs(interval);
                    } else {
                        self.plugins[index].refresh = Refresh::OnEvent;
                    }
                    self.results.push(PluginResult {
                        plugin: name,
                        scope,
                        fragments: valid.fragments,
                        refresh: self.plugins[index].refresh,
                    });
                }
                Err(rejection) => self.last_rejections.push((name, rejection)),
            },
        }
    }

    /// Names of plugins currently in a faulted or incompatible state.
    ///
    /// The caller uses this to withdraw their content so a stale fragment is
    /// never left on screen.
    pub fn faulted_sources(&self) -> Vec<String> {
        self.plugins
            .iter()
            .filter(|p| !p.state.is_healthy())
            .map(|p| p.spec.name.clone())
            .collect()
    }

    /// Names of healthy plugins, in registration order.
    pub fn healthy(&self) -> Vec<String> {
        self.plugins
            .iter()
            .filter(|p| p.state.is_healthy())
            .map(|p| p.spec.name.clone())
            .collect()
    }

    /// Convert validated fragments from one plugin into slot content.
    pub fn apply_fragments(
        store: &mut SlotStore,
        scope: crate::slots::Scope,
        source: &str,
        fragments: &[IncomingFragment],
        refresh: Refresh,
    ) {
        let converted: Vec<Fragment> = fragments
            .iter()
            .map(|incoming| {
                Fragment {
                    meter: incoming.meter,
                    text: incoming.text.clone(),
                    // An icon the theme does not have is dropped rather than
                    // rendered as a broken placeholder.
                    icon: incoming
                        .icon
                        .clone()
                        .filter(|name| crate::slots::icon_exists(name)),
                    style: parse_style(incoming.style.as_deref()),
                    tooltip: incoming.tooltip.clone(),
                    priority: incoming.priority,
                    source: source.to_string(),
                }
            })
            .filter(|f| f.text.as_deref().is_some_and(|t| !t.is_empty()) || f.icon.is_some())
            .collect();

        store.set(
            scope,
            source,
            Contribution {
                source: source.to_string(),
                fragments: converted,
                refresh,
                last_asked_ms: None,
            },
        );
    }

    /// Convert a summary from one plugin into sidebar content.
    pub fn apply_summary(
        store: &mut SlotStore,
        workspace: usize,
        source: &str,
        summary: &IncomingSummary,
    ) {
        store.set_summary(
            workspace,
            Summary {
                icon: summary
                    .icon
                    .clone()
                    .filter(|name| crate::slots::icon_exists(name)),
                text: summary.text.clone(),
                tooltip: summary.tooltip.clone(),
                style: parse_style(summary.style.as_deref()),
                source: source.to_string(),
            },
        );
    }
}

/// Map a style name from a plugin onto the closed [`Style`] set.
///
/// Unknown names fall back to `Default` rather than failing the message, so a
/// typo in a plugin degrades appearance instead of losing the content.
pub fn parse_style(name: Option<&str>) -> Style {
    match name {
        Some("success") => Style::Success,
        Some("warning") => Style::Warning,
        Some("error") => Style::Error,
        Some("accent") => Style::Accent,
        Some("dim") => Style::Dim,
        _ => Style::Default,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::slots::Scope;

    /// Command line of the `fake-plugin` example in the given behaviour.
    ///
    /// `cargo test` builds examples into `<profile>/examples`, next to the
    /// `<profile>/deps` directory holding this test executable, so the plugin
    /// is found relative to the running test rather than through PATH.
    fn fake_plugin(behaviour: &str) -> Vec<String> {
        let test_executable = std::env::current_exe().expect("test executable path");
        let profile = test_executable
            .parent()
            .and_then(std::path::Path::parent)
            .expect("test executable lives in <profile>/deps");
        let plugin = profile.join("examples").join("fake-plugin");
        assert!(
            plugin.is_file(),
            "{} is missing: a plain `cargo test` builds it from examples/fake-plugin.rs, \
             a filtered run may not, so build it with `cargo build --example fake-plugin`",
            plugin.display()
        );
        vec![plugin.to_string_lossy().into_owned(), behaviour.into()]
    }

    /// A plugin that answers every status request with a fixed body.
    fn echo_plugin() -> Vec<String> {
        fake_plugin("echo")
    }

    /// A plugin that completes the handshake and never answers a status request.
    fn hanging_plugin() -> Vec<String> {
        fake_plugin("hanging")
    }

    /// A plugin that greets and dies immediately.
    fn crashing_plugin() -> Vec<String> {
        fake_plugin("crashing")
    }

    fn wait_for(host: &mut PluginHost, predicate: impl Fn(&PluginHost) -> bool) -> bool {
        for _ in 0..200 {
            host.poll();
            if predicate(host) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        false
    }

    fn spec(command: Vec<String>) -> PluginSpec {
        PluginSpec {
            name: "probe".into(),
            command,
            enabled: true,
        }
    }

    #[test]
    fn a_missing_program_is_reported_without_aborting() {
        let mut host = PluginHost::new();
        host.register(PluginSpec {
            name: "ghost".into(),
            command: vec!["/nonexistent/binary".into()],
            enabled: true,
        });
        assert!(wait_for(&mut host, |host| matches!(
            host.listing()[0].1,
            PluginState::Missing { .. }
        )));
        let (_, state) = host.listing().remove(0);
        assert!(
            matches!(state, PluginState::Missing { .. }),
            "got {state:?}"
        );
        assert!(!state.is_healthy());
    }

    #[test]
    fn a_disabled_plugin_is_not_started() {
        let mut host = PluginHost::new();
        host.register(PluginSpec {
            name: "off".into(),
            command: echo_plugin(),
            enabled: false,
        });
        assert_eq!(host.listing()[0].1, PluginState::Disabled);
    }

    #[test]
    fn an_empty_command_is_reported_as_missing() {
        let mut host = PluginHost::new();
        host.register(PluginSpec {
            name: "empty".into(),
            command: Vec::new(),
            enabled: true,
        });
        assert!(matches!(host.listing()[0].1, PluginState::Missing { .. }));
    }

    #[test]
    fn a_well_behaved_plugin_answers_a_status_request() {
        let mut host = PluginHost::new();
        host.register(spec(echo_plugin()));
        let healthy = wait_for(&mut host, |h| h.listing()[0].1.is_healthy());
        assert!(healthy, "plugin never became healthy");

        let context = Context {
            active_workspace: 0,
            workspace_count: 1,
            ..Default::default()
        };
        assert!(host
            .request_status("probe", "app_header", &context)
            .is_some());

        assert!(wait_for(&mut host, |host| !host.results.is_empty()));
        let result = host.drain_results();
        assert_eq!(result[0].scope, "app_header");
        assert_eq!(result[0].fragments[0].text.as_deref(), Some("ok"));
        assert!(
            host.rejections().is_empty(),
            "unexpected: {:?}",
            host.rejections()
        );
    }

    #[test]
    fn a_crash_is_contained_and_recorded_as_a_fault() {
        let mut host = PluginHost::new();
        host.register(spec(crashing_plugin()));
        let faulted = wait_for(&mut host, |h| {
            matches!(h.listing()[0].1, PluginState::Faulted { .. })
        });
        assert!(faulted, "a crashing plugin was still reported healthy");
        assert!(host.faulted_sources().contains(&"probe".to_string()));
    }

    #[test]
    fn a_hanging_plugin_is_faulted_rather_than_blocking() {
        let mut host = PluginHost::new();
        host.register(spec(hanging_plugin()));
        assert!(wait_for(&mut host, |h| h.listing()[0].1.is_healthy()));

        let context = Context::default();
        host.request_status("probe", "app_header", &context);

        // poll() must return promptly rather than waiting out the full timeout
        // inline, otherwise the interface would freeze.
        let start = Instant::now();
        host.poll();
        assert!(
            start.elapsed() < Duration::from_millis(500),
            "poll blocked for {:?}",
            start.elapsed()
        );
    }

    #[test]
    fn oversized_unterminated_stdout_is_contained_before_parsing() {
        let mut host = PluginHost::new();
        host.register(spec(fake_plugin("unterminated")));
        assert!(wait_for(&mut host, |host| matches!(
            &host.listing()[0].1,
            PluginState::Faulted { reason } if reason.contains("64 KiB")
        )));
        assert!(host.drain_results().is_empty());
    }

    #[test]
    fn a_stdout_flood_does_not_starve_a_second_plugin() {
        let mut host = PluginHost::new();
        host.register(spec(fake_plugin("flood")));
        host.register(PluginSpec {
            name: "healthy".into(),
            command: echo_plugin(),
            enabled: true,
        });
        assert!(wait_for(&mut host, |host| matches!(
            &host.listing()[0].1,
            PluginState::Faulted { reason } if reason.contains("queue")
        )));
        assert!(host
            .request_status("healthy", "app_header", &Context::default())
            .is_some());
        assert!(wait_for(&mut host, |host| host
            .results
            .iter()
            .any(|result| result.plugin == "healthy")));
        assert!(host.drain_notices().len() <= MAX_QUEUE_FRAMES);
    }

    #[test]
    fn disabling_stops_the_process_and_withdraws_it() {
        let mut host = PluginHost::new();
        host.register(spec(echo_plugin()));
        wait_for(&mut host, |h| h.listing()[0].1.is_healthy());
        host.disable("probe");
        assert!(matches!(host.listing()[0].1, PluginState::Stopping { .. }));
        assert!(wait_for(&mut host, |host| host.listing()[0].1
            == PluginState::Disabled));
        assert_eq!(host.listing()[0].1, PluginState::Disabled);
        assert!(host.faulted_sources().contains(&"probe".to_string()));
    }

    #[test]
    fn enabling_a_stopped_plugin_starts_it_again() {
        let mut host = PluginHost::new();
        host.register(spec(echo_plugin()));
        host.disable("probe");
        host.enable("probe");
        let healthy = wait_for(&mut host, |h| h.listing()[0].1.is_healthy());
        assert!(healthy);
    }

    #[test]
    fn rapid_toggles_wait_for_the_previous_generation_to_be_reaped() {
        let mut host = PluginHost::new();
        host.register(spec(fake_plugin("stubborn")));
        assert!(wait_for(&mut host, |host| host.listing()[0].1.is_healthy()));
        let old_pid = host.plugins[0].io.as_ref().unwrap().process.pid();
        let started = Instant::now();
        for _ in 0..100 {
            host.disable("probe");
            host.enable("probe");
            assert_eq!(host.plugins[0].io.as_ref().unwrap().process.pid(), old_pid);
        }
        assert!(started.elapsed() < Duration::from_millis(500));
        assert!(wait_for(&mut host, |host| host.listing()[0].1.is_healthy()));
        assert_ne!(host.plugins[0].io.as_ref().unwrap().process.pid(), old_pid);
        host.shutdown();
        assert!(wait_for(&mut host, |host| host.plugins[0].io.is_none()));
    }

    #[test]
    fn automatic_restarts_are_finite_and_an_explicit_off_on_resets_them() {
        let mut host = PluginHost::new();
        host.register(spec(crashing_plugin()));
        for attempt in 0..=MAX_RESTARTS {
            assert!(wait_for(&mut host, |host| host.plugins[0].io.is_none()));
            assert_eq!(host.plugins[0].restart_attempts, attempt);
            if attempt == MAX_RESTARTS {
                assert!(host.plugins[0].restart_at.is_none());
            } else {
                let due = host.plugins[0].restart_at.unwrap();
                let expected = Duration::from_secs(2u64 << attempt);
                assert!(due > Instant::now());
                assert!(due.duration_since(Instant::now()) <= expected);
                host.restart_if_due(0, due);
            }
        }
        for _ in 0..100 {
            host.poll();
        }
        assert!(host.plugins[0].io.is_none());
        host.disable("probe");
        host.enable("probe");
        assert_eq!(host.plugins[0].restart_attempts, 0);
        assert!(host.plugins[0].io.is_some());
        host.shutdown();
    }

    #[test]
    fn outstanding_requests_are_bounded_and_same_scope_is_coalesced() {
        let mut host = PluginHost::new();
        host.register(spec(hanging_plugin()));
        assert!(wait_for(&mut host, |host| host.listing()[0].1.is_healthy()));
        for index in 0..MAX_INFLIGHT {
            let scope = format!("pane_header:{index}");
            assert!(host
                .request_status("probe", &scope, &Context::default())
                .is_some());
            for _ in 0..100 {
                assert!(host
                    .request_status("probe", &scope, &Context::default())
                    .is_none());
            }
        }
        assert!(host
            .request_status("probe", "app_header", &Context::default())
            .is_none());
        assert_eq!(host.plugins[0].inflight.len(), MAX_INFLIGHT);
        assert_eq!(host.plugins[0].scopes.len(), MAX_INFLIGHT);
        host.shutdown();
    }

    #[test]
    fn ordinary_eight_pane_collection_bursts_defer_without_faulting() {
        let mut host = PluginHost::new();
        host.register(spec(fake_plugin("delayed")));
        assert!(wait_for(&mut host, |host| host.plugins[0]
            .state
            .is_healthy()));
        let mut scopes = vec![
            "app_header".to_string(),
            "app_footer".into(),
            "workspace_header".into(),
            "workspace_summary:0".into(),
        ];
        for pane in 1..=8 {
            scopes.push(format!("pane_header:{pane}"));
            scopes.push(format!("pane_footer:{pane}"));
        }
        let mut requested = std::collections::BTreeSet::new();
        let mut answered = std::collections::BTreeSet::new();
        let deadline = Instant::now() + Duration::from_secs(5);
        while answered.len() < scopes.len() {
            host.poll();
            for result in host.drain_results() {
                answered.insert(result.scope);
            }
            // no sleeps or polling between requests in this real GUI-sized burst.
            for scope in &scopes {
                if !requested.contains(scope)
                    && host
                        .request_status("probe", scope, &Context::default())
                        .is_some()
                {
                    requested.insert(scope.clone());
                }
            }
            assert!(
                host.plugins[0].state.is_healthy(),
                "{:?}",
                host.plugins[0].state
            );
            assert_eq!(host.plugins[0].restart_attempts, 0);
            assert!(host.plugins[0].inflight.len() <= MAX_INFLIGHT);
            assert!(
                Instant::now() < deadline,
                "deferred scopes were never collected"
            );
            std::thread::sleep(Duration::from_millis(25));
        }
        assert_eq!(answered.len(), 20);
        host.shutdown();
    }

    #[test]
    fn a_full_input_queue_creates_no_status_request_authority_or_fault() {
        let mut host = PluginHost::new();
        host.register(spec(fake_plugin("never-read")));
        assert!(wait_for(&mut host, |host| host.plugins[0]
            .state
            .is_healthy()));
        let event = Request::Event {
            event: EventKind::WorkingDirectoryChanged,
            pane: 1,
            detail: Some("x".repeat(60 * 1024)),
        };
        let mut full = false;
        for _ in 0..64 {
            match host.plugins[0].io.as_ref().unwrap().process.send(&event) {
                Ok(()) => {}
                Err(SendError::Full) => {
                    full = true;
                    break;
                }
                Err(error) => panic!("unexpected fixture failure {error:?}"),
            }
        }
        assert!(full);
        let next_id = host.plugins[0].next_request_id;
        assert!(host
            .request_status("probe", "app_header", &Context::default())
            .is_none());
        assert_eq!(host.plugins[0].next_request_id, next_id);
        assert!(host.plugins[0].inflight.is_empty());
        assert!(host.plugins[0].scopes.is_empty());
        assert!(host.plugins[0].state.is_healthy());
        assert_eq!(host.plugins[0].restart_attempts, 0);
        host.shutdown();
    }

    #[test]
    fn a_closed_application_gate_preserves_desired_state_until_starts_are_safe() {
        let gate = Rc::new(Cell::new(false));
        let mut host = PluginHost::with_start_gate(gate.clone());
        host.register(spec(echo_plugin()));
        for _ in 0..20 {
            host.poll();
        }
        assert_eq!(host.plugins[0].state, PluginState::Waiting);
        assert!(host.plugins[0].spec.enabled);
        assert!(host.is_stopped());
        assert_eq!(host.plugins[0].restart_attempts, 0);
        gate.set(true);
        assert!(wait_for(&mut host, |host| host.plugins[0]
            .state
            .is_healthy()));
        host.shutdown();
        assert!(wait_for(&mut host, |host| host.is_stopped()));
    }

    #[test]
    fn process_capacity_includes_generations_still_stopping() {
        let mut host = PluginHost::new();
        for index in 0..MAX_PROCESSES {
            host.register(PluginSpec {
                name: format!("p{index}"),
                command: hanging_plugin(),
                enabled: true,
            });
        }
        assert!(wait_for(&mut host, |host| host
            .plugins
            .iter()
            .all(|plugin| plugin.state.is_healthy())));
        host.disable("p0");
        host.register(PluginSpec {
            name: "excess".into(),
            command: echo_plugin(),
            enabled: true,
        });
        assert!(
            matches!(&host.plugins[MAX_PROCESSES].state, PluginState::Faulted { reason } if reason.contains("32"))
        );
        assert_eq!(
            host.plugins
                .iter()
                .filter(|plugin| plugin.io.is_some())
                .count(),
            MAX_PROCESSES
        );
        let start = Instant::now();
        host.shutdown();
        assert!(start.elapsed() < Duration::from_millis(500));
        assert!(wait_for(&mut host, |host| host
            .plugins
            .iter()
            .all(|plugin| plugin.io.is_none())));
    }

    #[test]
    fn request_expiry_clears_authority_before_late_output() {
        let mut host = PluginHost::new();
        host.register(spec(hanging_plugin()));
        assert!(wait_for(&mut host, |host| host.listing()[0].1.is_healthy()));
        let request_id = host
            .request_status("probe", "app_header", &Context::default())
            .unwrap();
        host.plugins[0].inflight.insert(
            request_id,
            Instant::now() - REQUEST_TIMEOUT - Duration::from_millis(1),
        );
        host.poll();
        assert!(host.plugins[0].inflight.is_empty());
        assert!(host.plugins[0].scopes.is_empty());
        host.handle_line(
            0,
            &format!(
                r#"{{"type":"status","request_id":{request_id},"fragments":[{{"text":"late"}}]}}"#
            ),
        );
        assert!(host.results.is_empty());
        host.shutdown();
    }

    #[test]
    fn an_already_queued_answer_cannot_bypass_the_request_deadline() {
        let mut host = PluginHost::new();
        host.register(spec(echo_plugin()));
        assert!(wait_for(&mut host, |host| host.listing()[0].1.is_healthy()));
        let id = host
            .request_status("probe", "app_header", &Context::default())
            .unwrap();
        let io = host.plugins[0].io.as_ref().unwrap();
        let reply = io
            .process
            .frames
            .recv_timeout(Duration::from_secs(2))
            .unwrap();
        host.plugins[0]
            .inflight
            .insert(id, Instant::now() - REQUEST_TIMEOUT);
        host.handle_line(0, &reply);
        assert!(host.results.is_empty());
        assert!(
            matches!(&host.plugins[0].state, PluginState::Stopping { reason, .. } if reason.contains("deadline"))
        );
        assert!(host.plugins[0].inflight.is_empty());
        host.shutdown();
    }

    #[test]
    fn missing_commands_never_enter_the_automatic_restart_loop() {
        let mut host = PluginHost::new();
        host.register(spec(Vec::new()));
        host.disable("probe");
        host.enable("probe");
        for _ in 0..100 {
            host.poll();
        }
        assert!(matches!(host.plugins[0].state, PluginState::Missing { .. }));
        assert_eq!(host.plugins[0].restart_attempts, 0);
        assert!(host.plugins[0].restart_at.is_none());
    }

    #[test]
    fn unsolicited_duplicate_and_previous_generation_replies_do_not_render() {
        let mut host = PluginHost::new();
        host.register(spec(fake_plugin("unmatched")));
        assert!(wait_for(&mut host, |host| host.listing()[0].1.is_healthy()));
        for _ in 0..5 {
            host.poll();
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(host.drain_results().is_empty());
        let old_id = host
            .request_status("probe", "app_header", &Context::default())
            .unwrap();
        assert!(wait_for(&mut host, |host| !host.results.is_empty()));
        host.disable("probe");
        assert!(
            host.drain_results().is_empty(),
            "disabled generation retained a result"
        );
        host.plugins[0].spec.command = fake_plugin("duplicate");
        host.enable("probe");
        assert!(wait_for(&mut host, |host| host.listing()[0].1.is_healthy()));
        let new_id = host
            .request_status("probe", "app_header", &Context::default())
            .unwrap();
        assert!(new_id > old_id);
        host.handle_line(
            0,
            &format!(
                r#"{{"type":"status","request_id":{old_id},"fragments":[{{"text":"stale"}}]}}"#
            ),
        );
        assert!(wait_for(&mut host, |host| !host.results.is_empty()));
        for _ in 0..5 {
            host.poll();
            std::thread::sleep(Duration::from_millis(20));
        }
        let results = host.drain_results();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].fragments[0].text.as_deref(), Some("once"));
        assert!(host.plugins[0].inflight.is_empty());
        host.shutdown();
    }

    #[test]
    fn malformed_output_is_rejected_without_killing_the_plugin() {
        let mut host = PluginHost::new();
        // The plugin stays alive after the bad line, so it is the only fault.
        host.register(spec(fake_plugin("malformed")));
        // a preliminary health poll can consume the bad line; the next poll
        // clears that batch's rejections, so observe them in this loop only.
        for _ in 0..40 {
            host.poll();
            if !host.rejections().is_empty() {
                break;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        assert_eq!(
            host.rejections(),
            &[("probe".into(), Rejection::Malformed)],
            "malformed line was not rejected"
        );
        // The plugin survives its own bad output.
        assert!(host.listing()[0].1.is_healthy());

        // a running state alone cannot prove that the child still responds.
        assert!(host
            .request_status("probe", "app_header", &Context::default())
            .is_some());
        assert!(
            wait_for(&mut host, |h| !h.results.is_empty()),
            "plugin did not answer after its malformed output"
        );
        let results = host.drain_results();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].plugin, "probe");
        assert_eq!(results[0].scope, "app_header");
        assert_eq!(results[0].fragments.len(), 1);
        assert_eq!(results[0].fragments[0].text.as_deref(), Some("ok"));
        assert!(host.rejections().is_empty());
        assert!(host.listing()[0].1.is_healthy());
    }

    #[test]
    fn fragments_are_converted_and_unknown_icons_dropped() {
        let mut store = SlotStore::new();
        let fragments = vec![IncomingFragment {
            meter: None,
            text: Some("main".into()),
            icon: Some("definitely-not-a-real-icon-name".into()),
            style: Some("success".into()),
            tooltip: None,
            priority: 5,
        }];
        PluginHost::apply_fragments(
            &mut store,
            Scope::AppHeader,
            "git",
            &fragments,
            Refresh::OnEvent,
        );
        let visible = store.fragments(Scope::AppHeader);
        assert_eq!(visible.len(), 1);
        assert_eq!(visible[0].style, Style::Success);
        assert_eq!(visible[0].icon, None, "unknown icon should be dropped");
    }

    #[test]
    fn an_unknown_style_degrades_instead_of_failing() {
        assert_eq!(parse_style(Some("chartreuse")), Style::Default);
        assert_eq!(parse_style(None), Style::Default);
        assert_eq!(parse_style(Some("error")), Style::Error);
    }
}
