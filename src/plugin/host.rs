//! Launching plugins, talking to them, and surviving their failures.

pub use super::admission::Admission;
use super::admission::Busy;
use super::process::{Failure, Process, SendError};
use super::transport::{MAX_POLL_FRAMES, MAX_QUEUE_FRAMES};
use std::cell::Cell;
use std::collections::{BTreeSet, HashMap};
use std::rc::Rc;
use std::time::Duration;

use super::inventory;
use super::managed;
use super::protocol::{
    self, Context, EventKind, IncomingFragment, IncomingSummary, Rejection, Request, Response,
};
use crate::slots::{Contribution, Fragment, Refresh, SlotStore, Style, Summary};

/// Shown instead of running a generation that may run only in development mode.
pub const DEVELOPMENT_ONLY: &str =
    "development install: it starts only when Among is started with --plugin-development";

mod environment;
mod managed_runtime;
pub use managed_runtime::Configuration;
mod overview;
pub use overview::Overview;
#[cfg(test)]
mod fixtures;

/// How long a plugin has to answer before it is treated as faulted.
///
/// Generous enough for a slow `git status` on a cold cache, short enough that a
/// wedged plugin is noticed rather than silently poisoning a slot forever.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);
#[cfg(test)]
use super::admission::MAX_PROCESSES;
const MAX_RESTARTS: u32 = 3;
// one reply per accepted status request must fit the bounded stdout queue even
// if the plugin answers every request before GTK next gets a chance to poll.
const MAX_INFLIGHT: usize = MAX_QUEUE_FRAMES;

/// Input for explicitly constructed managed test packages, never discovery.
#[cfg(test)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginSpec {
    pub name: String,
    /// The command to run; `argv[0]` is the program.
    pub command: Vec<String>,
    /// Whether the plugin starts with the application.
    pub enabled: bool,
}

struct Registration {
    name: String,
    enabled: bool,
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
    package_version: Option<String>,
}

/// One plugin under management.
struct PluginEntry {
    spec: Registration,
    state: PluginState,
    io: Option<PluginIo>,
    /// Requests sent and not yet answered, for timeout accounting.
    inflight: HashMap<u64, Instant>,
    /// The scope each outstanding request asked about.
    scopes: HashMap<u64, StatusRequest>,
    next_request_id: u64,
    /// The refresh policy the plugin last declared.
    refresh: Refresh,
    restart_at: Option<Instant>,
    restart_attempts: u32,
    after_stop: Option<PluginState>,
    stopping_reason: Option<String>,
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
    admission: Admission,
    world: managed::World,
    workspace_revision: u64,
}

/// The row order an outstanding status request was asked under. The managed
/// runtime resolves the answer's scope from its own target; this only lets a
/// reorder withdraw answers addressed to the old order.
struct StatusRequest {
    workspace_revision: u64,
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
    workspace_revision: u64,
}

