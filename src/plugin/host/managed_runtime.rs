//! admission and revision authority belong to the current supervised generation,
//! not to raw plugin output or a persisted desired-enabled switch.

use super::*;
use crate::plugin::data::Problem;
use serde_json::{json, Map, Value};
use std::collections::BTreeSet;

#[cfg(all(test, feature = "managed-transport-interop"))]
mod transport_interop;

// one stdout frame remains available for the plugin's single enumeration request.
pub(super) const MAX_MANAGED_REPLIES: usize = MAX_INFLIGHT - 1;

struct Enumeration {
    request_id: u64,
    scope: managed::Scope,
    cursor: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Configuration {
    NotApplied,
    Pending,
    Applied,
    Rejected { reason: managed::Reason },
    RestartRequired,
}

impl Configuration {
    pub fn description(&self) -> &'static str {
        match self {
            Self::NotApplied => "not applied to a running process",
            Self::Pending => "awaiting current process acknowledgement",
            Self::Applied => "applied by the current process",
            Self::Rejected { reason } => match reason {
                managed::Reason::InvalidConfiguration => "configuration rejected: invalid values",
                managed::Reason::UnsupportedVersion => {
                    "configuration rejected: unsupported version"
                }
                managed::Reason::Unavailable => "configuration unavailable or not acknowledged",
                managed::Reason::InternalError => "configuration rejected: plugin error",
            },
            Self::RestartRequired => "plugin restart required",
        }
    }
}

pub(super) struct Runtime {
    review: Option<Value>,
    pub(super) grants: BTreeSet<String>,
    scopes: HashMap<u64, managed::Scope>,
    configuration: Configuration,
    revision: u64,
    configure_sent: Option<Instant>,
    pub(super) configurations: HashMap<u64, Instant>,
    enumeration: Option<Enumeration>,
    pub(super) deliveries: HashMap<u64, Instant>,
}

impl Runtime {
    pub(super) fn new(review: Option<Value>) -> Self {
        Self {
            review,
            grants: Default::default(),
            scopes: Default::default(),
            configuration: Configuration::NotApplied,
            revision: 0,
            configure_sent: None,
            configurations: Default::default(),
            enumeration: None,
            deliveries: Default::default(),
        }
    }
    pub(super) fn reset(&mut self) {
        self.scopes.clear();
        self.configuration = Configuration::NotApplied;
        self.configure_sent = None;
        self.configurations.clear();
        self.enumeration = None;
        self.deliveries.clear();
    }
}

type EffectiveSettings = Option<(u32, Map<String, Value>)>;

impl PluginHost {
    pub fn running_generation(&self, name: &str) -> Option<String> {
        let plugin = &self.plugins[self.find(name)?];
        if !plugin.state.is_healthy() || !plugin.spec.enabled || plugin.managed.is_none() {
            return None;
        }
        plugin.io.as_ref().map(|io| io.generation.clone())
    }

    pub fn restart_managed(&mut self, name: &str, generation: &str) -> bool {
        if self.running_generation(name).as_deref() != Some(generation) {
            return false;
        }
        self.disable(name);
        self.enable(name);
        true
    }

    pub fn reconcile_workspace_content(
        &self,
        store: &mut SlotStore,
        workspaces: &[u64],
        active: usize,
    ) {
        let previous = self
            .world
            .workspaces
            .get(self.world.legacy.active_workspace);
        let current = workspaces.get(active);
        // event-driven headers can describe positions/counts even when the
        // active identity survives a move; pane-ID content remains untouched.
        for plugin in &self.plugins {
            store.remap_workspace_source(
                &plugin.spec.name,
                &self.world.workspaces,
                workspaces,
                previous != current || self.world.workspaces != workspaces,
            );
        }
    }

    pub fn configuration(&self, name: &str) -> Configuration {
        self.find(name)
            .and_then(|index| self.plugins[index].runtime.as_ref())
            .map_or(Configuration::NotApplied, |runtime| {
                runtime.configuration.clone()
            })
    }

    pub fn access_review(&self, name: &str) -> Result<(), Problem> {
        let index = self
            .find(name)
            .ok_or(Problem("plugin is no longer registered"))?;
        self.selected_grants(index).map(|_| ())
    }

    pub(crate) fn reviewed_features(&self, name: &str) -> Result<BTreeSet<String>, Problem> {
        let index = self
            .find(name)
            .ok_or(Problem("plugin is no longer registered"))?;
        self.selected_grants(index)
    }

    pub fn reviewed_notification_role(
        &self,
        name: &str,
    ) -> Result<Option<managed::NotificationRole>, Problem> {
        let index = self
            .find(name)
            .ok_or(Problem("plugin is no longer registered"))?;
        managed::notification_role(&self.selected_grants(index)?)
            .map_err(managed::NotificationRoleConflict::problem)
    }

    pub fn admitted_notification_role(&self, name: &str) -> Option<managed::NotificationRole> {
        let plugin = &self.plugins[self.find(name)?];
        let io = plugin.io.as_ref()?;
        if !plugin.spec.enabled
            || !plugin.state.is_healthy()
            || !io.handshake_done
            || io
                .notification_live
                .as_ref()
                .is_none_or(|lease| !lease.is_current())
        {
            return None;
        }
        managed::notification_role(&plugin.runtime.as_ref()?.grants)
            .ok()
            .flatten()
    }

    pub fn review_access(&mut self, name: &str, review: Value) -> Result<(), Problem> {
        let index = self
            .find(name)
            .ok_or(Problem("plugin is no longer registered"))?;
        if self.plugins[index].io.is_some() {
            return Err(Problem(
                "stop the current plugin generation before changing access",
            ));
        }
        let entry = self.plugins[index]
            .managed
            .as_ref()
            .ok_or(Problem("legacy plugins do not use managed grants"))?;
        let inspected = entry.inspected.as_ref().map_err(|problem| *problem)?;
        let grants = managed::granted_features_with(
            &inspected.manifest,
            &entry.receipt,
            Some(&json!({"review":review})),
            &self.supported_features(),
        )?;
        let runtime = self.plugins[index]
            .runtime
            .as_mut()
            .ok_or(Problem("managed runtime is unavailable"))?;
        runtime.review = Some(review);
        runtime.grants = grants;
        Ok(())
    }

    fn selected_grants(&self, index: usize) -> Result<BTreeSet<String>, Problem> {
        let entry = self.plugins[index]
            .managed
            .as_ref()
            .ok_or(Problem("managed receipt is unavailable"))?;
        let inspected = entry.inspected.as_ref().map_err(|problem| *problem)?;
        let runtime = self.plugins[index]
            .runtime
            .as_ref()
            .ok_or(Problem("managed runtime is unavailable"))?;
        if self.notifications.as_ref().is_some_and(|backend| {
            !backend
                .try_borrow()
                .is_ok_and(|backend| backend.registration_current(&self.plugins[index].spec.name))
        }) {
            return Err(Problem(
                "notification registration was revoked; static re-registration is required",
            ));
        }
        if self.notifications.is_none() {
            return managed::granted_features(
                &inspected.manifest,
                &entry.receipt,
                Some(&json!({"review":runtime.review})),
            );
        }
        managed::granted_features_with(
            &inspected.manifest,
            &entry.receipt,
            Some(&json!({"review":runtime.review})),
            &self.supported_features(),
        )
    }

    pub(super) fn prepare_managed(&mut self, index: usize) -> Result<(), Problem> {
        let grants = self.selected_grants(index)?;
        if let Some(runtime) = self.plugins[index].runtime.as_mut() {
            runtime.grants = grants;
        }
        self.effective_settings(index)?;
        Ok(())
    }

