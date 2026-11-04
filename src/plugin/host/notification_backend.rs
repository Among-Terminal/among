//! the only process/core crossing for managed notification roles. all identities
//! come from the current receipt/admission, never an output payload's claim.

use super::*;
use among::plugin_notifications::{self as backend, ChannelKey, Declaration, Icon, Owner};

impl PluginHost {
    pub(super) fn notification_context(&self, index: usize, value: &mut serde_json::Value) {
        if !self.plugins[index].runtime.as_ref().is_some_and(|runtime| {
            runtime.grants.contains("notifications.produce")
                || runtime.grants.contains("notifications.ui")
        }) {
            return;
        }
        let Some(context) = &self.notifications else {
            return;
        };
        let Ok(context) = context.try_borrow() else {
            return;
        };
        if let Some(terminals) = value
            .get_mut("terminals")
            .and_then(serde_json::Value::as_array_mut)
        {
            for terminal in terminals {
                if let Some(source) = terminal["pane"]
                    .as_str()
                    .and_then(|pane| managed::parse_reference(pane).ok())
                    .and_then(|pane| context.source(pane))
                {
                    terminal["notification_generation"] = source.generation().to_string().into();
                }
            }
        }
    }
    pub(crate) fn supported_features(&self) -> Vec<&'static str> {
        let mut supported = managed::FEATURES.to_vec();
        if self.notifications.as_ref().is_some_and(|context| {
            context
                .try_borrow()
                .is_ok_and(|context| context.usable(&self.world.session))
        }) {
            supported.extend_from_slice(backend::FEATURES);
        }
        supported
    }

    pub(super) fn declare_notification_channels(&mut self, index: usize) {
        self.pending_notification_registrations
            .insert(self.plugins[index].spec.name.clone());
        self.reconcile_notification_registrations();
    }

    fn reconcile_notification_registrations(&mut self) {
        let Some(context) = &self.notifications else {
            self.pending_notification_registrations.clear();
            return;
        };
        let Ok(mut context) = context.try_borrow_mut() else {
            return;
        };
        for key in std::mem::take(&mut self.pending_notification_registrations) {
            // Static replacement withdraws channels absent from the new manifest,
            // without erasing the user's independently stored channel policies.
            context.uninstall(&key);
            let Some(plugin) = self
                .plugins
                .iter()
                .find(|plugin| plugin.spec.name == key && !plugin.removed)
            else {
                continue;
            };
            let Some(inspected) = plugin
                .managed
                .as_ref()
                .and_then(|entry| entry.inspected.as_ref().ok())
            else {
                continue;
            };
            if let Err(reason) = context.register_identity(&key) {
                context.reject(reason);
                continue;
            }
            for channel in &inspected.manifest.delivery_channels {
                if let Err(reason) = context.declare(Declaration {
                    key: ChannelKey {
                        identity: key.clone(),
                        channel: channel.id.clone(),
                    },
                    name: channel.name.clone(),
                    description: channel.description.clone(),
                }) {
                    context.reject(reason);
                }
            }
            context.plugin_enabled(&key, plugin.spec.enabled);
        }
    }

    fn notification_owner(
        &self,
        index: usize,
        feature: &str,
        session: &str,
        generation: &str,
    ) -> Option<Owner> {
        let plugin = &self.plugins[index];
        if !plugin.spec.enabled
            || !plugin.state.is_healthy()
            || session != self.world.session
            || !plugin.runtime.as_ref()?.grants.contains(feature)
        {
            return None;
        }
        let io = plugin.io.as_ref()?;
        if !io.handshake_done
            || io.generation != generation
            || io
                .notification_live
                .as_ref()
                .is_none_or(|live| !live.is_current())
        {
            return None;
        }
        Some(Owner {
            identity: plugin.spec.name.clone(),
            generation: io.generation.clone(),
        })
    }

    pub(super) fn admit_notifications(&mut self, index: usize) {
        let plugin = &self.plugins[index];
        let Some(runtime) = &plugin.runtime else {
            return;
        };
        if !backend::FEATURES
            .iter()
            .any(|feature| runtime.grants.contains(*feature))
        {
            return;
        }
        let Some(io) = &plugin.io else {
            return;
        };
        let Some(inspected) = plugin
            .managed
            .as_ref()
            .and_then(|entry| entry.inspected.as_ref().ok())
        else {
            return;
        };
        let owner = Owner {
            identity: plugin.spec.name.clone(),
            generation: io.generation.clone(),
        };
        let icon = match &inspected.manifest.icon {
            Some(super::super::manifest::Icon::Theme(name)) => Some(Icon::Theme(name.clone())),
            Some(super::super::manifest::Icon::File(_)) => inspected
                .raster_icon
                .as_ref()
                .map(|bytes| Icon::Raster(bytes.clone().into())),
            None => None,
        };
        let result = self
            .notifications
            .as_ref()
            .and_then(|context| context.try_borrow_mut().ok())
            .map(|mut context| {
                context.plugin_enabled(&owner.identity, plugin.spec.enabled);
                context.admit(
                    owner,
                    runtime.grants.contains("notifications.deliver"),
                    icon,
                )
            });
        match result {
            Some(Ok(live)) => {
                if let Some(io) = self.plugins[index].io.as_mut() {
                    io.notification_live = Some(live);
                }
            }
            _ => self.fault(
                index,
                "notification backend could not admit this generation",
            ),
        }
    }

    pub(super) fn revoke_notifications(&mut self, index: usize) {
        if let Some(mut context) = self
            .notifications
            .as_ref()
            .and_then(|context| context.try_borrow_mut().ok())
        {
            let plugin = &self.plugins[index];
            context.revoke(&plugin.spec.name);
            context.plugin_enabled(&plugin.spec.name, plugin.spec.enabled);
        }
    }

    pub(super) fn invalidate_notification_content(&mut self, index: usize) {
        if let Some(generation) = self.running_generation(&self.plugins[index].spec.name) {
            if let Some(mut context) = self
                .notifications
                .as_ref()
                .and_then(|context| context.try_borrow_mut().ok())
            {
                context.invalidate_content(&Owner {
                    identity: self.plugins[index].spec.name.clone(),
                    generation,
                });
            }
        }
    }

    pub(super) fn reconcile_notifications(&mut self) {
        self.reconcile_notification_registrations();
        let usable = self.supported_features().contains(&"notifications.produce");
        for index in 0..self.plugins.len() {
            if let Some(mut context) = self
                .notifications
                .as_ref()
                .and_then(|context| context.try_borrow_mut().ok())
            {
                context.plugin_enabled(
                    &self.plugins[index].spec.name,
                    self.plugins[index].spec.enabled,
                );
            }
            let has_roles = self.plugins[index].runtime.as_ref().is_some_and(|runtime| {
                backend::FEATURES
                    .iter()
                    .any(|feature| runtime.grants.contains(*feature))
            });
            if !usable && self.plugins[index].state.is_healthy() && has_roles {
                self.fault(index, "notification backend was detached");
            }
            if !self.plugins[index].state.is_healthy() {
                self.revoke_notifications(index);
            }
        }
        if let Some(mut context) = self
            .notifications
            .as_ref()
            .and_then(|context| context.try_borrow_mut().ok())
        {
            context.retain_scopes(|scope| self.world.resolve(scope).is_some());
            context.housekeep(Instant::now());
        }
    }

    pub(super) fn handle_notification_response(
        &mut self,
        index: usize,
        response: managed::Response,
    ) {
        let now = Instant::now();
        let (feature, session, generation) = match &response {
            managed::Response::NotificationEvent {
                session,
                generation,
                ..
            } => ("notifications.produce", session, generation),
            managed::Response::NotificationContribution {
                session,
                generation,
                ..
            }
            | managed::Response::WithdrawNotification {
                session,
                generation,
                ..
            } => ("notifications.ui", session, generation),
            managed::Response::DeliveryResult {
                session,
                generation,
                ..
            } => ("notifications.deliver", session, generation),
            _ => return,
        };
        let Some(owner) = self.notification_owner(index, feature, session, generation) else {
            if let Some(mut context) = self
                .notifications
                .as_ref()
                .and_then(|context| context.try_borrow_mut().ok())
            {
                context.reject(backend::Rejection::Unauthorized);
            }
            return;
        };
        let Some(context) = self.notifications.clone() else {
            return;
        };
        let Ok(mut context) = context.try_borrow_mut() else {
            return;
        };
        let grants = &self.plugins[index]
            .runtime
            .as_ref()
            .map(|runtime| &runtime.grants);
        let terminal_grant = grants.is_some_and(|grants| grants.contains("context.terminals"));
        let result = match response {
            managed::Response::NotificationEvent { event, .. } => {
                let workspace = match event.target {
                    managed::Target::Application => None,
                    managed::Target::Pane { reference } if terminal_grant => self
                        .world
                        .legacy
                        .terminals
                        .iter()
                        .find(|terminal| terminal.pane == reference)
                        .and_then(|terminal| self.world.workspaces.get(terminal.workspace))
                        .copied(),
                    _ => {
                        context.reject(backend::Rejection::Unauthorized);
                        return;
                    }
                };
                if event.target != managed::Target::Application && workspace.is_none() {
                    context.reject(backend::Rejection::Stale);
                    return;
                }
                context.produce(&owner, event, workspace, now).map(|_| ())
            }
            managed::Response::NotificationContribution { contribution, .. } => {
                if self.world.resolve(&contribution.scope).is_none()
                    || (!terminal_grant
                        && contribution.scope.target != managed::Target::Application)
                {
                    context.reject(backend::Rejection::Unauthorized);
                    return;
                }
                let source = match contribution.scope.target {
                    managed::Target::Pane { reference } => context_source(&context, reference),
                    _ => None,
                };
                if matches!(contribution.scope.target, managed::Target::Pane { .. })
                    && source.is_none()
                {
                    context.reject(backend::Rejection::Stale);
                    return;
                }
                context.contribute(&owner, contribution, source, now)
            }
            managed::Response::WithdrawNotification { id, .. } => {
                context.withdraw(&owner, &id);
                Ok(())
            }
            managed::Response::DeliveryResult {
                job_id, outcome, ..
            } => {
                // cancelled/expired replies consume only their retained transport
                // credit, not authority to replace the recorded outcome.
                let retained = self.plugins[index]
                    .runtime
                    .as_mut()
                    .and_then(|runtime| runtime.deliveries.remove(&job_id));
                if retained.is_some() {
                    context.result(&owner, job_id, outcome, now);
                }
                Ok(())
            }
            _ => return,
        };
        if let Err(reason) = result {
            context.reject(reason);
        }
    }

    pub(super) fn poll_notifications(&mut self) {
        let Some(context) = self.notifications.clone() else {
            return;
        };
        let now = Instant::now();
        self.reconcile_notifications();
        // rotating with normal process polling prevents a permanently ready
        // provider from monopolizing the two shared active provider slots.
        for offset in 0..self.plugins.len() {
            let index = (self.next_poll + offset) % self.plugins.len();
            let plugin = &self.plugins[index];
            let Some(io) = &plugin.io else {
                continue;
            };
            let Some(owner) = self.notification_owner(
                index,
                "notifications.deliver",
                &self.world.session,
                &io.generation,
            ) else {
                continue;
            };
            let Some(runtime) = &plugin.runtime else {
                continue;
            };
            if runtime
                .deliveries
                .values()
                .any(|sent| sent.elapsed() >= backend::DEADLINE)
            {
                // stop before releasing any reply credit. a timeout may follow
                // external side effects; reconnect cannot replay these jobs.
                self.fault(index, "notification provider exceeded delivery deadline");
                continue;
            }
            if !io.process.has_capacity()
                || plugin.inflight.len() + runtime.configurations.len() + runtime.deliveries.len()
                    >= managed_runtime::MAX_MANAGED_REPLIES
            {
                continue;
            }
            let delivery = context
                .try_borrow_mut()
                .ok()
                .and_then(|mut context| context.take_delivery(&owner, now));
            let Some(mut delivery) = delivery else {
                continue;
            };
            if !runtime.grants.contains("context.terminals") {
                delivery.event.source = None;
            }
            let id = delivery.job_id;
            let request = managed::Request::Deliver {
                session: self.world.session.clone(),
                generation: owner.generation.clone(),
                delivery,
            };
            if super::super::data::serialized_size(&request, backend::MAX_PAYLOAD_BYTES).is_err() {
                if let Ok(mut context) = context.try_borrow_mut() {
                    context.not_enqueued(&owner, id);
                }
                self.fault(
                    index,
                    "notification delivery payload exceeded its byte budget",
                );
                continue;
            }
            match io.process.send(&request) {
                Ok(()) => {
                    if let Some(runtime) = self.plugins[index].runtime.as_mut() {
                        runtime.deliveries.insert(id, now);
                    }
                }
                Err(error) => {
                    if let Ok(mut context) = context.try_borrow_mut() {
                        context.not_enqueued(&owner, id);
                    }
                    if let SendError::Fault(reason) = error {
                        self.fault(index, reason);
                    }
                }
            }
        }
        for _ in 0..16 {
            let cancellation = context
                .try_borrow_mut()
                .ok()
                .and_then(|mut context| context.take_cancellation());
            let Some((owner, job_id)) = cancellation else {
                break;
            };
            if let Some(index) = self.find(&owner.identity) {
                if self.running_generation(&owner.identity).as_deref() == Some(&owner.generation) {
                    let request = managed::Request::CancelDelivery {
                        session: self.world.session.clone(),
                        generation: owner.generation,
                        job_id,
                    };
                    if self.plugins[index]
                        .io
                        .as_ref()
                        .is_some_and(|io| io.process.send(&request).is_err())
                    {
                        self.fault(index, "provider cancellation input budget exceeded");
                    }
                }
            }
        }
        for _ in 0..16 {
            let action = context
                .try_borrow_mut()
                .ok()
                .and_then(|mut context| context.take_action(now));
            let Some((owner, contribution, revision, action)) = action else {
                break;
            };
            let Some(index) = self.find(&owner.identity) else {
                continue;
            };
            if self
                .notification_owner(
                    index,
                    "notifications.ui",
                    &self.world.session,
                    &owner.generation,
                )
                .is_none()
            {
                continue;
            }
            let request = managed::Request::NotificationAction {
                session: self.world.session.clone(),
                generation: owner.generation,
                contribution,
                revision,
                action,
            };
            if self.plugins[index]
                .io
                .as_ref()
                .is_some_and(|io| io.process.send(&request).is_err())
            {
                self.fault(index, "notification action input budget exceeded");
            }
        }
    }
}

fn context_source(context: &backend::Backend, pane: u64) -> Option<among::notifications::Source> {
    context.source(pane)
}

#[cfg(test)]
mod tests;
