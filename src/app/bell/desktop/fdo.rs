//! Asynchronous freedesktop notification delivery.
//!
//! GIO's freedesktop backend records a notification only after the `Notify`
//! reply, so a withdrawal issued in between is lost. This route owns the
//! asynchronous call and binds the server id to the unique service owner.

use super::target_variant;
use among::notifications::desktop::{Target, Text};
use among::notifications::MAX_PENDING;
use gio::prelude::*;
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;
use std::time::{Duration, Instant};

const FDO: &str = "org.freedesktop.Notifications";
const PATH: &str = "/org/freedesktop/Notifications";
const CALL_TIMEOUT_MS: i32 = 25_000;
const DRAIN_BUDGET: Duration = Duration::from_secs(30);

#[derive(Default)]
struct FdoState {
    in_flight: BTreeMap<String, Request>,
    desired: BTreeMap<String, Desired>,
    posted: BTreeMap<String, Posted>,
    retiring: BTreeMap<(String, u32), Retiring>,
    current_owner: Option<String>,
    owner_ever_seen: bool,
    next_seq: u64,
    shutdown: bool,
    deadline: Option<Instant>,
    drain_budget: Option<Duration>,
    drain_expired: bool,
    close_in_flight: usize,
}

struct Request {
    seq: u64,
    cancelled: bool,
    owner_hint: Option<String>,
    target: Target,
}

struct Desired {
    text: Text,
    target: Target,
}

#[derive(Clone)]
struct Retiring {
    target: Target,
    token: Option<String>,
}

#[derive(Clone)]
struct Posted {
    owner: String,
    server_id: u32,
    target: Target,
    token: Option<String>,
}

struct FdoShared {
    connection: gio::DBusConnection,
    application: glib::WeakRef<gio::Application>,
    state: RefCell<FdoState>,
    cancellable: gio::Cancellable,
}

pub(super) struct FdoSender {
    shared: Rc<FdoShared>,
    cleanup: Vec<Box<dyn FnOnce()>>,
}

impl FdoSender {
    pub(super) fn new(application: &gio::Application) -> Option<Self> {
        let connection = application.dbus_connection()?;
        let shared = Rc::new(FdoShared {
            connection: connection.clone(),
            application: application.downgrade(),
            state: RefCell::new(FdoState::default()),
            cancellable: gio::Cancellable::new(),
        });

        let subscription = connection.subscribe_to_signal(
            // Listen to the interface/path independently of the current
            // well-known owner: a live service that just released the name may
            // still emit a late ActivationToken/ActionInvoked from its unique
            // name. on_signal authorizes every signal by an exact
            // (unique owner, server id) Posted/Retiring record, so an unrelated
            // sender cannot activate a target.
            None,
            Some(FDO),
            None,
            Some(PATH),
            None,
            gio::DBusSignalFlags::NONE,
            {
                let shared = shared.clone();
                move |signal| {
                    shared.on_signal(signal.sender_name, signal.signal_name, signal.parameters);
                }
            },
        );

        let watcher = gio::bus_watch_name(
            gio::BusType::Session,
            FDO,
            gio::BusNameWatcherFlags::NONE,
            {
                let shared = shared.clone();
                move |_, _, owner: &str| shared.on_appeared(owner)
            },
            {
                let shared = shared.clone();
                move |_, _| shared.on_vanished()
            },
        );

        Some(Self {
            shared,
            cleanup: vec![
                Box::new(move || drop(subscription)) as Box<dyn FnOnce()>,
                Box::new(move || gio::bus_unwatch_name(watcher)) as Box<dyn FnOnce()>,
            ],
        })
    }

    pub(super) fn notify(&mut self, id: &str, text: &Text, target: &Target) {
        self.shared.notify(id, text.clone(), target.clone());
    }

    pub(super) fn withdraw(&mut self, id: &str) {
        self.shared.withdraw(id);
    }

    pub(super) fn shutdown(&mut self) {
        self.shared.shutdown();
    }

    pub(super) fn is_quiescent(&self) -> bool {
        self.shared.is_quiescent()
    }

    #[cfg(test)]
    pub(super) fn set_drain_budget_for_test(&mut self, budget: Duration) {
        self.shared.state.borrow_mut().drain_budget = Some(budget);
    }

    #[cfg(test)]
    pub(super) fn owner_for_test(&self) -> Option<String> {
        self.shared.state.borrow().current_owner.clone()
    }