    fn effective_settings(&self, index: usize) -> Result<EffectiveSettings, Problem> {
        let Some(runtime) = &self.plugins[index].runtime else {
            return Ok(None);
        };
        if !runtime.grants.contains("settings") {
            return Ok(None);
        }
        let schema = self.plugins[index]
            .managed
            .as_ref()
            .and_then(|entry| entry.inspected.as_ref().ok())
            .and_then(|inspected| inspected.manifest.settings.as_ref());
        schema
            .map(|schema| {
                schema
                    .effective(self.plugins[index].settings.as_ref())
                    .map(|values| (schema.version, values))
            })
            .transpose()
    }

    pub(super) fn send_managed_hello(&mut self, index: usize) {
        let Some(io) = self.plugins[index].io.as_ref() else {
            return;
        };
        let Some(runtime) = self.plugins[index].runtime.as_ref() else {
            return;
        };
        let hello = managed::Request::ManagedHello {
            major: managed::MAJOR,
            minor: managed::MINOR,
            identity: self.plugins[index].spec.name.clone(),
            session: self.world.session.clone(),
            generation: io.generation.clone(),
            features: runtime.grants.clone(),
        };
        if io.process.send(&hello).is_err() {
            self.fault(index, "managed handshake could not be enqueued");
        }
    }

    pub(super) fn request_managed_status(&mut self, index: usize, scope: &str) -> Option<u64> {
        let runtime = self.plugins[index].runtime.as_ref()?;
        if !runtime.grants.contains("status")
            || self.plugins[index].inflight.len()
                + runtime.configurations.len()
                + runtime.deliveries.len()
                >= MAX_MANAGED_REPLIES
        {
            return None;
        }
        let io = self.plugins[index].io.as_ref()?;
        if !io.process.has_capacity() {
            return None;
        }
        let target = self.world.scope(scope)?;
        if runtime.scopes.values().any(|pending| pending == &target) {
            return None;
        }
        let mut context = self.world.context(&target, &runtime.grants)?;
        self.notification_context(index, &mut context);
        let request_id = self.plugins[index].next_request_id;
        let next = request_id.checked_add(1)?;
        let request = managed::Request::Status {
            session: self.world.session.clone(),
            generation: io.generation.clone(),
            request_id,
            scope: target.clone(),
            context,
        };
        match io.process.send(&request) {
            Ok(()) => {
                self.plugins[index].next_request_id = next;
                self.plugins[index]
                    .inflight
                    .insert(request_id, Instant::now());
                self.plugins[index].scopes.insert(
                    request_id,
                    StatusRequest {
                        scope: scope.into(),
                        workspace_revision: self.workspace_revision,
                    },
                );
                if let Some(runtime) = self.plugins[index].runtime.as_mut() {
                    runtime.scopes.insert(request_id, target);
                }
                Some(request_id)
            }
            Err(SendError::Full) => None,
            Err(SendError::Fault(reason)) => {
                self.fault(index, reason);
                None
            }
        }
    }

    pub(super) fn send_managed_event(&mut self, index: usize, request: &Request) {
        let Request::Event {
            event,
            pane,
            detail,
        } = request
        else {
            return;
        };
        let Some(runtime) = &self.plugins[index].runtime else {
            return;
        };
        let Some(event) = self
            .world
            .event(event, *pane, detail.as_deref(), &runtime.grants)
        else {
            return;
        };
        let Some(io) = &self.plugins[index].io else {
            return;
        };
        let request = managed::Request::Event {
            session: self.world.session.clone(),
            generation: io.generation.clone(),
            event,
        };
        if io.process.send(&request).is_err() {
            self.fault(index, "managed event input budget exceeded");
        }
    }

    pub(super) fn settings_saved(&mut self, index: usize) {
        let running = self.plugins[index].state.is_healthy();
        let restart = self.plugins[index]
            .managed
            .as_ref()
            .and_then(|entry| entry.inspected.as_ref().ok())
            .and_then(|inspected| inspected.manifest.settings.as_ref())
            .is_some_and(|schema| schema.apply == crate::plugin::schema::Apply::Restart);
        if let Some(runtime) = self.plugins[index].runtime.as_mut() {
            runtime.revision = runtime.revision.saturating_add(1);
            runtime.configure_sent = None;
            runtime.configuration = if !running {
                Configuration::NotApplied
            } else if restart {
                Configuration::RestartRequired
            } else {
                Configuration::Pending
            };
        }
        self.configure_if_due(index);
    }

    pub(super) fn configure_if_due(&mut self, index: usize) {
        if !self.plugins[index].state.is_healthy() {
            return;
        }
        let Some(runtime) = &mut self.plugins[index].runtime else {
            return;
        };
        // an expired/superseded revision can still reply. retain its bounded
        // credit until that frame is consumed or the generation stops.
        if runtime.configuration != Configuration::Pending {
            return;
        }
        if let Some(sent) = runtime.configure_sent {
            if sent.elapsed() >= REQUEST_TIMEOUT {
                runtime.configuration = Configuration::Rejected {
                    reason: managed::Reason::Unavailable,
                };
                runtime.configure_sent = None;
            }
            return;
        }
        let (version, values) = match self.effective_settings(index) {
            Ok(Some(settings)) => settings,
            Ok(None) => {
                if let Some(runtime) = self.plugins[index].runtime.as_mut() {
                    runtime.configuration = Configuration::NotApplied;
                }
                return;
            }
            Err(_) => {
                if let Some(runtime) = self.plugins[index].runtime.as_mut() {
                    runtime.configuration = Configuration::Rejected {
                        reason: managed::Reason::InvalidConfiguration,
                    };
                }
                return;
            }
        };
        let Some(io) = self.plugins[index].io.as_ref() else {
            return;
        };
        if !io.process.has_capacity() {
            return;
        }
        let Some(runtime) = &self.plugins[index].runtime else {
            return;
        };
        // superseded configurations can still answer; reserve their reply credit
        // alongside statuses until replies or deadlines release it.
        if self.plugins[index].inflight.len()
            + runtime.configurations.len()
            + runtime.deliveries.len()
            >= MAX_MANAGED_REPLIES
        {
            return;
        }
        let request = managed::Request::Configure {
            session: self.world.session.clone(),
            generation: io.generation.clone(),
            revision: runtime.revision,
            version,
            values,
        };
        match io.process.send(&request) {
            Ok(()) => {
                if let Some(runtime) = self.plugins[index].runtime.as_mut() {
                    runtime.configure_sent = Some(Instant::now());
                    runtime
                        .configurations
                        .insert(runtime.revision, Instant::now());
                }
            }
            Err(SendError::Full) => {}
            Err(SendError::Fault(reason)) => self.fault(index, reason),
        }
    }

