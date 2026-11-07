//! GIO owns notification backend selection and action activation. Name watchers
//! only report whether the session currently has a notification service; GIO's
//! send API cannot confirm that a shell displayed the notification.

use adw::prelude::*;
use among::notifications::desktop::{Availability, Sender, Target, Text};
use std::cell::RefCell;
use std::rc::Rc;

pub(super) struct GioSender {
    application: glib::WeakRef<gio::Application>,
    services: Rc<RefCell<Vec<Option<bool>>>>,
    unwatch: Vec<Box<dyn FnOnce()>>,
}

impl GioSender {
    pub(super) fn new(window: &adw::ApplicationWindow) -> Self {
        let application = window
            .application()
            .map(|app| app.upcast::<gio::Application>());
        let mut names = vec!["org.gtk.Notifications", "org.freedesktop.Notifications"];
        // The portal is the delivery route inside Flatpak. Its presence on a
        // native desktop alone does not make it a native notification daemon.
        if std::path::Path::new("/.flatpak-info").exists() {
            names = vec!["org.freedesktop.portal.Desktop"];
        }
        let services = Rc::new(RefCell::new(vec![None; names.len()]));
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
                    move |_, _, _| appeared.borrow_mut()[index] = Some(true),
                    move |_, _| vanished.borrow_mut()[index] = Some(false),
                );
                Box::new(move || gio::bus_unwatch_name(watcher)) as Box<dyn FnOnce()>
            })
            .collect();
        Self {
            application: application
                .map(|application| application.downgrade())
                .unwrap_or_default(),
            services,
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
        let services = self.services.borrow();
        if services.contains(&Some(true)) {
            Availability::Available
        } else if services.iter().any(Option::is_none) {
            Availability::Checking
        } else {
            Availability::Unavailable
        }
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
        for unwatch in self.unwatch.drain(..) {
            unwatch();
        }
    }
}