    #[cfg(test)]
    pub(super) fn has_retired_for_test(&self, owner: &str, server_id: u32) -> bool {
        self.shared
            .state
            .borrow()
            .retiring
            .contains_key(&(owner.to_owned(), server_id))
    }
}

impl Drop for FdoSender {
    fn drop(&mut self) {
        {
            let mut state = self.shared.state.borrow_mut();
            state.shutdown = true;
            state.desired.clear();
            for request in state.in_flight.values_mut() {
                request.cancelled = true;
            }
        }
        self.shared.cancellable.cancel();
        for cleanup in self.cleanup.drain(..) {
            cleanup();
        }
    }
}

impl FdoShared {
    fn notify(self: &Rc<Self>, local_id: &str, text: Text, target: Target) {
        let Some(timeout) = self.timeout_ms() else {
            self.expire();
            return;
        };
        let (seq, replace_id, owner_hint) = {
            let mut state = self.state.borrow_mut();
            if state.shutdown {
                return;
            }
            if state.in_flight.contains_key(local_id) {
                state
                    .desired
                    .insert(local_id.to_owned(), Desired { text, target });
                return;
            }
            if state.in_flight.len() >= MAX_PENDING {
                eprintln!("among: freedesktop notification bound reached; bell not sent");
                return;
            }
            let replace_id = state
                .posted
                .get(local_id)
                .filter(|posted| state.current_owner.as_deref() == Some(posted.owner.as_str()))
                .map_or(0, |posted| posted.server_id);
            let seq = state.next_seq;
            state.next_seq = state.next_seq.saturating_add(1);
            let owner_hint = state.current_owner.clone();
            state.in_flight.insert(
                local_id.to_owned(),
                Request {
                    seq,
                    cancelled: false,
                    owner_hint: owner_hint.clone(),
                    target: target.clone(),
                },
            );
            (seq, replace_id, owner_hint)
        };
        self.send_notify(
            local_id.to_owned(),
            seq,
            text,
            replace_id,
            owner_hint,
            timeout,
        );
    }

    fn withdraw(self: &Rc<Self>, local_id: &str) {
        let posted = {
            let mut state = self.state.borrow_mut();
            if let Some(request) = state.in_flight.get_mut(local_id) {
                request.cancelled = true;
            }
            state.desired.remove(local_id);
            let posted = state.posted.remove(local_id);
            if let Some(posted) = &posted {
                state.retiring.insert(
                    (posted.owner.clone(), posted.server_id),
                    Retiring {
                        target: posted.target.clone(),
                        token: posted.token.clone(),
                    },
                );
            }
            posted
        };
        if let Some(posted) = posted {
            self.send_close(posted.owner, posted.server_id);
        }
    }

    fn shutdown(&self) {
        let mut state = self.state.borrow_mut();
        state.shutdown = true;
        state.desired.clear();
        for request in state.in_flight.values_mut() {
            request.cancelled = true;
        }
        if state.deadline.is_none() {
            let budget = state.drain_budget.unwrap_or(DRAIN_BUDGET);
            state.deadline = Some(
                Instant::now()
                    .checked_add(budget)
                    .unwrap_or_else(Instant::now),
            );
        }
    }

    fn is_quiescent(&self) -> bool {
        let mut state = self.state.borrow_mut();
        if let Some(deadline) = state.deadline {
            if Instant::now() >= deadline {
                if !state.drain_expired {
                    state.drain_expired = true;
                    eprintln!(
                        "among: notification drain budget expired; late delivery is not claimed withdrawn"
                    );
                }
                state.in_flight.clear();
                state.desired.clear();
                state.posted.clear();
                state.retiring.clear();
                state.close_in_flight = 0;
                return true;
            }
        }
        state.in_flight.is_empty()
            && state.close_in_flight == 0
            && state.posted.is_empty()
            && state.retiring.is_empty()
    }

    fn timeout_ms(&self) -> Option<i32> {
        let state = self.state.borrow();
        match state.deadline {
            None => Some(CALL_TIMEOUT_MS),
            Some(deadline) => {
                let now = Instant::now();
                if now >= deadline {
                    None
                } else {
                    let remaining = deadline
                        .saturating_duration_since(now)
                        .as_millis()
                        .min(CALL_TIMEOUT_MS as u128) as i32;
                    Some(remaining.max(1))
                }
            }
        }
    }

