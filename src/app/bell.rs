//! narrow GTK ingress/lifetime adapter; presentation and delivery stay separate.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;
use std::time::Instant;

use adw::prelude::*;
use among::notifications::{Aggregate, Attention, Diagnostic, Failure, Focus, Host, Source};
use vte::prelude::*;

pub(super) struct Backend {
    window: glib::WeakRef<adw::ApplicationWindow>,
    terminals: RefCell<BTreeMap<u64, glib::WeakRef<vte::Terminal>>>,
    host: RefCell<Option<Host>>,
}

impl Backend {
    pub(super) fn new(window: &adw::ApplicationWindow) -> Result<Rc<Self>, Failure> {
        let backend = Rc::new(Self {
            window: window.downgrade(),
            terminals: RefCell::new(BTreeMap::new()),
            host: RefCell::new(Some(Host::new()?)),
        });
        window.connect_is_active_notify({
            let backend = backend.clone();
            move |_| backend.observe_focus()
        });
        window.connect_focus_widget_notify({
            let backend = backend.clone();
            move |_| backend.observe_focus()
        });
        Ok(backend)
    }

    pub(super) fn attach(self: &Rc<Self>, pane: u64, terminal: &vte::Terminal) {
        let source = match self
            .host
            .borrow_mut()
            .as_mut()
            .map(|host| host.attach(pane))
        {
            Some(Ok(source)) => source,
            Some(Err(_)) => {
                eprintln!("among: bell source identity exhausted; source not attached");
                return;
            }
            None => return,
        };
        self.terminals
            .borrow_mut()
            .insert(pane, terminal.downgrade());
        // no VTE beep bypasses the disabled-by-default host audio channel.
        // this adapter does not claim that a host audio player exists yet.
        terminal.set_audible_bell(false);
        terminal.connect_bell({
            let backend = self.clone();
            move |_| backend.ingress(source)
        });
        terminal.connect_has_focus_notify({
            let backend = self.clone();
            move |_| backend.observe_focus()
        });
        terminal.connect_map({
            let backend = self.clone();
            move |_| backend.observe_focus()
        });
    }

    fn ingress(&self, source: Source) {
        // stamp at the signal, not on a later idle callback. the independent
        // state avoids borrowing AppState during VTE/reparenting callbacks.
        let now = Instant::now();
        let focus = self.focus();
        if let Some(host) = self.host.borrow_mut().as_mut() {
            let _ = host.bell(source, focus, now);
        }
    }

    pub(super) fn observe_focus(&self) {
        let focus = self.focus();
        if let Some(host) = self.host.borrow_mut().as_mut() {
            host.acknowledge(focus);
        }
    }

    pub(super) fn focus(&self) -> Focus {
        let Some(window) = self.window.upgrade() else {
            return Focus::default();
        };
        let main: &gtk::Window = window.upcast_ref();
        let application_active = gtk::Window::list_toplevels()
            .iter()
            .filter_map(|widget| widget.downcast_ref::<gtk::Window>())
            .any(|candidate| candidate.is_active() && belongs_to(candidate, main));
        let viewed =
            if window.is_active() && window.is_mapped() && !has_popover(window.upcast_ref()) {
                let actual = gtk::prelude::RootExt::focus(&window);
                self.terminals.borrow().iter().find_map(|(pane, weak)| {
                    let terminal = weak.upgrade()?;
                    (terminal.is_mapped()
                        && terminal.has_focus()
                        && actual.as_ref() == Some(terminal.upcast_ref()))
                    .then(|| self.host.borrow().as_ref()?.source(*pane))
                    .flatten()
                })
            } else {
                None
            };
        Focus {
            application_active,
            viewed,
        }
    }

    pub(super) fn retain(&self, live: &[crate::tree::PaneId]) {
        let removed: Vec<_> = self
            .terminals
            .borrow()
            .keys()
            .copied()
            .filter(|pane| !live.contains(&crate::tree::PaneId(*pane)))
            .collect();
        for pane in removed {
            self.terminals.borrow_mut().remove(&pane);
            if let Some(host) = self.host.borrow_mut().as_mut() {
                if let Some(source) = host.source(pane) {
                    host.detach(source);
                }
            }
        }
    }

    pub(super) fn shutdown(&self) {
        self.host.borrow_mut().take();
        self.terminals.borrow_mut().clear();
    }

    pub(super) fn event_count(&self) -> u64 {
        self.host.borrow().as_ref().map_or(0, Host::event_count)
    }

    pub(super) fn diagnostics(&self) -> Vec<Diagnostic> {
        self.host
            .borrow()
            .as_ref()
            .map(|host| host.diagnostics().iter().copied().collect())
            .unwrap_or_default()
    }

    pub(super) fn attention(&self, pane: u64) -> Attention {
        self.host
            .borrow()
            .as_ref()
            .and_then(|host| Some(host.attention(host.source(pane)?, Instant::now())))
            .unwrap_or_default()
    }

    pub(super) fn aggregate(&self, members: impl IntoIterator<Item = u64>) -> Aggregate {
        self.host
            .borrow()
            .as_ref()
            .map(|host| host.aggregate(members, Instant::now()))
            .unwrap_or_default()
    }
}

fn belongs_to(candidate: &gtk::Window, main: &gtk::Window) -> bool {
    if main
        .application()
        .is_some_and(|application| candidate.application().as_ref() == Some(&application))
    {
        return true;
    }
    // Preferences is currently a plain transient window, absent from
    // GApplication.windows(). count its entire transient chain as Among.
    let mut current = Some(candidate.clone());
    let mut visited = Vec::new();
    while let Some(window) = current {
        if window == *main {
            return true;
        }
        if visited.contains(&window) {
            break;
        }
        current = window.transient_for();
        visited.push(window);
    }
    false
}

fn has_popover(widget: &gtk::Widget) -> bool {
    if widget.is::<gtk::Popover>() && widget.is_mapped() {
        return true;
    }
    let mut child = widget.first_child();
    while let Some(widget) = child {
        if has_popover(&widget) {
            return true;
        }
        child = widget.next_sibling();
    }
    false
}