    pub(super) fn handle_managed_line(&mut self, index: usize, line: &str) {
        let Ok(response) = managed::parse(line.as_bytes()) else {
            self.last_rejections
                .push((self.plugins[index].spec.name.clone(), Rejection::Malformed));
            return;
        };
        let Some(io) = self.plugins[index].io.as_ref() else {
            return;
        };
        let Some(runtime) = self.plugins[index].runtime.as_ref() else {
            return;
        };
        match response {
            managed::Response::ManagedHello {
                major,
                minor,
                identity,
                session,
                generation,
                features,
            } => {
                if io.handshake_done
                    || !io.process.started()
                    || io.started.elapsed() >= REQUEST_TIMEOUT
                    || major != managed::MAJOR
                    || minor != managed::MINOR
                    || identity != self.plugins[index].spec.name
                    || session != self.world.session
                    || generation != io.generation
                    || !features.is_subset(&runtime.grants)
                    || managed::feature_dependencies(&features).is_err()
                    || self.plugins[index]
                        .managed
                        .as_ref()
                        .and_then(|entry| entry.inspected.as_ref().ok())
                        .is_none_or(|inspected| {
                            inspected
                                .manifest
                                .required_features
                                .iter()
                                .any(|required| !features.contains(required))
                        })
                {
                    self.fault(
                        index,
                        "managed admission disagrees with current identity, generation or features",
                    );
                    return;
                }
                if let Some(io) = self.plugins[index].io.as_mut() {
                    io.handshake_done = true;
                }
                self.plugins[index].state = PluginState::Running;
                self.plugins[index].observed = Some(super::super::manifest::Api { major, minor });
                if let Some(runtime) = self.plugins[index].runtime.as_mut() {
                    runtime.grants = features;
                    runtime.revision = runtime.revision.saturating_add(1);
                    runtime.configuration = Configuration::Pending;
                }
                self.admit_notifications(index);
                self.configure_if_due(index);
            }
            managed::Response::Status {
                session,
                generation,
                request_id,
                refresh_ms,
                fragments,
            } => {
                if !io.handshake_done
                    || !self.plugins[index].state.is_healthy()
                    || session != self.world.session
                    || generation != io.generation
                {
                    return;
                }
                let Ok(valid) = protocol::validate_status(Response::Status {
                    request_id,
                    refresh_ms,
                    fragments,
                }) else {
                    self.last_rejections
                        .push((self.plugins[index].spec.name.clone(), Rejection::Malformed));
                    return;
                };
                let Some(sent) = self.plugins[index].inflight.remove(&request_id) else {
                    return;
                };
                let request = self.plugins[index].scopes.remove(&request_id);
                let target = self.plugins[index]
                    .runtime
                    .as_mut()
                    .and_then(|runtime| runtime.scopes.remove(&request_id));
                if sent.elapsed() >= REQUEST_TIMEOUT {
                    self.fault(index, "managed status exceeded its deadline");
                    return;
                }
                let Some(scope) = target.and_then(|target| self.world.resolve(&target)) else {
                    return;
                };
                let Some(request) = request else {
                    return;
                };
                if self.results.len() >= MAX_POLL_FRAMES {
                    self.fault(index, "managed result backlog exceeded");
                    return;
                }
                self.results.push(PluginResult {
                    plugin: self.plugins[index].spec.name.clone(),
                    scope,
                    fragments: valid.fragments,
                    refresh: refresh_ms.map_or(Refresh::OnEvent, Refresh::IntervalMs),
                    workspace_revision: request.workspace_revision,
                });
            }
            managed::Response::Configured {
                session,
                generation,
                revision,
                outcome,
                reason,
            } => {
                if !io.handshake_done
                    || !self.plugins[index].state.is_healthy()
                    || session != self.world.session
                    || generation != io.generation
                {
                    return;
                }
                let accepted = self.plugins[index]
                    .runtime
                    .as_mut()
                    .and_then(|runtime| runtime.configurations.remove(&revision));
                if accepted.is_none_or(|sent| sent.elapsed() >= REQUEST_TIMEOUT) {
                    return;
                }
                if self.plugins[index]
                    .runtime
                    .as_ref()
                    .is_none_or(|runtime| runtime.revision != revision)
                {
                    return;
                }
                let applied = outcome == managed::Outcome::Applied;
                if let Some(runtime) = self.plugins[index].runtime.as_mut() {
                    runtime.configure_sent = None;
                    runtime.configuration = match outcome {
                        managed::Outcome::Applied => Configuration::Applied,
                        managed::Outcome::Rejected => Configuration::Rejected {
                            reason: reason.unwrap_or(managed::Reason::InvalidConfiguration),
                        },
                        managed::Outcome::RestartRequired => Configuration::RestartRequired,
                    };
                    if applied {
                        runtime.scopes.clear();
                    }
                }
                if applied {
                    self.invalidate_notification_content(index);
                    // old status replies keep their transport credits, but no
                    // longer have authority to restore pre-configuration content.
                    self.plugins[index].scopes.clear();
                    let name = self.plugins[index].spec.name.clone();
                    self.results.retain(|result| result.plugin != name);
                    self.invalidated_sources.insert(name);
                }
            }
            managed::Response::Enumerate {
                session,
                generation,
                request_id,
                scope,
                cursor,
            } => {
                if !io.handshake_done
                    || !self.plugins[index].state.is_healthy()
                    || session != self.world.session
                    || generation != io.generation
                    || !runtime.grants.contains("context.terminals")
                    || self.world.resolve(&scope).is_none()
                {
                    return;
                }
                let cursor = match cursor.as_deref().map(managed::parse_reference).transpose() {
                    Ok(cursor) => cursor,
                    Err(_) => {
                        self.last_rejections
                            .push((self.plugins[index].spec.name.clone(), Rejection::Malformed));
                        return;
                    }
                };
                if let Some(pending) = runtime.enumeration.as_ref() {
                    if pending.request_id == request_id
                        && pending.scope == scope
                        && pending.cursor == cursor
                    {
                        return;
                    }
                    self.fault(
                        index,
                        "only one pending managed enumeration request is allowed",
                    );
                    return;
                }
                if let Some(runtime) = self.plugins[index].runtime.as_mut() {
                    runtime.enumeration = Some(Enumeration {
                        request_id,
                        scope,
                        cursor,
                    });
                }
                self.enumerate_if_due(index);
            }
            response @ (managed::Response::NotificationEvent { .. }
            | managed::Response::NotificationContribution { .. }
            | managed::Response::WithdrawNotification { .. }
            | managed::Response::DeliveryResult { .. }) => {
                self.handle_notification_response(index, response)
            }
            managed::Response::Notify {} => {
                if io.handshake_done && self.notices.len() < MAX_POLL_FRAMES {
                    self.notices.push((
                        self.plugins[index].spec.name.clone(),
                        "Managed plugin diagnostic (payload omitted)".into(),
                        None,
                    ));
                }
            }
            managed::Response::Error {} => self
                .last_rejections
                .push((self.plugins[index].spec.name.clone(), Rejection::Malformed)),
        }
    }