    fn expire(&self) {
        let mut state = self.state.borrow_mut();
        if !state.drain_expired {
            state.drain_expired = true;
            eprintln!(
                "among: notification drain budget expired; late delivery is not claimed withdrawn"
            );
        }
        state.in_flight.clear();
        state.desired.clear();
        state.posted.clear();
        state.retiring.clear();
        state.close_in_flight = 0;
    }

    fn send_notify(
        self: &Rc<Self>,
        local_id: String,
        seq: u64,
        text: Text,
        replace_id: u32,
        destination: Option<String>,
        timeout: i32,
    ) {
        let Some(application) = self.application.upgrade() else {
            self.fail(&local_id, seq, "application disappeared");
            return;
        };
        let Some(application_id) = application.application_id() else {
            self.fail(&local_id, seq, "application id is absent");
            return;
        };
        let app_name = glib::application_name()
            .map(|name| name.to_string())
            .unwrap_or_default();
        let actions = vec!["default".to_string(), String::new()];
        let mut hints = BTreeMap::new();
        hints.insert(
            "desktop-entry".to_string(),
            application_id.as_str().to_variant(),
        );
        hints.insert("urgency".to_string(), 1_u8.to_variant());
        hints.insert(
            "image-path".to_string(),
            "com.github.among-terminal".to_variant(),
        );
        let body = (
            app_name, replace_id, "", text.title, text.body, actions, hints, -1_i32,
        )
            .to_variant();
        let message = gio::DBusMessage::new_method_call(
            Some(destination.as_deref().unwrap_or(FDO)),
            PATH,
            Some(FDO),
            "Notify",
        );
        message.set_body(&body);
        let shared = self.clone();
        self.connection.send_message_with_reply(
            &message,
            gio::DBusSendMessageFlags::NONE,
            timeout,
            Some(&self.cancellable),
            move |result| shared.on_notify_reply(local_id, seq, result),
        );
    }

    fn on_notify_reply(
        self: &Rc<Self>,
        local_id: String,
        seq: u64,
        result: Result<gio::DBusMessage, glib::Error>,
    ) {
        let message = match result {
            Ok(message) => message,
            Err(error) => {
                self.fail(&local_id, seq, &error.to_string());
                return;
            }
        };
        if message.message_type() == gio::DBusMessageType::Error {
            let name = message
                .error_name()
                .map_or_else(|| "error reply".to_string(), |name| name.to_string());
            let detail = message
                .body()
                .and_then(|body| body.get::<(String,)>())
                .map_or_else(String::new, |(detail,)| detail);
            self.fail(&local_id, seq, &format!("{name}: {detail}"));
            return;
        }
        let Some(body) = message.body() else {
            self.fail(&local_id, seq, "reply has no body");
            return;
        };
        let Some((server_id,)) = body.get::<(u32,)>() else {
            self.fail(&local_id, seq, "reply is not a server id");
            return;
        };
        if server_id == 0 {
            self.fail(&local_id, seq, "reply has server id zero");
            return;
        }
        let Some(owner) = message.sender().map(|sender| sender.to_string()) else {
            self.fail(&local_id, seq, "reply has no unique sender");
            return;
        };

        enum Action {
            Nothing,
            Close {
                owner: String,
                server_id: u32,
                desired: Option<Desired>,
            },
            Follow {
                owner: String,
                seq: u64,
                text: Text,
                replace_id: u32,
            },
        }

        let action = {
            let mut state = self.state.borrow_mut();
            let Some(request) = state.in_flight.get(&local_id) else {
                return;
            };
            if request.seq != seq {
                return;
            }
            let Some(request) = state.in_flight.remove(&local_id) else {
                return;
            };
            let owner_matches = state.current_owner.as_deref() == Some(owner.as_str())
                || (state.current_owner.is_none() && request.owner_hint.is_none());
            let previous_token = replacement_token(&state, &local_id, &owner, server_id);
            if request.cancelled || !owner_matches {
                Action::Close {
                    owner,
                    server_id,
                    desired: state.desired.remove(&local_id),
                }
            } else if let Some(desired) = state.desired.remove(&local_id) {
                // Keep the first banner visible and addressable while the
                // replacement is in flight. Without this record withdraw and
                // failure cannot close the visible id 37.
                state.retiring.remove(&(owner.clone(), server_id));
                state.posted.insert(
                    local_id.clone(),
                    Posted {
                        owner: owner.clone(),
                        server_id,
                        target: request.target.clone(),
                        token: previous_token.clone(),
                    },
                );
                let next_seq = state.next_seq;
                state.next_seq = state.next_seq.saturating_add(1);
                let owner_hint = state.current_owner.clone();
                state.in_flight.insert(
                    local_id.clone(),
                    Request {
                        seq: next_seq,
                        cancelled: false,
                        owner_hint,
                        target: desired.target.clone(),
                    },
                );
                Action::Follow {
                    owner,
                    seq: next_seq,
                    text: desired.text,
                    replace_id: server_id,
                }
            } else {
                state.retiring.remove(&(owner.clone(), server_id));
                state.posted.insert(
                    local_id.clone(),
                    Posted {
                        owner,
                        server_id,
                        target: request.target,
                        token: previous_token,
                    },
                );
                Action::Nothing
            }
        };

        match action {
            Action::Nothing => {}
            Action::Close {
                owner,
                server_id,
                desired,
            } => {
                self.send_close(owner, server_id);
                if let Some(desired) = desired {
                    self.notify(&local_id, desired.text, desired.target);
                }
            }
            Action::Follow {
                owner,
                seq,
                text,
                replace_id,
            } => {
                if let Some(timeout) = self.timeout_ms() {
                    self.send_notify(local_id, seq, text, replace_id, Some(owner), timeout);
                } else {
                    self.expire();
                }
            }
        }
    }

