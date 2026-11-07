//! GIO owns notification backend selection and action activation. Name watchers
//! and the bus's activatable names report whether a delivery route exists; GIO's
//! send API cannot confirm that a shell displayed the notification.

use adw::prelude::*;
use among::notifications::desktop::{Availability, Sender, Target, Text};
use std::cell::RefCell;
use std::rc::Rc;

pub(super) struct GioSender {
    application: glib::WeakRef<gio::Application>,
    services: Rc<RefCell<Services>>,
    discovery: gio::Cancellable,
    unwatch: Vec<Box<dyn FnOnce()>>,
}

struct Services {
    names: Vec<&'static str>,
    owners: Vec<Option<bool>>,
    activatable: Option<Vec<String>>,
}

fn portal_route(flatpak: bool, portal_env: Option<&std::ffi::OsStr>) -> bool {
    flatpak || portal_env.is_some_and(|value| value.as_encoded_bytes().first() == Some(&b'1'))
}

impl Services {
    fn availability(&self) -> Availability {
        if self.owners.contains(&Some(true))
            || self.activatable.as_ref().is_some_and(|names| {
                self.names.iter().any(|name| {
                    // GIO's GTK backend requires an existing owner. Its FDO
                    // and selected portal backends can activate their service.
                    *name != "org.gtk.Notifications"
                        && names.iter().any(|found| found.as_str() == *name)
                })
            })
        {
            Availability::Available
        } else if self.owners.iter().any(Option::is_none) || self.activatable.is_none() {
            Availability::Checking
        } else {
            Availability::Unavailable
        }
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
        let mut names = vec!["org.gtk.Notifications", "org.freedesktop.Notifications"];
        // Follow GIO's native opt-in too, without counting an unrelated portal
        // as a notification route on an ordinary native session.
        if portal_route(
            std::path::Path::new("/.flatpak-info").exists(),
            std::env::var_os("GIO_USE_PORTALS").as_deref(),
        ) {
            names = vec!["org.freedesktop.portal.Desktop"];
        }
        let services = Rc::new(RefCell::new(Services {
            owners: vec![None; names.len()],
            names: names.clone(),
            activatable: None,
        }));
        let unwatch = names
            .into_iter()
            .enumerate()
            .map(|(index, name)| {
                let appeared = services.clone();
                let vanished = services.clone();
                let watcher = gio::bus_watch_name(
                    gio::BusType::Session,
                    name,
                    gio::BusNameWatcherFlags::NONE,
                    move |_, _, _| appeared.borrow_mut().owners[index] = Some(true),
                    move |_, _| vanished.borrow_mut().owners[index] = Some(false),
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
        Self {
            application: application
                .map(|application| application.downgrade())
                .unwrap_or_default(),
            services,
            discovery,
            unwatch,
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
        self.services.borrow().availability()
    }

    fn attempt(&mut self, id: &str, text: &Text, target: &Target) {
        let Some(application) = self.application.upgrade() else {
            return;
        };
        let notification = gio::Notification::new(&text.title);
        notification.set_body(Some(&text.body));
        notification.set_priority(gio::NotificationPriority::Normal);
        notification.set_icon(&gio::ThemedIcon::new("com.github.among-terminal"));
        notification
            .set_default_action_and_target_value("app.focus-pane", Some(&target_variant(target)));
        application.send_notification(Some(id), &notification);
    }

    fn withdraw(&mut self, id: &str) {
        if let Some(application) = self.application.upgrade() {
            application.withdraw_notification(id);
        }
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