fn stop_io(plugin: &mut PluginEntry) {
    if let Some(io) = plugin.io.as_ref() {
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

impl PluginHost {
    pub fn new() -> Self {
        Self::with_admission(Admission::default())
    }

    #[cfg(test)]
    pub fn with_start_gate(starts_allowed: Rc<Cell<bool>>) -> Self {
        let mut host = Self::new();
        host.starts_allowed = starts_allowed;
        host
    }

    pub fn with_admission(admission: Admission) -> Self {
        Self {
            plugins: Vec::new(),
            last_rejections: Vec::new(),
            results: Vec::new(),
            notices: Vec::new(),
            invalidated_sources: Default::default(),
            next_poll: 0,
            starts_allowed: Rc::new(Cell::new(true)),
            admission,
            world: managed::World {
                session: glib::uuid_string_random().to_string(),
                ..Default::default()
            },
            workspace_revision: 0,
        }
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
        // replies can already be queued when a reorder commits. filtering only
        // at receipt would give such an answer authority over its old row index.
        std::mem::take(&mut self.results)
            .into_iter()
            .filter(|result| result.workspace_revision == self.workspace_revision)
            .collect()
    }

    #[cfg(test)]
    pub(crate) fn status_pending(&self, name: &str, request_id: u64) -> bool {
        self.find(name)
            .is_some_and(|index| self.plugins[index].inflight.contains_key(&request_id))
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
        self.plugins.push(PluginEntry {
            spec: Registration {
                name: name.clone(),
                enabled: desired,
            },
            state: PluginState::Disabled,
            io: None,
            inflight: HashMap::new(),
            scopes: HashMap::new(),
            next_request_id: 1,
            refresh: Refresh::IntervalMs(2000),
            restart_at: None,
            restart_attempts: 0,
            after_stop: None,
            stopping_reason: None,
            managed: Some(entry),
            settings: local.and_then(|local| local.get("settings")).cloned(),
            runtime: Some(managed_runtime::Runtime::new(
                local.and_then(|local| local.get("review")).cloned(),
            )),
            observed: None,
            removing,
            removed: false,
        });
        self.start_by_name(name);
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
        !self.admission.held(key)
            && self
                .find(key)
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
        if self.world.workspaces != workspaces {
            self.workspace_revision = self.workspace_revision.wrapping_add(1);
        }
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

    #[cfg(test)]
    pub fn supervised_identities(&self) -> BTreeSet<String> {
        self.plugins
            .iter()
            .filter(|plugin| plugin.io.is_some())
            .map(|plugin| plugin.spec.name.clone())
            .collect()
    }

    #[cfg(test)]
    pub(crate) fn hold_retirement(
        &self,
        key: &str,
        errno: Option<i32>,
    ) -> Option<super::process::ProofHold> {
        self.find(key)
            .and_then(|index| self.plugins[index].io.as_ref())
            .map(|io| io.process.hold_group_proof(errno))
    }

    /// Spawn a plugin process.
    ///
    /// Failure to start is not an error for the host: it is recorded as the
    /// plugin's state and every other plugin continues.
    /// a deferred admission is not a crash-restart attempt.
    fn start_by_name(&mut self, name: String) -> bool {
        let Some(index) = self.find(&name) else {
            return false;
        };
        if !self.plugins[index].spec.enabled
            || self.plugins[index].io.is_some()
            || self.plugins[index].removing
        {
            return false;
        }
        // no raw command or filename can substitute for an inspected receipt.
        // keep unavailable managed selections visible, but never launch a
        // manifestless process to discover whether it might speak our protocol.
        if self.plugins[index].managed.is_none() {
            self.plugins[index].state = PluginState::Unavailable {
                reason: "a validated managed installation is required before execution".into(),
            };
            self.plugins[index].restart_at = None;
            return false;
        }
        {
            if self.plugins[index]
                .managed
                .as_ref()
                .is_some_and(|entry| entry.removal_pending())
            {
                self.plugins[index].state = PluginState::Unavailable {
                    reason: "uninstall is still active for this installation".into(),
                };
                self.plugins[index].restart_at = None;
                return false;
            }
            // Every launch and restart passes here, so a development install
            // stays listed in an ordinary run but its code never starts, even
            // when its switch was left on in a development run.
            if self.plugins[index]
                .managed
                .as_ref()
                .is_some_and(|entry| entry.receipt.requires_development())
                && !crate::plugin::development::enabled()
            {
                self.plugins[index].state = PluginState::Unavailable {
                    reason: DEVELOPMENT_ONLY.into(),
                };
                self.plugins[index].restart_at = None;
                return false;
            }
            if let Err(problem) = self.prepare_managed(index) {
                self.plugins[index].state = PluginState::Unavailable {
                    reason: problem.to_string(),
                };
                self.plugins[index].restart_at = None;
                return false;
            }
        }
        if !self.starts_allowed.get() {
            self.plugins[index].state = PluginState::Waiting;
            self.plugins[index].restart_at = None;
            return false;
        }
        self.plugins[index].restart_at = None;

        let mut program = if let Some(entry) = &self.plugins[index].managed {
            match entry.executable_files() {
                Ok((directory, executable)) => {
                    super::process::pinned_program(directory, executable, entry.generation_lease())
                }
                Err(problem) => {
                    self.plugins[index].state = PluginState::Unavailable {
                        reason: problem.to_string(),
                    };
                    return false;
                }
            }
        } else {
            return false;
        };
        if let Err(reason) = environment::configure(&mut program) {
            self.plugins[index].state = PluginState::Unavailable {
                reason: reason.into(),
            };
            return false;
        }
        let reservation = match self.admission.acquire(&self.plugins[index].spec.name) {
            Ok(reservation) => reservation,
            Err(busy) => {
                self.plugins[index].state = match busy {
                    Busy::Identity(reason) => PluginState::Stopping {
                        reason: format!("previous generation: {reason}"),
                        enabled: self.plugins[index].spec.enabled,
                    },
                    // capacity is temporary across windows, not a plugin crash.
                    // poll retries this identity while enabled, without a new
                    // generation until an actual shared slot is available.
                    Busy::Capacity | Busy::Borrowed => PluginState::Waiting,
                };
                self.plugins[index].restart_at = None;
                return false;
            }
        };
        let spawned = Process::start_admitted(program, reservation);

        match spawned {
            Ok(process) => {
                self.plugins[index].io = Some(PluginIo {
                    process,
                    started: Instant::now(),
                    handshake_done: false,
                    generation: glib::uuid_string_random().to_string(),
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

                self.send_managed_hello(index);
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
        true
    }

    fn stop_as(&mut self, index: usize, final_state: PluginState, reason: &str) {
        stop_io(&mut self.plugins[index]);
        let name = &self.plugins[index].spec.name;
        self.results.retain(|result| &result.plugin != name);
        self.notices.retain(|(source, _, _)| source != name);
        if self.plugins[index].io.is_some() || self.admission.held(name) {
            self.plugins[index].after_stop = Some(final_state);
            self.plugins[index].stopping_reason = Some(reason.into());
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
    pub fn request_status(&mut self, name: &str, scope: &str) -> Option<u64> {
        let index = self.find(name)?;
        if !self.plugins[index].state.is_healthy() {
            return None;
        }
        self.request_managed_status(index, scope)
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
                self.send_managed_event(index, &request);
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
                self.plugins[index].stopping_reason = None;
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
                if let Some(reason) = self.plugins[index]
                    .io
                    .as_ref()
                    .and_then(|io| io.process.retirement_reason())
                {
                    let cause = self.plugins[index]
                        .stopping_reason
                        .as_deref()
                        .unwrap_or("stopping");
                    let reason = format!("{cause}; {reason}");
                    if let PluginState::Stopping {
                        reason: current, ..
                    } = &mut self.plugins[index].state
                    {
                        *current = reason;
                    }
                } else if self.plugins[index].io.is_none() {
                    let name = self.plugins[index].spec.name.clone();
                    if let Some(reason) = self.admission.reason(&name) {
                        if let PluginState::Stopping {
                            reason: current, ..
                        } = &mut self.plugins[index].state
                        {
                            *current = format!("previous generation: {reason}");
                        }
                    } else {
                        self.plugins[index].state = self.plugins[index]
                            .after_stop
                            .take()
                            .unwrap_or(PluginState::Disabled);
                    }
                    if self.plugins[index].spec.enabled {
                        if self.plugins[index].restart_at.is_some() {
                            self.restart_if_due(index, now);
                        } else {
                            self.start_by_name(name);
                        }
                    }
                }
                continue;
            }
            if self.plugins[index].io.is_none() {
                if self.plugins[index].state == PluginState::Waiting
                    && self.plugins[index].spec.enabled
                    && self.starts_allowed.get()
                {
                    if self.plugins[index].restart_at.is_some() {
                        self.restart_if_due(index, now);
                    } else {
                        let name = self.plugins[index].spec.name.clone();
                        self.start_by_name(name);
                    }
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
            let retry_at = self.plugins[index].restart_at;
            let attempts = self.plugins[index].restart_attempts;
            self.plugins[index].restart_attempts += 1;
            let name = self.plugins[index].spec.name.clone();
            if !self.start_by_name(name) {
                self.plugins[index].restart_attempts = attempts;
                if matches!(
                    self.plugins[index].state,
                    PluginState::Waiting | PluginState::Stopping { .. }
                ) {
                    // keep a deferred restart distinct from a first startup:
                    // charge it exactly once when spawning becomes possible.
                    self.plugins[index].restart_at = retry_at;
                }
            }
        }
    }

    fn handle_line(&mut self, index: usize, line: &str) {
        self.handle_managed_line(index, line);
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
    const KEY: &str = "github:12/probe";

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
            name: KEY.into(),
            command,
            enabled: true,
        }
    }

    #[test]
    fn a_missing_program_is_reported_without_aborting() {
        let mut host = PluginHost::new();
        host.register_fixture(PluginSpec {
            name: "github:12/ghost".into(),
            command: vec!["/nonexistent/binary".into()],
            enabled: true,
        });
        assert!(wait_for(&mut host, |host| matches!(
            host.listing()[0].1,
            PluginState::Unavailable { .. }
        )));
        let (_, state) = host.listing().remove(0);
        assert!(
            matches!(state, PluginState::Unavailable { .. }),
            "got {state:?}"
        );
        assert!(!state.is_healthy());
    }

    #[test]
    fn a_disabled_plugin_is_not_started() {
        let mut host = PluginHost::new();
        host.register_fixture(PluginSpec {
            name: "github:12/off".into(),
            command: echo_plugin(),
            enabled: false,
        });
        assert_eq!(host.listing()[0].1, PluginState::Disabled);
    }

    #[test]
    fn a_managed_receipt_with_no_entrypoint_is_unavailable_without_execution() {
        let mut host = PluginHost::new();
        host.register_fixture(PluginSpec {
            name: "github:12/empty".into(),
            command: Vec::new(),
            enabled: true,
        });
        assert!(matches!(
            host.listing()[0].1,
            PluginState::Unavailable { .. }
        ));
        assert_eq!(host.supervised_count(), 0);
    }

    #[test]
    fn a_well_behaved_plugin_answers_a_status_request() {
        let mut host = PluginHost::new();
        host.register_fixture(spec(echo_plugin()));
        let healthy = wait_for(&mut host, |h| h.listing()[0].1.is_healthy());
        assert!(healthy, "plugin never became healthy");

        assert!(host.request_status(KEY, "app_header").is_some());

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
        host.register_fixture(spec(crashing_plugin()));
        let faulted = wait_for(&mut host, |h| {
            matches!(h.listing()[0].1, PluginState::Faulted { .. })
        });
        assert!(faulted, "a crashing plugin was still reported healthy");
        assert!(host.faulted_sources().contains(&KEY.to_string()));
    }

    #[test]
    fn a_hanging_plugin_is_faulted_rather_than_blocking() {
        let mut host = PluginHost::new();
        host.register_fixture(spec(hanging_plugin()));
        assert!(wait_for(&mut host, |h| h.listing()[0].1.is_healthy()));

        host.request_status(KEY, "app_header");

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
        host.register_fixture(spec(fake_plugin("unterminated")));
        assert!(wait_for(&mut host, |host| matches!(
            &host.listing()[0].1,
            PluginState::Faulted { reason } if reason.contains("64 KiB")
        )));
        assert!(host.drain_results().is_empty());
    }

    #[test]
    fn a_stdout_flood_does_not_starve_a_second_plugin() {
        let mut host = PluginHost::new();
        host.register_fixture(spec(fake_plugin("flood")));
        host.register_fixture(PluginSpec {
            name: "github:12/healthy".into(),
            command: echo_plugin(),
            enabled: true,
        });
        assert!(wait_for(&mut host, |host| matches!(
            &host.listing()[0].1,
            PluginState::Faulted { reason } if reason.contains("queue")
        )));
        assert!(host
            .request_status("github:12/healthy", "app_header")
            .is_some());
        assert!(wait_for(&mut host, |host| host
            .results
            .iter()
            .any(|result| result.plugin == "github:12/healthy")));
        assert!(host.drain_notices().len() <= MAX_QUEUE_FRAMES);
    }

    #[test]
    fn disabling_stops_the_process_and_withdraws_it() {
        let mut host = PluginHost::new();
        host.register_fixture(spec(echo_plugin()));
        wait_for(&mut host, |h| h.listing()[0].1.is_healthy());
        host.disable(KEY);
        assert!(matches!(host.listing()[0].1, PluginState::Stopping { .. }));
        assert!(wait_for(&mut host, |host| host.listing()[0].1
            == PluginState::Disabled));
        assert_eq!(host.listing()[0].1, PluginState::Disabled);
        assert!(host.faulted_sources().contains(&KEY.to_string()));
    }

    #[test]
    fn enabling_a_stopped_plugin_starts_it_again() {
        let mut host = PluginHost::new();
        host.register_fixture(spec(echo_plugin()));
        host.disable(KEY);
        host.enable(KEY);
        let healthy = wait_for(&mut host, |h| h.listing()[0].1.is_healthy());
        assert!(healthy);
    }

    #[test]
    fn rapid_toggles_wait_for_the_previous_generation_to_be_reaped() {
        let mut host = PluginHost::new();
        host.register_fixture(spec(fake_plugin("stubborn")));
        assert!(wait_for(&mut host, |host| host.listing()[0].1.is_healthy()));
        let old_pid = host.plugins[0].io.as_ref().unwrap().process.pid();
        let started = Instant::now();
        for _ in 0..100 {
            host.disable(KEY);
            host.enable(KEY);
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
        host.register_fixture(spec(crashing_plugin()));
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
        host.disable(KEY);
        host.enable(KEY);
        assert_eq!(host.plugins[0].restart_attempts, 0);
        assert!(host.plugins[0].io.is_some());
        host.shutdown();
    }

    #[test]
    fn outstanding_requests_are_bounded_and_same_scope_is_coalesced() {
        let mut host = PluginHost::new();
        host.register_fixture(spec(hanging_plugin()));
        assert!(wait_for(&mut host, |host| host.listing()[0].1.is_healthy()));
        // managed transport reserves one frame for its enumeration exchange.
        for index in 1..MAX_INFLIGHT {
            let scope = format!("pane_header:{index}");
            assert!(host.request_status(KEY, &scope).is_some());
            for _ in 0..100 {
                assert!(host.request_status(KEY, &scope).is_none());
            }
        }
        assert!(host.request_status(KEY, "app_header").is_none());
        assert_eq!(host.plugins[0].inflight.len(), MAX_INFLIGHT - 1);
        assert_eq!(host.plugins[0].scopes.len(), MAX_INFLIGHT - 1);
        host.shutdown();
    }

    #[test]
    fn ordinary_eight_pane_collection_bursts_defer_without_faulting() {
        let mut host = PluginHost::new();
        host.register_fixture(spec(fake_plugin("delayed")));
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
                if !requested.contains(scope) && host.request_status(KEY, scope).is_some() {
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
        host.register_fixture(spec(fake_plugin("never-read")));
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
        assert!(host.request_status(KEY, "app_header").is_none());
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
        host.register_fixture(spec(echo_plugin()));
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
            host.register_fixture(PluginSpec {
                name: format!("github:12/p{index}"),
                command: hanging_plugin(),
                enabled: true,
            });
        }
        assert!(wait_for(&mut host, |host| host
            .plugins
            .iter()
            .all(|plugin| plugin.state.is_healthy())));
        host.disable("github:12/p0");
        host.register_fixture(PluginSpec {
            name: "github:12/excess".into(),
            command: echo_plugin(),
            enabled: true,
        });
        assert_eq!(host.plugins[MAX_PROCESSES].state, PluginState::Waiting);
        assert_eq!(
            host.plugins
                .iter()
                .filter(|plugin| plugin.io.is_some())
                .count(),
            MAX_PROCESSES
        );
        assert!(wait_for(&mut host, |host| host.plugins[MAX_PROCESSES]
            .state
            .is_healthy()));
        assert_eq!(host.plugins[MAX_PROCESSES].restart_attempts, 0);
        assert_eq!(host.supervised_count(), MAX_PROCESSES);
        let start = Instant::now();
        host.shutdown();
        assert!(start.elapsed() < Duration::from_millis(500));
        assert!(wait_for(&mut host, |host| host
            .plugins
            .iter()
            .all(|plugin| plugin.io.is_none())));
    }

    #[test]
    fn unresolved_identity_retirement_does_not_block_unrelated_replacement_window_plugins() {
        let admission = Admission::default();
        let mut retiring = PluginHost::with_admission(admission.clone());
        retiring.register_fixture(spec(fake_plugin("stubborn")));
        assert!(wait_for(&mut retiring, |host| host.plugins[0]
            .state
            .is_healthy()));
        let hold = retiring.hold_retirement(KEY, Some(libc::EPERM)).unwrap();
        retiring.shutdown();
        let deadline = Instant::now() + Duration::from_secs(5);
        while hold.observations() < 2 {
            retiring.poll();
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(!retiring.is_stopped());
        assert!(!retiring.supervised_stopped(KEY));
        let mut replacement = PluginHost::with_admission(admission);
        replacement.register_fixture(spec(echo_plugin()));
        replacement.register_fixture(PluginSpec {
            name: "github:12/unrelated".into(),
            command: echo_plugin(),
            enabled: true,
        });
        assert!(wait_for(&mut replacement, |host| host.plugins[1]
            .state
            .is_healthy()));
        assert!(
            matches!(&replacement.plugins[0].state, PluginState::Stopping { reason, .. } if reason.contains("permission denied"))
        );
        assert!(
            replacement.plugins[0].io.is_none(),
            "same identity started before owned retirement proof"
        );
        assert!(
            !replacement.supervised_stopped(KEY),
            "replacement window falsely acknowledged the old identity's stop"
        );
        assert_eq!(
            retiring.supervised_count() + replacement.supervised_count(),
            2
        );
        replacement.disable(KEY);
        assert!(matches!(
            replacement.plugins[0].state,
            PluginState::Stopping { .. }
        ));
        assert!(
            !replacement.supervised_stopped(KEY),
            "disable without local IO is not a global stop acknowledgement"
        );
        replacement.enable(KEY);
        assert!(replacement.plugins[0].io.is_none());
        drop(hold);
        assert!(wait_for(&mut retiring, PluginHost::is_stopped));
        assert!(wait_for(&mut replacement, |host| host.plugins[0]
            .state
            .is_healthy()));
        assert_eq!(replacement.supervised_count(), 2);
        replacement.shutdown();
        assert!(wait_for(&mut replacement, PluginHost::is_stopped));
    }

    #[test]
    fn changed_identities_retry_after_a_full_retiring_window_releases_capacity() {
        let admission = Admission::default();
        let mut retiring = PluginHost::with_admission(admission.clone());
        for index in 0..MAX_PROCESSES {
            retiring.register_fixture(PluginSpec {
                name: format!("github:12/retiring-{index}"),
                command: fake_plugin("stubborn"),
                enabled: true,
            });
        }
        assert!(wait_for(&mut retiring, |host| host.healthy().len() == MAX_PROCESSES));
        let mut holds: Vec<_> = (0..MAX_PROCESSES)
            .map(|index| {
                retiring
                    .hold_retirement(&format!("github:12/retiring-{index}"), None)
                    .unwrap()
            })
            .collect();
        retiring.shutdown();
        let deadline = Instant::now() + Duration::from_secs(5);
        while holds.iter().any(|hold| hold.observations() < 2) {
            retiring.poll();
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(10));
        }

        let mut replacement = PluginHost::with_admission(admission);
        for index in 0..MAX_PROCESSES {
            replacement.register_fixture(PluginSpec {
                name: format!("github:12/replacement-{index}"),
                command: echo_plugin(),
                enabled: true,
            });
        }
        for _ in 0..20 {
            retiring.poll();
            replacement.poll();
            assert_eq!(
                retiring.supervised_count() + replacement.supervised_count(),
                MAX_PROCESSES
            );
            assert!(replacement.plugins.iter().all(|plugin| plugin.spec.enabled
                && plugin.state == PluginState::Waiting
                && plugin.restart_attempts == 0
                && plugin.restart_at.is_none()));
        }

        drop(holds.pop());
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            retiring.poll();
            replacement.poll();
            assert!(retiring.supervised_count() + replacement.supervised_count() <= MAX_PROCESSES);
            assert!(replacement.supervised_count() <= 1);
            if replacement.healthy().len() == 1 {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "the first free slot was not reused"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(retiring.supervised_count(), MAX_PROCESSES - 1);
        drop(holds);
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            retiring.poll();
            replacement.poll();
            assert!(retiring.supervised_count() + replacement.supervised_count() <= MAX_PROCESSES);
            assert!(replacement
                .plugins
                .iter()
                .all(|plugin| plugin.restart_attempts == 0));
            if retiring.is_stopped() && replacement.healthy().len() == MAX_PROCESSES {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "changed identities did not start after capacity became free"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        println!("PASS changed-identity replacement: 32 real retiring generations held capacity; 32 new identities waited and all started after real retirement without exceeding the budget or consuming crash restarts");
        replacement.shutdown();
        assert!(wait_for(&mut replacement, PluginHost::is_stopped));
    }

    #[test]
    fn capacity_deferral_does_not_spend_a_scheduled_crash_restart() {
        let admission = Admission::default();
        let mut reservations: Vec<_> = (0..MAX_PROCESSES)
            .map(|index| {
                admission
                    .acquire(&format!("occupied-{index}"))
                    .ok()
                    .unwrap()
            })
            .collect();
        let mut host = PluginHost::with_admission(admission);
        let mut registration = spec(echo_plugin());
        registration.enabled = false;
        host.register_fixture(registration);
        host.plugins[0].spec.enabled = true;
        host.plugins[0].state = PluginState::Faulted {
            reason: "prior crash".into(),
        };
        host.plugins[0].restart_attempts = MAX_RESTARTS - 1;
        let due = Instant::now() - Duration::from_millis(1);
        host.plugins[0].restart_at = Some(due);
        for _ in 0..20 {
            host.poll();
            assert_eq!(host.plugins[0].state, PluginState::Waiting);
            assert_eq!(host.plugins[0].restart_attempts, MAX_RESTARTS - 1);
            assert_eq!(host.plugins[0].restart_at, Some(due));
            assert!(host.plugins[0].io.is_none());
        }
        drop(reservations.pop());
        assert!(wait_for(&mut host, |host| host.plugins[0]
            .state
            .is_healthy()));
        assert_eq!(host.plugins[0].restart_attempts, MAX_RESTARTS);
        assert!(host.plugins[0].restart_at.is_none());
        host.shutdown();
        assert!(wait_for(&mut host, PluginHost::is_stopped));
    }

    #[test]
    fn disabling_a_capacity_waiter_prevents_late_startup() {
        let admission = Admission::default();
        let reservations: Vec<_> = (0..MAX_PROCESSES)
            .map(|index| {
                admission
                    .acquire(&format!("occupied-{index}"))
                    .ok()
                    .unwrap()
            })
            .collect();
        let mut host = PluginHost::with_admission(admission);
        host.register_fixture(spec(echo_plugin()));
        assert_eq!(host.plugins[0].state, PluginState::Waiting);
        host.disable(KEY);
        drop(reservations);
        for _ in 0..20 {
            host.poll();
            assert_eq!(host.plugins[0].state, PluginState::Disabled);
            assert!(host.plugins[0].io.is_none());
            assert_eq!(host.plugins[0].restart_attempts, 0);
        }
    }

    #[test]
    fn a_detached_unproven_worker_keeps_its_shared_identity_reservation() {
        let admission = Admission::default();
        let mut retiring = PluginHost::with_admission(admission.clone());
        retiring.register_fixture(spec(fake_plugin("stubborn")));
        assert!(wait_for(&mut retiring, |host| host.plugins[0]
            .state
            .is_healthy()));
        let hold = retiring.hold_retirement(KEY, None).unwrap();
        retiring.shutdown();
        let deadline = Instant::now() + Duration::from_secs(5);
        while hold.observations() < 2 {
            retiring.poll();
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(10));
        }
        drop(retiring);
        assert!(admission.held(KEY));
        let mut replacement = PluginHost::with_admission(admission);
        replacement.register_fixture(spec(echo_plugin()));
        assert!(replacement.plugins[0].io.is_none());
        assert!(!replacement.supervised_stopped(KEY));
        replacement.disable(KEY);
        drop(hold);
        assert!(wait_for(&mut replacement, |host| host
            .supervised_stopped(KEY)
            && host.plugins[0].state == PluginState::Disabled));
        assert!(replacement.plugins[0].io.is_none());
        replacement.enable(KEY);
        assert!(wait_for(&mut replacement, |host| host.plugins[0]
            .state
            .is_healthy()));
        replacement.shutdown();
        assert!(wait_for(&mut replacement, PluginHost::is_stopped));
    }

    #[test]
    fn request_expiry_clears_authority_before_late_output() {
        let mut host = PluginHost::new();
        host.register_fixture(spec(hanging_plugin()));
        assert!(wait_for(&mut host, |host| host.listing()[0].1.is_healthy()));
        let request_id = host.request_status(KEY, "app_header").unwrap();
        let reply = serde_json::json!({"type":"status","session":host.world.session,"generation":host.plugins[0].io.as_ref().unwrap().generation,"request_id":request_id,"fragments":[{"text":"late"}]}).to_string();
        assert!(managed::parse(reply.as_bytes()).is_ok());
        host.plugins[0].inflight.insert(
            request_id,
            Instant::now() - REQUEST_TIMEOUT - Duration::from_millis(1),
        );
        host.poll();
        assert!(host.plugins[0].inflight.is_empty());
        assert!(host.plugins[0].scopes.is_empty());
        host.handle_line(0, &reply);
        assert!(host.results.is_empty());
        host.shutdown();
    }

    #[test]
    fn an_already_queued_answer_cannot_bypass_the_request_deadline() {
        let mut host = PluginHost::new();
        host.register_fixture(spec(echo_plugin()));
        assert!(wait_for(&mut host, |host| host.listing()[0].1.is_healthy()));
        let id = host.request_status(KEY, "app_header").unwrap();
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
    fn unavailable_managed_entrypoints_never_enter_the_automatic_restart_loop() {
        let mut host = PluginHost::new();
        host.register_fixture(spec(Vec::new()));
        host.disable(KEY);
        host.enable(KEY);
        for _ in 0..100 {
            host.poll();
        }
        assert!(matches!(
            host.plugins[0].state,
            PluginState::Unavailable { .. }
        ));
        assert_eq!(host.plugins[0].restart_attempts, 0);
        assert!(host.plugins[0].restart_at.is_none());
    }

    #[test]
    fn unsolicited_duplicate_and_previous_generation_replies_do_not_render() {
        let mut host = PluginHost::new();
        host.register_fixture(spec(fake_plugin("unmatched")));
        assert!(wait_for(&mut host, |host| host.listing()[0].1.is_healthy()));
        for _ in 0..5 {
            host.poll();
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(host.drain_results().is_empty());
        let old_id = host.request_status(KEY, "app_header").unwrap();
        let old_reply = serde_json::json!({"type":"status","session":host.world.session,"generation":host.plugins[0].io.as_ref().unwrap().generation,"request_id":old_id,"fragments":[{"text":"stale"}]}).to_string();
        assert!(managed::parse(old_reply.as_bytes()).is_ok());
        assert!(wait_for(&mut host, |host| !host.results.is_empty()));
        host.disable(KEY);
        assert!(
            host.drain_results().is_empty(),
            "disabled generation retained a result"
        );
        let package = host.plugins[0]
            .managed
            .as_ref()
            .unwrap()
            .inspected
            .as_ref()
            .unwrap()
            .executable
            .parent()
            .unwrap();
        std::fs::write(package.join("among-fake-mode"), "transport-duplicate").unwrap();
        host.enable(KEY);
        assert!(wait_for(&mut host, |host| host.listing()[0].1.is_healthy()));
        let new_id = host.request_status(KEY, "app_header").unwrap();
        assert!(new_id > old_id);
        host.handle_line(0, &old_reply);
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
        host.register_fixture(spec(fake_plugin("malformed")));
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
            &[(KEY.into(), Rejection::Malformed)],
            "malformed line was not rejected"
        );
        // The plugin survives its own bad output.
        assert!(host.listing()[0].1.is_healthy());

        // a running state alone cannot prove that the child still responds.
        assert!(host.request_status(KEY, "app_header").is_some());
        assert!(
            wait_for(&mut host, |h| !h.results.is_empty()),
            "plugin did not answer after its malformed output"
        );
        let results = host.drain_results();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].plugin, KEY);
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