    fn send_close(self: &Rc<Self>, owner: String, server_id: u32) {
        if owner.is_empty() {
            eprintln!("among: freedesktop CloseNotification has no unique owner");
            return;
        }
        let Some(timeout) = self.timeout_ms() else {
            self.expire();
            return;
        };
        {
            let mut state = self.state.borrow_mut();
            state.close_in_flight = state.close_in_flight.saturating_add(1);
        }
        let body = (server_id,).to_variant();
        let shared = self.clone();
        let owner_for_reply = owner.clone();
        self.connection.call(
            Some(owner.as_str()),
            PATH,
            FDO,
            "CloseNotification",
            Some(&body),
            Some(&<()>::static_variant_type()),
            gio::DBusCallFlags::NONE,
            timeout,
            Some(&self.cancellable),
            move |result| shared.on_close_reply(owner_for_reply, server_id, result),
        );
    }

    fn on_close_reply(
        &self,
        owner: String,
        server_id: u32,
        result: Result<glib::Variant, glib::Error>,
    ) {
        {
            let mut state = self.state.borrow_mut();
            state.close_in_flight = state.close_in_flight.saturating_sub(1);
            state.retiring.remove(&(owner, server_id));
        }
        match result {
            Ok(value) if value.type_().as_str() != "()" => {
                eprintln!(
                    "among: freedesktop CloseNotification returned malformed reply type {}",
                    value.type_()
                );
            }
            Ok(_) => {}
            Err(error) => {
                eprintln!("among: freedesktop CloseNotification failed: {error}");
            }
        }
    }

    fn fail(&self, local_id: &str, seq: u64, reason: &str) {
        {
            let mut state = self.state.borrow_mut();
            if state
                .in_flight
                .get(local_id)
                .is_some_and(|request| request.seq == seq)
            {
                state.in_flight.remove(local_id);
            }
            state.desired.remove(local_id);
        }
        eprintln!("among: freedesktop notification {local_id} failed: {reason}");
    }

    fn on_appeared(self: &Rc<Self>, owner: &str) {
        let mut closes = Vec::new();
        {
            let mut state = self.state.borrow_mut();
            transition_appeared(&mut state, owner);
            let stale: Vec<String> = state
                .posted
                .iter()
                .filter(|(_, posted)| posted.owner != owner)
                .map(|(local_id, _)| local_id.clone())
                .collect();
            for local_id in stale {
                if let Some(posted) = state.posted.remove(&local_id) {
                    closes.push((posted.owner, posted.server_id));
                }
            }
        }
        for (owner, server_id) in closes {
            self.send_close(owner, server_id);
        }
    }

    fn on_vanished(self: &Rc<Self>) {
        let mut closes = Vec::new();
        {
            let mut state = self.state.borrow_mut();
            if let Some(previous) = state.current_owner.take() {
                cancel_owner(&mut state, &previous);
            }
            let stale: Vec<String> = state.posted.keys().cloned().collect();
            for local_id in stale {
                if let Some(posted) = state.posted.remove(&local_id) {
                    closes.push((posted.owner, posted.server_id));
                }
            }
        }
        for (owner, server_id) in closes {
            self.send_close(owner, server_id);
        }
    }

