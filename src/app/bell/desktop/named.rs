//! Direct GTK and portal notification senders.
//!
//! Both routes use the small public D-Bus surface of their service on the
//! application's existing connection. GTK is addressed by the owned unique
//! name so removals cannot cross an owner change. The portal is addressed by
//! its well-known name and may start through D-Bus activation.

use super::target_variant;
use among::notifications::desktop::{Target, Text};
use gio::prelude::*;
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;
use std::time::{Duration, Instant};

use super::PORTAL;

const GTK_PATH: &str = "/org/gtk/Notifications";
const GTK_INTERFACE: &str = "org.gtk.Notifications";
const PORTAL_PATH: &str = "/org/freedesktop/portal/desktop";
const PORTAL_INTERFACE: &str = "org.freedesktop.portal.Notification";
const CALL_TIMEOUT_MS: i32 = 25_000;
const DRAIN_BUDGET: Duration = Duration::from_secs(30);

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Destination {
    Gtk(String),
    Portal,
}

#[derive(Default)]
struct NamedState {
    posted: BTreeMap<String, Destination>,
    add_in_flight: usize,
    remove_in_flight: usize,
    shutdown: bool,
    deadline: Option<Instant>,
    drain_budget: Option<Duration>,
    drain_expired: bool,
    #[cfg(test)]
    add_errors: usize,
    #[cfg(test)]
    remove_errors: usize,
}

struct NamedShared {
    connection: gio::DBusConnection,
    application_id: String,
    state: RefCell<NamedState>,
    cancellable: gio::Cancellable,
}

pub(super) struct NamedSender {
    shared: Rc<NamedShared>,
}

impl NamedSender {
    pub(super) fn new(application: &gio::Application) -> Option<Self> {
        let connection = application.dbus_connection()?;
        let application_id = application.application_id()?.to_string();
        Some(Self {
            shared: Rc::new(NamedShared {
                connection,
                application_id,
                state: RefCell::new(NamedState::default()),
                cancellable: gio::Cancellable::new(),
            }),
        })
    }

    pub(super) fn notify(
        &mut self,
        id: &str,
        text: &Text,
        target: &Target,
        destination: Destination,
    ) {
        self.shared
            .notify(id, text.clone(), target.clone(), destination);
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
    pub(super) fn error_counts(&self) -> (usize, usize) {
        let state = self.shared.state.borrow();
        (state.add_errors, state.remove_errors)
    }
}

impl Drop for NamedSender {
    fn drop(&mut self) {
        self.shared.cancellable.cancel();
    }
}

impl NamedShared {
    fn notify(self: &Rc<Self>, id: &str, text: Text, target: Target, destination: Destination) {
        let previous = {
            let mut state = self.state.borrow_mut();
            if state.shutdown {
                return;
            }
            state.posted.insert(id.to_owned(), destination.clone())
        };
        if let Some(previous) = previous {
            if previous != destination {
                // A later id must not inherit the old owner. Close the old
                // notification at its own unique owner before the new Add.
                self.send_remove(id, previous);
            }
        }
        self.send_add(id, text, target, destination);
    }

    fn withdraw(self: &Rc<Self>, id: &str) {
        let destination = self.state.borrow_mut().posted.remove(id);
        if let Some(destination) = destination {
            self.send_remove(id, destination);
        }
    }

    fn shutdown(self: &Rc<Self>) {
        let posted = {
            let mut state = self.state.borrow_mut();
            state.shutdown = true;
            if state.deadline.is_none() {
                let budget = state.drain_budget.unwrap_or(DRAIN_BUDGET);
                state.deadline = Some(
                    Instant::now()
                        .checked_add(budget)
                        .unwrap_or_else(Instant::now),
                );
            }
            std::mem::take(&mut state.posted)
        };
        for (id, destination) in posted {
            self.send_remove(&id, destination);
        }
    }

