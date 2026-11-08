//! Desktop delivery route selection and the direct named senders.
//!
//! The route is selected before the first platform call and kept for the
//! process. GTK and portal use direct D-Bus calls in `named`; freedesktop uses
//! the asynchronous sender in `fdo`. GIO's own backend selection is not used,
//! because it can choose and cache a different backend after the route freeze.

use adw::prelude::*;
use among::notifications::desktop::{Availability, Sender, Target, Text};
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

mod fdo;
mod named;

const GTK: &str = "org.gtk.Notifications";
const FDO: &str = "org.freedesktop.Notifications";
const PORTAL: &str = "org.freedesktop.portal.Desktop";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Route {
    Gtk,
    Portal,
    Fdo,
}

impl Route {
    fn name(self) -> &'static str {
        match self {
            Self::Gtk => GTK,
            Self::Portal => PORTAL,
            Self::Fdo => FDO,
        }
    }
}

pub(super) struct GioSender {
    application: glib::WeakRef<gio::Application>,
    services: Rc<RefCell<Services>>,
    discovery: gio::Cancellable,
    unwatch: Vec<Box<dyn FnOnce()>>,
    fdo: Option<fdo::FdoSender>,
    named: Option<named::NamedSender>,
}

struct Services {
    owners: BTreeMap<&'static str, Option<bool>>,
    owner_names: BTreeMap<&'static str, Option<String>>,
    activatable: Option<Vec<String>>,
    portal_supported: bool,
    forced: Option<String>,
    route: Option<Route>,
}

fn portal_route(flatpak: bool, portal_env: Option<&std::ffi::OsStr>) -> bool {
    flatpak || portal_env.is_some_and(|value| value.as_encoded_bytes().first() == Some(&b'1'))
}

impl Services {
    fn new(portal_supported: bool, forced: Option<String>) -> Self {
        let mut owners = BTreeMap::new();
        let mut owner_names = BTreeMap::new();
        for name in [GTK, FDO, PORTAL] {
            owners.insert(name, None);
            owner_names.insert(name, None);
        }
        Self {
            owners,
            owner_names,
            activatable: None,
            portal_supported,
            forced,
            route: None,
        }
    }

    fn owner(&self, name: &str) -> Option<bool> {
        self.owners.get(name).copied().flatten()
    }

    fn owner_known(&self, name: &str) -> bool {
        self.owners.get(name).is_some_and(Option::is_some)
    }

    fn owner_name(&self, name: &str) -> Option<String> {
        self.owner_names.get(name).cloned().flatten()
    }

    fn is_activatable(&self, name: &str) -> bool {
        self.activatable
            .as_ref()
            .is_some_and(|names| names.iter().any(|found| found.as_str() == name))
    }

    fn gtk_owned(&self) -> bool {
        self.owner(GTK) == Some(true)
    }

    fn selected(&self) -> Route {
        match self.forced.as_deref() {
            Some("freedesktop") => Route::Fdo,
            Some("gtk") => {
                if self.gtk_owned() {
                    Route::Gtk
                } else if self.portal_supported {
                    Route::Portal
                } else {
                    Route::Fdo
                }
            }
            Some("portal") => {
                if self.portal_supported {
                    Route::Portal
                } else if self.gtk_owned() {
                    Route::Gtk
                } else {
                    Route::Fdo
                }
            }
            _ => {
                if self.portal_supported {
                    Route::Portal
                } else if self.gtk_owned() {
                    Route::Gtk
                } else {
                    Route::Fdo
                }
            }
        }
    }

    fn depends_on_gtk(&self) -> bool {
        match self.forced.as_deref() {
            Some("freedesktop") => false,
            Some("gtk") => true,
            Some("portal") => !self.portal_supported,
            _ => !self.portal_supported,
        }
    }