    fn on_signal(&self, sender: &str, signal: &str, parameters: &glib::Variant) {
        if self.state.borrow().shutdown {
            return;
        }
        match signal {
            "NotificationClosed" => {
                let Some((server_id, _reason)) = parameters.get::<(u32, u32)>() else {
                    return;
                };
                let mut state = self.state.borrow_mut();
                remove_posted(&mut state, sender, server_id);
                state.retiring.remove(&(sender.to_string(), server_id));
            }
            "ActivationToken" => {
                let Some((server_id, token)) = parameters.get::<(u32, String)>() else {
                    return;
                };
                let mut state = self.state.borrow_mut();
                if let Some(posted) = find_posted_mut(&mut state, sender, server_id) {
                    posted.token = Some(token);
                } else if let Some(retiring) = find_retiring_mut(&mut state, sender, server_id) {
                    retiring.token = Some(token);
                }
            }
            "ActionInvoked" => {
                let Some((server_id, action)) = parameters.get::<(u32, String)>() else {
                    return;
                };
                if action != "default" {
                    return;
                }
                let (target, token) = {
                    let state = self.state.borrow();
                    if let Some(posted) = find_posted(&state, sender, server_id) {
                        (posted.target.clone(), posted.token.clone())
                    } else if let Some(retiring) = find_retiring(&state, sender, server_id) {
                        (retiring.target.clone(), retiring.token.clone())
                    } else {
                        return;
                    }
                };
                self.activate(&target, token.as_deref());
            }
            _ => {}
        }
    }

    fn activate(&self, target: &Target, token: Option<&str>) {
        let Some(application) = self.application.upgrade() else {
            return;
        };
        let Some(object_path) = application.dbus_object_path() else {
            return;
        };
        let Some(destination) = self.connection.unique_name() else {
            return;
        };
        let mut platform = BTreeMap::new();
        if let Some(token) = token {
            platform.insert("desktop-startup-id".to_string(), token.to_variant());
            platform.insert("activation-token".to_string(), token.to_variant());
        }
        let parameters = activation_parameters(target, platform);
        self.connection.call(
            Some(destination.as_str()),
            object_path.as_str(),
            "org.freedesktop.Application",
            "ActivateAction",
            Some(&parameters),
            None,
            gio::DBusCallFlags::NONE,
            5_000,
            Some(&self.cancellable),
            |result| {
                if let Err(error) = result {
                    eprintln!("among: freedesktop ActivateAction failed: {error}");
                }
            },
        );
    }
}

fn cancel_owner(state: &mut FdoState, owner: &str) {
    for request in state.in_flight.values_mut() {
        if request.owner_hint.as_deref() == Some(owner) {
            request.cancelled = true;
        }
    }
}

fn transition_appeared(state: &mut FdoState, owner: &str) {
    let previous = state.current_owner.replace(owner.to_owned());
    // Every appearance may be the activation result for a request sent while
    // the name had no owner. Bind those requests without cancelling them, then
    // cancel only requests that belonged to the previous owner.
    for request in state.in_flight.values_mut() {
        if request.owner_hint.is_none() {
            request.owner_hint = Some(owner.to_owned());
        }
    }
    if state.owner_ever_seen {
        if let Some(previous) = previous {
            if previous != owner {
                cancel_owner(state, &previous);
            }
        }
    } else {
        state.owner_ever_seen = true;
    }
}

fn activation_parameters(
    target: &Target,
    platform: BTreeMap<String, glib::Variant>,
) -> glib::Variant {
    ("focus-pane", vec![target_variant(target)], platform).to_variant()
}

fn replacement_token(
    state: &FdoState,
    local_id: &str,
    owner: &str,
    server_id: u32,
) -> Option<String> {
    state
        .posted
        .get(local_id)
        .filter(|posted| posted.owner == owner && posted.server_id == server_id)
        .and_then(|posted| posted.token.clone())
}

fn find_posted<'a>(state: &'a FdoState, owner: &str, server_id: u32) -> Option<&'a Posted> {
    state
        .posted
        .values()
        .find(|posted| posted.owner == owner && posted.server_id == server_id)
}