    fn is_quiescent(&self) -> bool {
        let mut state = self.state.borrow_mut();
        if let Some(deadline) = state.deadline {
            if Instant::now() >= deadline {
                if !state.drain_expired {
                    state.drain_expired = true;
                    eprintln!(
                        "among: notification named-sender drain budget expired; late delivery is not claimed withdrawn"
                    );
                }
                state.posted.clear();
                state.add_in_flight = 0;
                state.remove_in_flight = 0;
                return true;
            }
        }
        state.posted.is_empty() && state.add_in_flight == 0 && state.remove_in_flight == 0
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
                "among: notification named-sender drain budget expired; late delivery is not claimed withdrawn"
            );
        }
        state.posted.clear();
        state.add_in_flight = 0;
        state.remove_in_flight = 0;
    }

    fn send_add(self: &Rc<Self>, id: &str, text: Text, target: Target, destination: Destination) {
        let Some(timeout) = self.timeout_ms() else {
            self.expire();
            return;
        };
        let payload = serialized_payload(&text, &target);
        let (bus_name, path, interface, parameters) = match &destination {
            Destination::Gtk(owner) => (
                owner.as_str(),
                GTK_PATH,
                GTK_INTERFACE,
                gtk_add_parameters(self.application_id.as_str(), id, payload),
            ),
            Destination::Portal => (
                PORTAL,
                PORTAL_PATH,
                PORTAL_INTERFACE,
                portal_add_parameters(id, payload),
            ),
        };
        self.state.borrow_mut().add_in_flight += 1;
        let shared = Rc::clone(self);
        let destination_for_callback = destination.clone();
        self.connection.call(
            Some(bus_name),
            path,
            interface,
            "AddNotification",
            Some(&parameters),
            Some(&<()>::static_variant_type()),
            gio::DBusCallFlags::NONE,
            timeout,
            Some(&self.cancellable),
            move |result| {
                shared.on_add_reply(&destination_for_callback, result);
            },
        );
    }

    fn send_remove(self: &Rc<Self>, id: &str, destination: Destination) {
        let Some(timeout) = self.timeout_ms() else {
            self.expire();
            return;
        };
        self.state.borrow_mut().remove_in_flight += 1;
        let (bus_name, path, interface, parameters) = match &destination {
            Destination::Gtk(owner) => (
                owner.as_str(),
                GTK_PATH,
                GTK_INTERFACE,
                gtk_remove_parameters(self.application_id.as_str(), id),
            ),
            Destination::Portal => (
                PORTAL,
                PORTAL_PATH,
                PORTAL_INTERFACE,
                portal_remove_parameters(id),
            ),
        };
        let shared = Rc::clone(self);
        let destination_for_callback = destination.clone();
        self.connection.call(
            Some(bus_name),
            path,
            interface,
            "RemoveNotification",
            Some(&parameters),
            Some(&<()>::static_variant_type()),
            gio::DBusCallFlags::NONE,
            timeout,
            Some(&self.cancellable),
            move |result| {
                shared.on_remove_reply(&destination_for_callback, result);
            },
        );
    }

    fn on_add_reply(&self, destination: &Destination, result: Result<glib::Variant, glib::Error>) {
        {
            let mut state = self.state.borrow_mut();
            state.add_in_flight = state.add_in_flight.saturating_sub(1);
        }
        if let Err(error) = result {
            #[cfg(test)]
            {
                self.state.borrow_mut().add_errors += 1;
            }
            eprintln!("among: {destination:?} AddNotification failed: {error}");
        }
    }

    fn on_remove_reply(
        &self,
        destination: &Destination,
        result: Result<glib::Variant, glib::Error>,
    ) {
        {
            let mut state = self.state.borrow_mut();
            state.remove_in_flight = state.remove_in_flight.saturating_sub(1);
        }
        if let Err(error) = result {
            #[cfg(test)]
            {
                self.state.borrow_mut().remove_errors += 1;
            }
            eprintln!("among: {destination:?} RemoveNotification failed: {error}");
        }
    }
}

fn gtk_add_parameters(application_id: &str, id: &str, payload: glib::Variant) -> glib::Variant {
    glib::Variant::tuple_from_iter([application_id.to_variant(), id.to_variant(), payload])
}

fn portal_add_parameters(id: &str, payload: glib::Variant) -> glib::Variant {
    glib::Variant::tuple_from_iter([id.to_variant(), payload])
}

fn gtk_remove_parameters(application_id: &str, id: &str) -> glib::Variant {
    glib::Variant::tuple_from_iter([application_id.to_variant(), id.to_variant()])
}

fn portal_remove_parameters(id: &str) -> glib::Variant {
    glib::Variant::tuple_from_iter([id.to_variant()])
}

pub(super) fn serialized_payload(text: &Text, target: &Target) -> glib::Variant {
    let mut payload = BTreeMap::new();
    payload.insert("title".to_string(), text.title.as_str().to_variant());
    payload.insert("body".to_string(), text.body.as_str().to_variant());
    if let Some(icon) = gio::ThemedIcon::new("com.github.among-terminal").serialize() {
        payload.insert("icon".to_string(), icon);
    }
    payload.insert("priority".to_string(), "normal".to_variant());
    payload.insert("default-action".to_string(), "app.focus-pane".to_variant());
    payload.insert("default-action-target".to_string(), target_variant(target));
    payload.to_variant()
}

#[cfg(test)]
#[path = "named_tests.rs"]
mod tests;