    fn route_available(&self, route: Route) -> bool {
        match route {
            Route::Gtk => self.owner(GTK) == Some(true),
            // A selected portal that nothing owns and nothing can start would
            // make every eligible bell fail instead of being reported.
            Route::Portal => {
                self.portal_supported
                    && (self.owner(PORTAL) == Some(true) || self.is_activatable(PORTAL))
            }
            Route::Fdo => self.owner(FDO) == Some(true) || self.is_activatable(FDO),
        }
    }

    fn effective_route(&self) -> Route {
        self.route.unwrap_or_else(|| self.selected())
    }

    fn availability(&self) -> Availability {
        if self.activatable.is_none() {
            return Availability::Checking;
        }
        if self.route.is_none() && self.depends_on_gtk() && !self.owner_known(GTK) {
            return Availability::Checking;
        }
        let route = self.effective_route();
        if !self.owner_known(route.name()) {
            return Availability::Checking;
        }
        if self.route_available(route) {
            Availability::Available
        } else {
            Availability::Unavailable
        }
    }

    fn freeze(&mut self) -> Route {
        let selected = self.selected();
        *self.route.get_or_insert(selected)
    }
}

impl GioSender {
    pub(super) fn new(window: &adw::ApplicationWindow) -> Self {
        let application = window
            .application()
            .map(|app| app.upcast::<gio::Application>());
        Self::for_application(application)
    }

    fn for_application(application: Option<gio::Application>) -> Self {
        let portal_supported = portal_route(
            std::path::Path::new("/.flatpak-info").exists(),
            std::env::var_os("GIO_USE_PORTALS").as_deref(),
        );
        let forced = std::env::var("GNOTIFICATION_BACKEND").ok();
        let services = Rc::new(RefCell::new(Services::new(portal_supported, forced)));
        let unwatch = [GTK, FDO, PORTAL]
            .into_iter()
            .map(|name| {
                let appeared = services.clone();
                let vanished = services.clone();
                let watcher = gio::bus_watch_name(
                    gio::BusType::Session,
                    name,
                    gio::BusNameWatcherFlags::NONE,
                    move |_, _, owner: &str| {
                        let mut services = appeared.borrow_mut();
                        services.owners.insert(name, Some(true));
                        services.owner_names.insert(name, Some(owner.to_string()));
                    },
                    move |_, _| {
                        let mut services = vanished.borrow_mut();
                        services.owners.insert(name, Some(false));
                        services.owner_names.insert(name, None);
                    },
                );
                Box::new(move || gio::bus_unwatch_name(watcher)) as Box<dyn FnOnce()>
            })
            .collect();
        let discovery = gio::Cancellable::new();
        gio::bus_get(gio::BusType::Session, Some(&discovery), {
            let services = services.clone();
            let discovery = discovery.clone();
            move |result| match result {
                Ok(connection) => connection.call(
                    Some("org.freedesktop.DBus"),
                    "/org/freedesktop/DBus",
                    "org.freedesktop.DBus",
                    "ListActivatableNames",
                    None,
                    None,
                    gio::DBusCallFlags::NONE,
                    2_000,
                    Some(&discovery),
                    move |result| {
                        services.borrow_mut().activatable = Some(
                            result
                                .ok()
                                .and_then(|reply| reply.get::<(Vec<String>,)>())
                                .map(|(names,)| names)
                                .unwrap_or_default(),
                        );
                    },
                ),
                Err(_) => services.borrow_mut().activatable = Some(Vec::new()),
            }
        });
        let fdo = application.as_ref().and_then(fdo::FdoSender::new);
        let named = application.as_ref().and_then(named::NamedSender::new);
        Self {
            application: application
                .map(|application| application.downgrade())
                .unwrap_or_default(),
            services,
            discovery,
            unwatch,
            fdo,
            named,
        }
    }
}

pub(super) fn target_variant(target: &Target) -> glib::Variant {
    (
        target.instance.as_str(),
        target.session,
        target.generation,
        target.pane,
    )
        .to_variant()
}