    pub(super) fn enumerate_if_due(&mut self, index: usize) {
        if !self.plugins[index].state.is_healthy() {
            return;
        }
        let Some(io) = self.plugins[index].io.as_ref() else {
            return;
        };
        if !io.process.has_capacity() {
            return;
        }
        let Some(runtime) = self.plugins[index].runtime.as_ref() else {
            return;
        };
        let Some(pending) = runtime.enumeration.as_ref() else {
            return;
        };
        let context = self
            .world
            .page(&pending.scope, &runtime.grants, pending.cursor);
        let Some(mut context) = context else {
            if let Some(runtime) = self.plugins[index].runtime.as_mut() {
                runtime.enumeration = None;
            }
            return;
        };
        self.notification_context(index, &mut context);
        let page = managed::Request::ContextPage {
            session: self.world.session.clone(),
            generation: io.generation.clone(),
            request_id: pending.request_id,
            scope: pending.scope.clone(),
            context,
        };
        match io.process.send(&page) {
            Ok(()) => {
                if let Some(runtime) = self.plugins[index].runtime.as_mut() {
                    runtime.enumeration = None;
                }
            }
            Err(SendError::Full) => {}
            Err(SendError::Fault(reason)) => self.fault(index, reason),
        }
    }
    pub(super) fn revoke_stale_targets(&mut self) {
        for plugin in &mut self.plugins {
            let Some(runtime) = plugin.runtime.as_mut() else {
                continue;
            };
            let stale: Vec<_> = runtime
                .scopes
                .iter()
                .filter(|(_, scope)| self.world.resolve(scope).is_none())
                .map(|(id, _)| *id)
                .collect();
            for id in stale {
                runtime.scopes.remove(&id);
                plugin.scopes.remove(&id);
                // revoke target authority, not the stdout credit for an answer
                // the child may already have queued. replies/deadlines reap it.
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    const KEY: &str = "github:12/sample";

    struct Fixture {
        host: PluginHost,
        root: PathBuf,
        package: PathBuf,
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            self.host.shutdown();
            let deadline = Instant::now() + Duration::from_secs(3);
            while !self.host.is_stopped() && Instant::now() < deadline {
                self.host.poll();
                std::thread::sleep(Duration::from_millis(2));
            }
            if self.host.is_stopped() {
                let _ = std::fs::remove_dir_all(&self.root);
            }
        }
    }
    impl Fixture {
        fn wait(&mut self, predicate: impl Fn(&PluginHost) -> bool) {
            self.wait_for(Duration::from_secs(4), predicate);
        }
        fn wait_for(&mut self, timeout: Duration, predicate: impl Fn(&PluginHost) -> bool) {
            let deadline = Instant::now() + timeout;
            loop {
                self.host.poll();
                if predicate(&self.host) {
                    return;
                }
                assert!(
                    Instant::now() < deadline,
                    "managed fixture timed out: {:?}",
                    self.host.listing()
                );
                std::thread::sleep(Duration::from_millis(3));
            }
        }
        fn frames(&self) -> Vec<Value> {
            std::fs::read_to_string(self.package.join("transcript.jsonl"))
                .unwrap_or_default()
                .lines()
                .filter_map(|line| serde_json::from_str(line).ok())
                .collect()
        }
    }
    fn fixture(mode: &str, optional: &[&str], grants: Option<&[&str]>) -> Fixture {
        fixture_manifest(mode, optional, grants, |_| {})
    }
    fn fixture_manifest(
        mode: &str,
        optional: &[&str],
        grants: Option<&[&str]>,
        alter: impl FnOnce(&mut Value),
    ) -> Fixture {
        let root = std::env::var_os("AMONG_CHECK_ROOT")
            .map(PathBuf::from)
            .unwrap_or_else(|| std::env::temp_dir().join("opencode"))
            .join(format!(
                "managed-runtime-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
        let mut receipt = inventory::tests::fixture(&root);
        let package = receipt.package_path(&root);
        let executable = std::env::current_exe()
            .unwrap()
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("examples/fake-plugin");
        assert!(
            executable.is_file(),
            "build the Rust fake-plugin fixture first"
        );
        std::fs::copy(executable, package.join("plugin")).unwrap();
        std::fs::set_permissions(
            package.join("plugin"),
            std::fs::Permissions::from_mode(0o700),
        )
        .unwrap();
        std::fs::write(package.join("among-fake-mode"), mode).unwrap();
        let mut manifest: Value =
            serde_json::from_slice(&std::fs::read(package.join("among-plugin.json")).unwrap())
                .unwrap();
        manifest["required_features"] = json!(["status", "settings"]);
        manifest["optional_features"] = json!(optional);
        alter(&mut manifest);
        let bytes = manifest.to_string();
        std::fs::write(package.join("among-plugin.json"), &bytes).unwrap();
        receipt.manifest_sha256 = inventory::checksum(bytes.as_bytes()).unwrap();
        std::fs::write(
            root.join("installed.json"),
            json!({"version":1,"installations":[receipt]}).to_string(),
        )
        .unwrap();
        let entry = inventory::read(&root).unwrap().remove(0);
        let local=grants.map(|features|json!({"enabled":true,"review":{"owner":"example","repository":"sample","features":features}}));
        let mut host = PluginHost::new();
        host.update_context(
            vec![10, 20],
            Context {
                active_workspace: 0,
                workspace_count: 2,
                focused_pane: Some(1),
                terminals: vec![
                    crate::plugin::TerminalInfo {
                        pane: 1,
                        workspace: 0,
                        cwd: Some("/first".into()),
                        title: Some("First".into()),
                        running: true,
                    },
                    crate::plugin::TerminalInfo {
                        pane: 2,
                        workspace: 1,
                        cwd: Some("/sibling-private".into()),
                        title: Some("Other".into()),
                        running: false,
                    },
                ],
            },
        );
        host.register_managed(entry, local.as_ref());
        Fixture {
            host,
            root,
            package,
        }
    }

    #[test]
    fn applied_settings_invalidate_event_content_without_releasing_old_reply_credit() {
        let mut fixture = fixture("managed-on-event-held", &[], Some(&["status", "settings"]));
        fixture.wait(|host| host.configuration(KEY) == Configuration::Applied);
        fixture.host.drain_invalidated_sources();
        fixture
            .host
            .request_status(KEY, "app_header", &Context::default())
            .unwrap();
        fixture.wait(|host| !host.results.is_empty());
        let initial = fixture.host.drain_results();
        assert_eq!(initial.len(), 1);
        assert_eq!(initial[0].refresh, Refresh::OnEvent);
        assert_eq!(initial[0].fragments[0].text.as_deref(), Some("default"));
        let old = fixture
            .host
            .request_status(KEY, "app_footer", &Context::default())
            .unwrap();
        let package = fixture.package.clone();
        fixture.wait(|_| package.join("old-status-held").exists());
        fixture.host.stored_settings_changed(
            KEY,
            json!({"version":1,"values":{"label":"new configured label"}}),
        );
        fixture.wait(|host| {
            host.configuration(KEY) == Configuration::Applied
                && package.join("new-settings-applied").exists()
        });
        assert_eq!(
            fixture.host.drain_invalidated_sources(),
            vec![KEY.to_string()]
        );
        assert!(
            fixture.host.plugins[0].inflight.contains_key(&old),
            "the child can still emit the old reply"
        );
        assert!(
            !fixture.host.plugins[0].scopes.contains_key(&old),
            "old content authority is revoked"
        );
        assert!(fixture.host.drain_results().is_empty());
        let current = fixture
            .host
            .request_status(KEY, "app_footer", &Context::default())
            .unwrap();
        assert_ne!(current, old);
        assert_eq!(fixture.host.plugins[0].inflight.len(), 2);
        std::fs::write(package.join("release-old-status"), b"yes").unwrap();
        fixture.wait(|host| !host.results.is_empty());
        assert!(!fixture.host.plugins[0].inflight.contains_key(&old));
        let results = fixture.host.drain_results();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].scope, "app_footer");
        assert_eq!(
            results[0].fragments[0].text.as_deref(),
            Some("new configured label")
        );
        assert_eq!(results[0].refresh, Refresh::OnEvent);
    }

    #[test]
    fn managed_admission_and_configuration_reach_a_real_pinned_executable() {
        let mut fixture = fixture("managed-echo", &[], Some(&["status", "settings"]));
        fixture.wait(|host| host.configuration(KEY) == Configuration::Applied);
        assert!(fixture.package.join("executed").exists());
        assert_eq!(
            std::fs::read_to_string(fixture.package.join("credential-inherited")).unwrap(),
            "no"
        );
        fixture.host.stored_settings_changed(
            KEY,
            json!({"version":1,"values":{"label":"changed","unknown":"not delivered"}}),
        );
        assert_eq!(fixture.host.configuration(KEY), Configuration::Pending);
        fixture.wait(|host| host.configuration(KEY) == Configuration::Applied);
        assert!(fixture
            .host
            .request_status(KEY, "app_header", &Context::default())
            .is_some());
        fixture.wait(|host| !host.results.is_empty());
        let results = fixture.host.drain_results();
        assert_eq!(results[0].fragments[0].text.as_deref(), Some("changed"));
        let configured = fixture
            .frames()
            .into_iter()
            .rfind(|frame| frame["type"] == "configure")
            .unwrap();
        assert_eq!(configured["values"], json!({"label":"changed"}));
        assert!(
            fixture
                .host
                .request_status(KEY, "pane_header:1", &Context::default())
                .is_none(),
            "status alone must not enumerate terminal identities through requested scopes"
        );
    }

    #[test]
    fn unreviewed_or_wrong_publisher_plugins_never_execute_and_invalid_hellos_never_admit() {
        let mut unreviewed = fixture("managed-echo", &[], None);
        unreviewed.host.enable(KEY);
        assert!(matches!(
            unreviewed.host.listing()[0].1,
            PluginState::Unavailable { .. }
        ));
        assert!(!unreviewed.package.join("executed").exists());
        assert!(unreviewed
            .host
            .review_access(
                KEY,
                json!({"owner":"other","repository":"sample","features":["status","settings"]})
            )
            .is_err());
        assert!(!unreviewed.package.join("executed").exists());
        for mode in [
            "managed-wrong-generation",
            "managed-wrong-identity",
            "managed-wrong-version",
            "managed-wrong-session",
            "managed-missing-required",
            "managed-ungranted-feature",
        ] {
            let mut fixture = fixture(mode, &[], Some(&["status", "settings"]));
            fixture.wait(|host| {
                matches!(
                    host.listing()[0].1,
                    PluginState::Stopping { .. } | PluginState::Faulted { .. }
                )
            });
            assert!(fixture.host.healthy().is_empty());
            assert!(fixture.host.drain_results().is_empty());
        }
    }

    #[test]
    fn managed_pre_admission_and_old_generation_output_has_no_authority() {
        let mut fixture = fixture(
            "managed-before-admission",
            &[],
            Some(&["status", "settings"]),
        );
        fixture.wait(|host| host.configuration(KEY) == Configuration::Applied);
        assert!(fixture.host.drain_results().is_empty());
        let io = fixture.host.plugins[0].io.as_ref().unwrap();
        let generation = io.generation.clone();
        let session = fixture.host.world.session.clone();
        let request = fixture
            .host
            .request_status(KEY, "app_header", &Context::default())
            .unwrap();
        fixture.host.handle_managed_line(0,&json!({"type":"status","session":session,"generation":"old-generation","request_id":request,"fragments":[{"text":"stale"}]}).to_string());
        assert!(fixture.host.results.is_empty());
        assert!(fixture.host.plugins[0].inflight.contains_key(&request));
        fixture.wait(|host| !host.results.is_empty());
        assert_eq!(
            fixture.host.drain_results()[0].fragments[0].text.as_deref(),
            Some("default")
        );
        fixture.host.disable(KEY);
        fixture.wait(|host| host.is_stopped());
        fixture.host.enable(KEY);
        fixture.wait(|host| host.configuration(KEY) == Configuration::Applied);
        assert_ne!(
            fixture.host.plugins[0].io.as_ref().unwrap().generation,
            generation
        );
        fixture.host.handle_managed_line(0,&json!({"type":"configured","session":session,"generation":generation,"revision":999,"outcome":"rejected"}).to_string());
        assert_eq!(fixture.host.configuration(KEY), Configuration::Applied);
    }

    #[test]
    fn granted_context_and_events_are_observed_on_wire_without_sibling_or_field_leaks() {
        let grants = [
            "status",
            "settings",
            "context.terminals",
            "context.cwd",
            "events.terminal",
        ];
        let mut fixture = fixture(
            "managed-echo",
            &["context.terminals", "context.cwd", "events.terminal"],
            Some(&grants),
        );
        fixture.wait(|host| host.configuration(KEY) == Configuration::Applied);
        fixture
            .host
            .request_status(KEY, "pane_header:1", &Context::default())
            .unwrap();
        fixture.wait(|host| !host.results.is_empty());
        fixture.host.drain_results();
        let status = fixture
            .frames()
            .into_iter()
            .find(|frame| frame["type"] == "status")
            .unwrap();
        assert_eq!(status["scope"]["target"]["reference"], "1");
        assert_eq!(status["context"]["terminals"].as_array().unwrap().len(), 1);
        assert_eq!(status["context"]["terminals"][0]["cwd"], "/first");
        assert!(status["context"]["terminals"][0].get("title").is_none());
        assert!(status["context"]["terminals"][0].get("running").is_none());
        assert!(!status.to_string().contains("sibling-private"));
        fixture
            .host
            .broadcast_event(EventKind::WorkingDirectoryChanged, 1, Some("/new".into()));
        let deadline = Instant::now() + Duration::from_secs(2);
        while !fixture
            .frames()
            .iter()
            .any(|frame| frame["type"] == "event")
        {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(3));
        }
        let event = fixture
            .frames()
            .into_iter()
            .find(|frame| frame["type"] == "event")
            .unwrap();
        assert_eq!(event["event"]["cwd"], "/new");
        assert_eq!(event["event"]["workspace"], "10");
        let target = fixture.host.world.scope("workspace_summary:1").unwrap();
        let id = fixture
            .host
            .request_status(KEY, "workspace_summary:1", &Context::default())
            .unwrap();
        let mut context = fixture.host.world.legacy.clone();
        context
            .terminals
            .iter_mut()
            .for_each(|terminal| terminal.workspace = 1 - terminal.workspace);
        fixture.host.update_context(vec![20, 10], context);
        assert_eq!(
            fixture.host.world.resolve(&target).unwrap(),
            "workspace_summary:0"
        );
        fixture.wait(|host| !host.results.is_empty());
        assert!(fixture.host.drain_results().is_empty());
        assert!(!fixture.host.plugins[0].inflight.contains_key(&id));
        fixture
            .host
            .request_status(KEY, "workspace_summary:0", &Context::default())
            .unwrap();
        fixture.wait(|host| !host.results.is_empty());
        assert_eq!(fixture.host.drain_results()[0].scope, "workspace_summary:0");
    }

    #[test]
    fn a_real_superseded_ack_cannot_mark_the_current_revision_applied() {
        let mut fixture = fixture("managed-stale-config", &[], Some(&["status", "settings"]));
        fixture.wait(|host| {
            host.plugins[0]
                .runtime
                .as_ref()
                .unwrap()
                .configure_sent
                .is_some()
        });
        fixture
            .host
            .stored_settings_changed(KEY, json!({"version":1,"values":{"label":"new revision"}}));
        let deadline = Instant::now() + Duration::from_secs(2);
        while !fixture.package.join("old-ack-sent").exists() {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(2));
        }
        fixture.host.poll();
        assert_eq!(fixture.host.configuration(KEY), Configuration::Pending);
        std::fs::write(fixture.package.join("allow-current-ack"), b"yes").unwrap();
        fixture.wait(|host| host.configuration(KEY) == Configuration::Applied);
    }