fn find_posted_mut<'a>(
    state: &'a mut FdoState,
    owner: &str,
    server_id: u32,
) -> Option<&'a mut Posted> {
    state
        .posted
        .values_mut()
        .find(|posted| posted.owner == owner && posted.server_id == server_id)
}

fn find_retiring<'a>(state: &'a FdoState, owner: &str, server_id: u32) -> Option<&'a Retiring> {
    state.retiring.get(&(owner.to_string(), server_id))
}

fn find_retiring_mut<'a>(
    state: &'a mut FdoState,
    owner: &str,
    server_id: u32,
) -> Option<&'a mut Retiring> {
    state.retiring.get_mut(&(owner.to_string(), server_id))
}

fn remove_posted(state: &mut FdoState, owner: &str, server_id: u32) {
    let key = state
        .posted
        .iter()
        .find(|(_, posted)| posted.owner == owner && posted.server_id == server_id)
        .map(|(local_id, _)| local_id.clone());
    if let Some(key) = key {
        state.posted.remove(&key);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target() -> Target {
        Target {
            instance: "test".to_string(),
            session: 1,
            generation: 2,
            pane: 3,
        }
    }

    fn insert(state: &mut FdoState, local_id: &str, owner: Option<&str>) {
        state.in_flight.insert(
            local_id.to_string(),
            Request {
                seq: 0,
                cancelled: false,
                owner_hint: owner.map(str::to_string),
                target: target(),
            },
        );
    }

    #[test]
    fn first_owner_after_activation_binds_without_cancelling() {
        let mut state = FdoState::default();
        insert(&mut state, "first", None);
        transition_appeared(&mut state, ":1.7");
        assert_eq!(state.current_owner.as_deref(), Some(":1.7"));
        assert!(!state.in_flight["first"].cancelled);
        assert_eq!(state.in_flight["first"].owner_hint.as_deref(), Some(":1.7"));
    }

    #[test]
    fn known_owner_replacement_cancels_old_requests_only() {
        let mut state = FdoState {
            owner_ever_seen: true,
            current_owner: Some(":1.1".to_string()),
            ..Default::default()
        };
        insert(&mut state, "old", Some(":1.1"));
        insert(&mut state, "unowned", None);
        transition_appeared(&mut state, ":1.2");
        assert!(state.in_flight["old"].cancelled);
        assert!(!state.in_flight["unowned"].cancelled);
        assert_eq!(state.current_owner.as_deref(), Some(":1.2"));
    }

    #[test]
    fn owner_reappearance_after_vanish_binds_then_cancels_on_next_change() {
        let mut state = FdoState {
            owner_ever_seen: true,
            ..Default::default()
        };
        insert(&mut state, "after-vanish", None);
        transition_appeared(&mut state, ":1.9");
        assert!(!state.in_flight["after-vanish"].cancelled);
        assert_eq!(
            state.in_flight["after-vanish"].owner_hint.as_deref(),
            Some(":1.9")
        );
        transition_appeared(&mut state, ":2.0");
        assert!(state.in_flight["after-vanish"].cancelled);
    }

    #[test]
    fn activation_parameters_carry_the_token_and_target() {
        let mut platform = BTreeMap::new();
        platform.insert("desktop-startup-id".to_string(), "tok".to_variant());
        platform.insert("activation-token".to_string(), "tok".to_variant());
        let parameters = activation_parameters(&target(), platform);
        let (action, arguments, platform) = parameters
            .get::<(String, Vec<glib::Variant>, BTreeMap<String, glib::Variant>)>()
            .unwrap();
        assert_eq!(action, "focus-pane");
        assert_eq!(arguments.len(), 1);
        assert_eq!(
            platform
                .get("activation-token")
                .and_then(|value| value.get::<String>()),
            Some("tok".to_string())
        );
    }

    #[test]
    fn replacement_keeps_token_only_for_same_owner_and_server_id() {
        let mut state = FdoState::default();
        state.posted.insert(
            "pane".to_string(),
            Posted {
                owner: ":1.1".to_string(),
                server_id: 37,
                target: target(),
                token: Some("tok".to_string()),
            },
        );
        assert_eq!(
            replacement_token(&state, "pane", ":1.1", 37),
            Some("tok".to_string())
        );
        assert_eq!(replacement_token(&state, "pane", ":1.1", 38), None);
        assert_eq!(replacement_token(&state, "pane", ":1.2", 37), None);
    }
}