pub(super) fn target(value: &glib::Variant) -> Option<Target> {
    let (instance, session, generation, pane) = value.get::<(String, u64, u64, u64)>()?;
    Some(Target {
        instance,
        session,
        generation,
        pane,
    })
}

impl Sender for GioSender {
    fn availability(&self) -> Availability {
        if self.application.upgrade().is_none() {
            return Availability::Unavailable;
        }
        let services = self.services.borrow();
        match services.effective_route() {
            Route::Fdo if self.fdo.is_none() => return Availability::Unavailable,
            Route::Gtk | Route::Portal if self.named.is_none() => {
                return Availability::Unavailable;
            }
            _ => {}
        }
        services.availability()
    }

    fn attempt(&mut self, id: &str, text: &Text, target: &Target) {
        let route = self.services.borrow_mut().freeze();
        match route {
            Route::Fdo => {
                if let Some(fdo) = &mut self.fdo {
                    fdo.notify(id, text, target);
                }
            }
            Route::Gtk => {
                let owner = self.services.borrow().owner_name(GTK);
                let Some(owner) = owner else {
                    return;
                };
                let Some(named) = &mut self.named else {
                    return;
                };
                named.notify(id, text, target, named::Destination::Gtk(owner));
            }
            Route::Portal => {
                if let Some(named) = &mut self.named {
                    named.notify(id, text, target, named::Destination::Portal);
                }
            }
        }
    }

    fn withdraw(&mut self, id: &str) {
        let route = self.services.borrow().effective_route();
        match route {
            Route::Fdo => {
                if let Some(fdo) = &mut self.fdo {
                    fdo.withdraw(id);
                }
            }
            Route::Gtk | Route::Portal => {
                if let Some(named) = &mut self.named {
                    named.withdraw(id);
                }
            }
        }
    }

    fn shutdown(&mut self) {
        let route = self.services.borrow().effective_route();
        match route {
            Route::Fdo => {
                if let Some(fdo) = &mut self.fdo {
                    fdo.shutdown();
                }
            }
            Route::Gtk | Route::Portal => {
                if let Some(named) = &mut self.named {
                    named.shutdown();
                }
            }
        }
    }

    fn is_quiescent(&self) -> bool {
        let route = self.services.borrow().effective_route();
        match route {
            Route::Fdo => self.fdo.as_ref().is_none_or(fdo::FdoSender::is_quiescent),
            Route::Gtk | Route::Portal => self
                .named
                .as_ref()
                .is_none_or(named::NamedSender::is_quiescent),
        }
    }
}

#[cfg(test)]
impl GioSender {
    pub(super) fn set_fdo_drain_budget_for_test(&mut self, budget: std::time::Duration) {
        if let Some(fdo) = &mut self.fdo {
            fdo.set_drain_budget_for_test(budget);
        }
    }

    pub(super) fn fdo_owner_for_test(&self) -> Option<String> {
        self.fdo.as_ref().and_then(fdo::FdoSender::owner_for_test)
    }

    pub(super) fn fdo_has_retired_for_test(&self, owner: &str, server_id: u32) -> bool {
        self.fdo
            .as_ref()
            .is_some_and(|fdo| fdo.has_retired_for_test(owner, server_id))
    }

    pub(super) fn set_named_drain_budget_for_test(&mut self, budget: std::time::Duration) {
        if let Some(named) = &mut self.named {
            named.set_drain_budget_for_test(budget);
        }
    }

    pub(super) fn gtk_owner_for_test(&self) -> Option<String> {
        self.services.borrow().owner_name(GTK)
    }

    pub(super) fn named_error_counts_for_test(&self) -> (usize, usize) {
        self.named
            .as_ref()
            .map_or((0, 0), named::NamedSender::error_counts)
    }
}

impl Drop for GioSender {
    fn drop(&mut self) {
        self.discovery.cancel();
        for unwatch in self.unwatch.drain(..) {
            unwatch();
        }
    }
}

#[cfg(test)]
#[path = "../desktop_service_tests.rs"]
mod service_tests;