    #[test]
    fn rejection_reason_is_finite_and_settings_bearing_diagnostics_are_redacted() {
        let mut fixture = fixture("managed-reject", &[], Some(&["status", "settings"]));
        fixture.wait(|host| matches!(host.configuration(KEY), Configuration::Rejected { .. }));
        fixture.host.stored_settings_changed(
            KEY,
            json!({"version":1,"values":{"label":"not-a-real-secret-marker"}}),
        );
        fixture.wait(|host| matches!(host.configuration(KEY), Configuration::Rejected { .. }));
        let descriptions = format!(
            "{:?} {:?}",
            fixture.host.configuration(KEY),
            fixture.host.drain_notices()
        );
        assert!(!descriptions.contains("not-a-real-secret-marker"));
        assert!(fixture.host.plugins[0].state.is_healthy());
    }

    #[test]
    fn optional_runtime_features_can_degrade_and_closed_targets_revoke_requests() {
        let grants = ["status", "settings", "context.terminals", "context.title"];
        let mut fixture = fixture(
            "managed-optional-subset",
            &["context.terminals", "context.title"],
            Some(&grants),
        );
        fixture.wait(|host| host.configuration(KEY) == Configuration::Applied);
        assert!(!fixture.host.plugins[0]
            .runtime
            .as_ref()
            .unwrap()
            .grants
            .contains("context.title"));
        let id = fixture
            .host
            .request_status(KEY, "pane_header:1", &Context::default())
            .unwrap();
        let mut context = fixture.host.world.legacy.clone();
        context.terminals.retain(|terminal| terminal.pane != 1);
        fixture.host.update_context(vec![10, 20], context);
        assert!(!fixture.host.plugins[0]
            .runtime
            .as_ref()
            .unwrap()
            .scopes
            .contains_key(&id));
        fixture.wait(|host| !host.plugins[0].inflight.contains_key(&id));
        assert!(fixture.host.drain_results().is_empty());
        assert!(fixture.host.plugins[0].state.is_healthy());
    }

    #[test]
    fn selection_revalidation_refuses_replaced_links_before_explicit_enable() {
        let mut fixture = fixture("managed-echo", &[], None);
        std::fs::remove_file(fixture.package.join("plugin")).unwrap();
        std::os::unix::fs::symlink("/bin/sh", fixture.package.join("plugin")).unwrap();
        fixture
            .host
            .review_access(
                KEY,
                json!({"owner":"example","repository":"sample","features":["status","settings"]}),
            )
            .unwrap();
        fixture.host.enable(KEY);
        assert!(matches!(
            fixture.host.listing()[0].1,
            PluginState::Unavailable { .. }
        ));
        assert!(fixture.host.is_stopped());
        assert!(!Path::new(&fixture.package.join("executed")).exists());
    }

    #[test]
    fn real_context_enumeration_pages_large_authorized_lists_with_bounded_frames() {
        let grants = [
            "status",
            "settings",
            "context.terminals",
            "context.cwd",
            "context.title",
        ];
        let mut fixture = fixture(
            "managed-enumerate",
            &["context.terminals", "context.cwd", "context.title"],
            Some(&grants),
        );
        fixture.wait(|host| host.configuration(KEY) == Configuration::Applied);
        let context = Context {
            active_workspace: 0,
            workspace_count: 1,
            focused_pane: Some(1),
            terminals: (1..=100)
                .map(|pane| crate::plugin::TerminalInfo {
                    pane,
                    workspace: 0,
                    cwd: Some("x".repeat(2048)),
                    title: Some("oversize".repeat(1024)),
                    running: true,
                })
                .collect(),
        };
        fixture.host.update_context(vec![10], context);
        fixture
            .host
            .request_status(KEY, "app_header", &Context::default())
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(4);
        while !fixture.package.join("enumeration-complete").exists() {
            fixture.host.poll();
            assert!(Instant::now() < deadline, "enumeration did not finish");
            assert!(fixture.host.plugins[0].state.is_healthy());
            std::thread::sleep(Duration::from_millis(3));
        }
        let pages: Vec<_> = fixture
            .frames()
            .into_iter()
            .filter(|frame| matches!(frame["type"].as_str(), Some("status" | "context_page")))
            .collect();
        assert!(pages.len() > 1);
        let mut references = BTreeSet::new();
        for page in pages {
            assert!(serde_json::to_vec(&page).unwrap().len() < 32 * 1024);
            for terminal in page["context"]["terminals"].as_array().unwrap() {
                assert!(references.insert(terminal["pane"].as_str().unwrap().to_string()));
                assert!(terminal["title"].is_null());
                assert_eq!(terminal["unavailable"], json!(["title"]));
                assert!(terminal.get("running").is_none());
            }
        }
        assert_eq!(references.len(), 100);
    }

    #[test]
    fn configurations_and_statuses_share_reply_credit_without_faulting_a_fast_plugin() {
        let grants = ["status", "settings", "context.terminals"];
        let mut fixture = fixture("managed-echo", &["context.terminals"], Some(&grants));
        fixture.wait(|host| host.plugins[0].state.is_healthy());
        fixture.host.update_context(
            vec![10],
            Context {
                active_workspace: 0,
                workspace_count: 1,
                focused_pane: Some(1),
                terminals: (1..=8)
                    .map(|pane| crate::plugin::TerminalInfo {
                        pane,
                        workspace: 0,
                        ..Default::default()
                    })
                    .collect(),
            },
        );
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
        let mut sent = BTreeSet::new();
        let mut answered = BTreeSet::new();
        let deadline = Instant::now() + Duration::from_secs(4);
        while answered.len() < scopes.len() {
            fixture.host.poll();
            // mirror the application's invalidated-source consumer: admission
            // may apply configuration after these first status requests queued.
            if !fixture.host.drain_invalidated_sources().is_empty() {
                sent.clear();
                answered.clear();
            }
            for result in fixture.host.drain_results() {
                answered.insert(result.scope);
            }
            for scope in &scopes {
                if !sent.contains(scope)
                    && fixture
                        .host
                        .request_status(KEY, scope, &Context::default())
                        .is_some()
                {
                    sent.insert(scope.clone());
                }
            }
            let runtime = fixture.host.plugins[0].runtime.as_ref().unwrap();
            assert!(
                fixture.host.plugins[0].inflight.len() + runtime.configurations.len()
                    <= MAX_MANAGED_REPLIES
            );
            assert_eq!(fixture.host.plugins[0].restart_attempts, 0);
            assert!(fixture.host.plugins[0].state.is_healthy());
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(answered.len(), 20);
    }

    #[test]
    fn unsupported_required_features_and_expanded_required_grants_refuse_before_execution() {
        for required in ["future.unsupported", "context.terminals"] {
            let fixture = fixture_manifest(
                "managed-echo",
                &[],
                Some(&["status", "settings"]),
                |manifest| {
                    manifest["required_features"] = json!(["status", "settings", required]);
                },
            );
            assert!(matches!(
                fixture.host.listing()[0].1,
                PluginState::Unavailable { .. }
            ));
            assert!(fixture.host.desired_enabled(KEY));
            assert_eq!(fixture.host.supervised_count(), 0);
            assert!(!fixture.package.join("executed").exists());
        }
        let mut optional = fixture(
            "managed-echo",
            &["future.unsupported"],
            Some(&["status", "settings", "future.unsupported"]),
        );
        optional.wait(|host| host.configuration(KEY) == Configuration::Applied);
        let hello = optional
            .frames()
            .into_iter()
            .find(|frame| frame["type"] == "managed_hello")
            .unwrap();
        assert_eq!(hello["features"], json!(["settings", "status"]));
    }

    #[test]
    fn managed_environment_excludes_credentials_present_in_the_actual_parent_process() {
        const MARKER: &str = "AMONG_MANAGED_TEST_CREDENTIAL";
        if std::env::var_os(MARKER).is_none() {
            let output=std::process::Command::new(std::env::current_exe().unwrap())
                .args(["plugin::host::managed_runtime::tests::managed_environment_excludes_credentials_present_in_the_actual_parent_process","--exact","--nocapture"])
                .env(MARKER,"synthetic-parent-only-marker")
                .env("GITHUB_TOKEN","synthetic-parent-only-marker")
                .env("AWS_SECRET_ACCESS_KEY","synthetic-parent-only-marker")
                .output().unwrap();
            assert!(
                output.status.success(),
                "{} {}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }
        assert!(std::env::var_os("GITHUB_TOKEN").is_some());
        assert!(std::env::var_os("AWS_SECRET_ACCESS_KEY").is_some());
        let mut fixture = fixture("managed-echo", &[], Some(&["status", "settings"]));
        fixture.wait(|host| host.configuration(KEY) == Configuration::Applied);
        assert_eq!(
            std::fs::read_to_string(fixture.package.join("credential-inherited")).unwrap(),
            "no"
        );
        let names: BTreeSet<String> = serde_json::from_slice(
            &std::fs::read(fixture.package.join("environment-names.json")).unwrap(),
        )
        .unwrap();
        assert!(names.contains("LANG") && names.contains("PATH"));
        assert!(names.iter().all(|name| matches!(
            name.as_str(),
            "HOME"
                | "PATH"
                | "LANG"
                | "XDG_DATA_HOME"
                | "XDG_CONFIG_HOME"
                | "XDG_CACHE_HOME"
                | "XDG_RUNTIME_DIR"
        )));
    }

    #[test]
    fn an_expired_generation_clock_faults_handshake_before_a_new_observer_waits() {
        let mut handshake = fixture("managed-hanging", &[], Some(&["status", "settings"]));
        // control only the recorded deadline origin, not worker completion or
        // stop ownership. first-poll time must not replace registration time.
        handshake.host.plugins[0].io.as_mut().unwrap().started = Instant::now() - REQUEST_TIMEOUT;
        handshake.host.poll();
        assert!(
            matches!(
                &handshake.host.plugins[0].state,
                PluginState::Stopping { reason, .. } | PluginState::Faulted { reason }
                    if reason.starts_with("plugin did not complete handshake within five seconds")
            ),
            "{:?}",
            handshake.host.listing()
        );
        assert!(handshake.host.healthy().is_empty());
        assert!(handshake.host.drain_results().is_empty());
    }

    #[test]
    fn real_handshake_and_configuration_silence_enforce_five_second_deadlines() {
        let mut handshake = fixture("managed-hanging", &[], Some(&["status", "settings"]));
        // registration starts the real handshake deadline. fixture return or a
        // delayed test thread is not a new deadline origin.
        let start = handshake.host.plugins[0].io.as_ref().unwrap().started;
        let observer_start = Instant::now();
        handshake.wait_for(Duration::from_secs(7), |host| {
            matches!(
                host.listing()[0].1,
                PluginState::Stopping { .. } | PluginState::Faulted { .. }
            )
        });
        assert!(
            start.elapsed() >= REQUEST_TIMEOUT,
            "generation_elapsed={:?}, observer_elapsed={:?}, state={:?}",
            start.elapsed(),
            observer_start.elapsed(),
            handshake.host.listing()
        );
        assert!(
            matches!(
                &handshake.host.plugins[0].state,
                PluginState::Stopping { reason, .. } | PluginState::Faulted { reason }
                    if reason.starts_with("plugin did not complete handshake within five seconds")
            ),
            "{:?}",
            handshake.host.listing()
        );
        assert!(handshake.host.healthy().is_empty());
        assert!(handshake.host.drain_results().is_empty());
        let mut configuration = fixture(
            "managed-no-config-reply",
            &[],
            Some(&["status", "settings"]),
        );
        configuration.wait(|host| host.configuration(KEY) == Configuration::Pending);
        let sent = configuration.host.plugins[0]
            .runtime
            .as_ref()
            .unwrap()
            .configure_sent
            .unwrap();
        configuration.wait_for(Duration::from_secs(7), |host| {
            matches!(
                host.configuration(KEY),
                Configuration::Rejected {
                    reason: managed::Reason::Unavailable
                }
            )
        });
        assert!(sent.elapsed() >= REQUEST_TIMEOUT);
        assert!(configuration.host.plugins[0].state.is_healthy());
        assert!(configuration
            .host
            .request_status(KEY, "app_header", &Context::default())
            .is_some());
        configuration.wait(|host| !host.results.is_empty());
        assert_ne!(
            configuration.host.drain_results()[0].fragments[0]
                .text
                .as_deref(),
            Some("default")
        );
    }

    #[test]
    fn declared_restart_settings_wait_for_a_new_process_and_runtime_restart_results_are_distinct() {
        let mut declared = fixture_manifest(
            "managed-echo",
            &[],
            Some(&["status", "settings"]),
            |manifest| manifest["settings"]["apply"] = json!("restart"),
        );
        declared.wait(|host| host.configuration(KEY) == Configuration::Applied);
        let generation = declared.host.plugins[0]
            .io
            .as_ref()
            .unwrap()
            .generation
            .clone();
        declared
            .host
            .stored_settings_changed(KEY, json!({"version":1,"values":{"label":"after restart"}}));
        assert_eq!(
            declared.host.configuration(KEY),
            Configuration::RestartRequired
        );
        declared
            .host
            .request_status(KEY, "app_header", &Context::default())
            .unwrap();
        declared.wait(|host| !host.results.is_empty());
        assert_eq!(
            declared.host.drain_results()[0].fragments[0]
                .text
                .as_deref(),
            Some("default")
        );
        assert_eq!(
            declared
                .frames()
                .iter()
                .filter(|frame| frame["type"] == "configure")
                .count(),
            1
        );
        assert!(!declared.host.restart_managed(KEY, "retired-generation"));
        assert!(declared.host.restart_managed(KEY, &generation));
        assert!(declared.host.desired_enabled(KEY));
        assert_eq!(declared.host.configuration(KEY), Configuration::NotApplied);
        declared.wait(|host| host.configuration(KEY) == Configuration::Applied);
        assert_ne!(
            declared.host.plugins[0].io.as_ref().unwrap().generation,
            generation
        );
        declared
            .host
            .request_status(KEY, "app_header", &Context::default())
            .unwrap();
        declared.wait(|host| !host.results.is_empty());
        assert_eq!(
            declared.host.drain_results()[0].fragments[0]
                .text
                .as_deref(),
            Some("after restart")
        );
        let mut runtime = fixture("managed-restart", &[], Some(&["status", "settings"]));
        runtime.wait(|host| host.configuration(KEY) == Configuration::RestartRequired);
        assert!(runtime.host.plugins[0].state.is_healthy());
    }

    #[test]
    fn changing_grants_requires_reaping_and_changes_real_context_and_event_traffic() {
        let optional = [
            "context.terminals",
            "context.cwd",
            "context.title",
            "context.process",
            "events.terminal",
        ];
        let mut fixture = fixture("managed-echo", &optional, Some(&["status", "settings"]));
        fixture.wait(|host| host.configuration(KEY) == Configuration::Applied);
        fixture.host.broadcast_event(
            EventKind::WorkingDirectoryChanged,
            1,
            Some("not-authorized".into()),
        );
        let review = json!({"owner":"example","repository":"sample","features":["status","settings","context.terminals","context.title","context.process","events.terminal"]});
        assert!(fixture.host.review_access(KEY, review.clone()).is_err());
        fixture.host.disable(KEY);
        fixture.wait(|host| host.is_stopped());
        assert_eq!(
            fixture
                .frames()
                .iter()
                .filter(|frame| frame["type"] == "event")
                .count(),
            0
        );
        fixture.host.review_access(KEY, review).unwrap();
        fixture.host.enable(KEY);
        fixture.wait(|host| host.configuration(KEY) == Configuration::Applied);
        fixture
            .host
            .broadcast_event(EventKind::ProcessExited { status: 7 }, 1, None);
        fixture
            .host
            .request_status(KEY, "pane_header:1", &Context::default())
            .unwrap();
        fixture.wait(|host| !host.results.is_empty());
        let frames = fixture.frames();
        let status = frames
            .iter()
            .rfind(|frame| frame["type"] == "status")
            .unwrap();
        assert_eq!(status["context"]["terminals"][0]["title"], "First");
        assert_eq!(status["context"]["terminals"][0]["running"], true);
        assert!(status["context"]["terminals"][0].get("cwd").is_none());
        let event = frames
            .iter()
            .find(|frame| frame["type"] == "event")
            .unwrap();
        assert_eq!(event["event"]["status"], 7);
        assert!(!frames
            .iter()
            .any(|frame| frame.to_string().contains("not-authorized")));
    }

    #[test]
    fn closed_target_requests_keep_reply_credit_until_the_real_child_answers() {
        let mut fixture = fixture(
            "managed-held-status",
            &["context.terminals"],
            Some(&["status", "settings", "context.terminals"]),
        );
        fixture.wait(|host| host.configuration(KEY) == Configuration::Applied);
        let context = |start| Context {
            active_workspace: 0,
            workspace_count: 1,
            focused_pane: None,
            terminals: (start..start + 15)
                .map(|pane| crate::plugin::TerminalInfo {
                    pane,
                    workspace: 0,
                    ..Default::default()
                })
                .collect(),
        };
        fixture.host.update_context(vec![10], context(1));
        for pane in 1..=15 {
            assert!(fixture
                .host
                .request_status(KEY, &format!("pane_header:{pane}"), &Context::default())
                .is_some());
        }
        fixture.host.update_context(vec![20], context(16));
        assert!(
            fixture
                .host
                .request_status(KEY, "pane_header:16", &Context::default())
                .is_none(),
            "revoking target authority must not release credit for a reply still in flight"
        );
        std::fs::write(fixture.package.join("allow-status"), b"yes").unwrap();
        fixture.wait(|host| host.plugins[0].inflight.is_empty());
        assert!(fixture.host.drain_results().is_empty());
        fixture
            .host
            .request_status(KEY, "pane_header:16", &Context::default())
            .unwrap();
        fixture.wait(|host| !host.results.is_empty());
        assert_eq!(fixture.host.drain_results()[0].scope, "pane_header:16");
        assert_eq!(fixture.host.plugins[0].restart_attempts, 0);
    }

    #[test]
    fn expired_configuration_revisions_keep_credit_until_late_real_replies_are_consumed() {
        let mut fixture = fixture("managed-held-config", &[], Some(&["status", "settings"]));
        fixture.wait(|host| host.configuration(KEY) == Configuration::Pending);
        for revision in 1..15 {
            fixture.host.stored_settings_changed(
                KEY,
                json!({"version":1,"values":{"label":format!("revision {revision}")}}),
            );
        }
        fixture.wait(|host| {
            host.plugins[0]
                .runtime
                .as_ref()
                .unwrap()
                .configurations
                .len()
                == 15
        });
        fixture.wait_for(Duration::from_secs(7), |host| {
            matches!(
                host.configuration(KEY),
                Configuration::Rejected {
                    reason: managed::Reason::Unavailable
                }
            )
        });
        assert!(fixture
            .host
            .request_status(KEY, "app_header", &Context::default())
            .is_none());
        std::fs::write(fixture.package.join("allow-configuration"), b"yes").unwrap();
        fixture.wait(|host| {
            host.plugins[0]
                .runtime
                .as_ref()
                .unwrap()
                .configurations
                .is_empty()
        });
        assert!(matches!(
            fixture.host.configuration(KEY),
            Configuration::Rejected { .. }
        ));
        fixture
            .host
            .stored_settings_changed(KEY, json!({"version":1,"values":{"label":"current"}}));
        fixture.wait(|host| host.configuration(KEY) == Configuration::Applied);
        fixture
            .host
            .request_status(KEY, "app_header", &Context::default())
            .unwrap();
        fixture.wait(|host| !host.results.is_empty());
        assert_eq!(
            fixture.host.drain_results()[0].fragments[0].text.as_deref(),
            Some("current")
        );
        assert_eq!(fixture.host.plugins[0].restart_attempts, 0);
    }

    #[test]
    fn a_destination_summary_stays_with_it_when_a_pane_move_closes_the_source() {
        use crate::tree::{Orientation, PaneId};
        use crate::workspace::{Workspace, WorkspaceSet};
        let mut fixture = fixture("managed-echo", &[], None);
        let mut set = WorkspaceSet::new(Workspace::new("source".into(), PaneId(1), 0));
        set.push_active(Workspace::new("destination".into(), PaneId(2), 1));
        set.activate(0);
        let references = |set: &WorkspaceSet| -> Vec<u64> {
            set.workspaces()
                .iter()
                .map(|workspace| workspace.reference)
                .collect()
        };
        let before = references(&set);
        let mut context = fixture.host.world.legacy.clone();
        context.active_workspace = 0;
        fixture.host.update_context(before.clone(), context);
        let mut store = SlotStore::new();
        store.set_summary(0, Summary::text(KEY, "source"));
        store.set_summary(1, Summary::text(KEY, "destination"));

        // The source's only pane moves away, so the source closes and the
        // destination shifts from index 1 to index 0.
        let movement = set
            .move_pane_between(before[0], before[1], PaneId(1), Orientation::Vertical)
            .expect("the pane moves");
        assert!(movement.source_closed);
        assert_eq!(movement.target, 0);

        // The same order the window applies after the drop: reconcile managed
        // content against the new references, then drop what no workspace owns.
        let after = references(&set);
        fixture
            .host
            .reconcile_workspace_content(&mut store, &after, set.active_index());
        store.retain_workspaces(set.len());
        let summaries = store.summaries(0);
        assert_eq!(summaries.len(), 1);
        assert_eq!(summaries[0].text.as_deref(), Some("destination"));
        assert!(store.summaries(1).is_empty());
    }

    #[test]
    fn cached_managed_summaries_follow_surviving_targets_without_remapping_legacy_sources() {
        let fixture = fixture("managed-echo", &[], None);
        let mut store = SlotStore::new();
        for (index, text) in [(0, "closed managed"), (1, "surviving managed")] {
            store.set_summary(index, Summary::text(KEY, text));
        }
        store.set_summary(0, Summary::text("legacy", "legacy index"));
        PluginHost::apply_fragments(
            &mut store,
            crate::slots::Scope::WorkspaceHeader,
            KEY,
            &[serde_json::from_value(json!({"text":"old header"})).unwrap()],
            Refresh::OnEvent,
        );
        PluginHost::apply_fragments(
            &mut store,
            crate::slots::Scope::AppHeader,
            KEY,
            &[serde_json::from_value(json!({"text":"application"})).unwrap()],
            Refresh::OnEvent,
        );
        fixture
            .host
            .reconcile_workspace_content(&mut store, &[20], 0);
        let summaries = store.summaries(0);
        assert_eq!(summaries.len(), 2);
        assert!(summaries
            .iter()
            .any(|summary| summary.text.as_deref() == Some("surviving managed")));
        assert!(summaries
            .iter()
            .any(|summary| summary.text.as_deref() == Some("legacy index")));
        assert!(store.summaries(1).is_empty());
        assert!(store.is_empty(crate::slots::Scope::WorkspaceHeader));
        assert!(!store.is_empty(crate::slots::Scope::AppHeader));
    }

    #[test]
    fn disable_during_configuration_revokes_acknowledgements_until_a_fresh_process() {
        let mut fixture = fixture("managed-held-config", &[], Some(&["status", "settings"]));
        fixture.wait(|host| host.configuration(KEY) == Configuration::Pending);
        let generation = fixture.host.plugins[0]
            .io
            .as_ref()
            .unwrap()
            .generation
            .clone();
        fixture
            .host
            .stored_settings_changed(KEY, json!({"version":1,"values":{"label":"next process"}}));
        fixture.host.disable(KEY);
        assert_eq!(fixture.host.configuration(KEY), Configuration::NotApplied);
        fixture.wait(|host| host.is_stopped());
        assert!(fixture.host.drain_results().is_empty());
        std::fs::write(fixture.package.join("allow-configuration"), b"yes").unwrap();
        fixture.host.enable(KEY);
        fixture.wait(|host| host.configuration(KEY) == Configuration::Applied);
        assert_ne!(
            fixture.host.plugins[0].io.as_ref().unwrap().generation,
            generation
        );
        fixture
            .host
            .request_status(KEY, "app_header", &Context::default())
            .unwrap();
        fixture.wait(|host| !host.results.is_empty());
        assert_eq!(
            fixture.host.drain_results()[0].fragments[0].text.as_deref(),
            Some("next process")
        );
    }
}
