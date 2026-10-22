//! narrow GTK ingress/lifetime adapter; presentation and delivery stay separate.

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::rc::Rc;
use std::time::Instant;

use adw::prelude::*;
use among::notifications::preferences::{AudioSource, Edit, Preferences};
use among::notifications::{Aggregate, Attention, Diagnostic, Failure, Focus, Host, Source};
use vte::prelude::*;

mod command;
mod sound;

pub(super) struct Backend {
    window: glib::WeakRef<adw::ApplicationWindow>,
    terminals: RefCell<BTreeMap<u64, glib::WeakRef<vte::Terminal>>>,
    host: RefCell<Option<Host>>,
    preferences: RefCell<Preferences>,
    commands: RefCell<command::Adapter>,
    sound: Rc<sound::Shared>,
    audio_channel: among::notifications::Channel,
    providers: RefCell<among::notifications::providers::Registry>,
    closed: Cell<bool>,
}

impl Backend {
    pub(super) fn new(window: &adw::ApplicationWindow) -> Result<Rc<Self>, Failure> {
        let mut host = Host::new()?;
        let audio_channel =
            host.register_channel(among::notifications::Policy::DELIVERY_DEFAULT)?;
        let backend = Rc::new(Self {
            window: window.downgrade(),
            terminals: RefCell::new(BTreeMap::new()),
            host: RefCell::new(Some(host)),
            preferences: RefCell::new(Preferences::default()),
            commands: RefCell::new(command::Adapter::new()),
            sound: sound::Shared::application(),
            audio_channel,
            providers: RefCell::new(Default::default()),
            closed: Cell::new(false),
        });
        window.connect_is_active_notify({
            let backend = backend.clone();
            move |_| backend.observe_focus()
        });
        window.connect_focus_widget_notify({
            let backend = backend.clone();
            move |_| backend.observe_focus()
        });
        if let Some(app) = window.application() {
            let weak = Rc::downgrade(&backend.sound);
            app.connect_shutdown(move |_| {
                if let Some(sound) = weak.upgrade() {
                    if let Some(audio) = &sound.audio {
                        audio.shutdown();
                    }
                }
            });
        }
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
            self.sound.ingress(host, self.audio_channel, now);
        }
    }

    pub(super) fn observe_focus(&self) {
        let focus = self.focus();
        if let Some(host) = self.host.borrow_mut().as_mut() {
            host.acknowledge(focus);
        }
        self.sound
            .revalidate(self.audio_channel, self.preferences.borrow().audio, focus);
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
        self.closed.set(true);
        self.sound.cancel_channel(self.audio_channel);
        self.sound.cancel_selection(self.audio_channel);
        if let Some(host) = self.host.borrow_mut().as_mut() {
            host.remove_channel(self.audio_channel);
            self.commands.borrow_mut().shutdown(host);
            let sources: Vec<_> = self
                .terminals
                .borrow()
                .keys()
                .filter_map(|pane| host.source(*pane))
                .collect();
            for source in sources {
                host.detach(source);
            }
        }
        self.terminals.borrow_mut().clear();
    }

    pub(super) fn poll(&self) {
        if let Some(host) = self.host.borrow_mut().as_mut() {
            self.commands.borrow_mut().poll(host);
        }
        self.sound.poll();
    }

    pub(super) fn is_stopped(&self) -> bool {
        self.commands.borrow().is_stopped()
    }
    pub(super) fn preferences(&self) -> Preferences {
        self.preferences.borrow().clone()
    }
    pub(super) fn command_status(&self) -> String {
        let state = self.commands.borrow().status();
        if let Some(completion) = self
            .host
            .borrow()
            .as_ref()
            .and_then(|host| host.command_completions().next_back())
            .copied()
        {
            format!(
                "{state} Last local result: {:?}; cleanup confirmed: {}",
                completion.outcome, completion.reaped
            )
        } else {
            state
        }
    }
    pub(super) fn provider_available(&self, key: &among::notifications::providers::Key) -> bool {
        self.providers.borrow().available(key)
    }
    pub(super) fn audio_status(&self) -> String {
        self.sound.status.borrow().clone()
    }
    pub(super) fn preview(&self) -> Result<(), String> {
        if self.closed.get() {
            return Err("Notification session closed".into());
        }
        self.sound.preview()
    }

    pub(super) fn restore_sound(self: &Rc<Self>, source: AudioSource) {
        // an unreadable saved local file must not silently play the built-in.
        if matches!(source, AudioSource::Local(_)) {
            self.sound.ready.set(false);
        }
        self.select_sound(
            source,
            Box::new({
                let sound = self.sound.clone();
                move |result| {
                    if let Err(problem) = result {
                        *sound.status.borrow_mut() = problem;
                    }
                }
            }),
        );
    }

    pub(super) fn select_sound(
        self: &Rc<Self>,
        source: AudioSource,
        finished: Box<dyn FnOnce(Result<(), String>)>,
    ) {
        if self.closed.get() {
            finished(Err("Notification session closed".into()));
            return;
        }
        match source {
            AudioSource::Builtin => {
                let result = self.sound.builtin();
                if result.is_ok() {
                    self.preferences.borrow_mut().sound = AudioSource::Builtin;
                }
                finished(result);
            }
            AudioSource::Local(path) => {
                let Some(audio) = &self.sound.audio else {
                    finished(Err("Audio unavailable".into()));
                    return;
                };
                let weak = Rc::downgrade(self);
                let completed = Rc::new(RefCell::new(Some(finished)));
                let callback = completed.clone();
                let accepted_path = path.clone();
                let previous_ready = self.sound.ready.replace(false);
                let result = audio.select_local(path.into(), move |result| {
                    let Some(backend) = weak.upgrade() else {
                        return;
                    };
                    if backend.closed.get() {
                        return;
                    }
                    backend.sound.loading_owner.set(None);
                    backend.sound.ready.set(result.is_ok() || previous_ready);
                    if result.is_ok() {
                        backend.preferences.borrow_mut().sound = AudioSource::Local(accepted_path);
                    }
                    if let Some(finished) = callback.borrow_mut().take() {
                        finished(result.map_err(|error| error.to_string()));
                    }
                });
                if let Err(error) = result {
                    self.sound.ready.set(previous_ready);
                    if let Some(finished) = completed.borrow_mut().take() {
                        finished(Err(error.to_string()));
                    }
                } else {
                    self.sound.loading_owner.set(Some(self.audio_channel));
                }
            }
        }
    }

    pub(super) fn configure(&self, preferences: Preferences) {
        if let Some(host) = self.host.borrow_mut().as_mut() {
            host.set_visual_policy(preferences.visual);
            if self
                .providers
                .borrow_mut()
                .configure(host, preferences.providers.clone())
                .is_err()
            {
                eprintln!("among: provider policy admission unavailable");
            }
            if self.preferences.borrow().audio != preferences.audio {
                let _ = host.set_channel_policy(self.audio_channel, preferences.audio);
                self.sound.cancel_channel(self.audio_channel);
            }
            self.commands
                .borrow_mut()
                .configure(host, &self.preferences.borrow(), &preferences);
            *self.preferences.borrow_mut() = preferences;
        }
    }

    pub(super) fn edit(&self, edit: &Edit) -> Result<(), &'static str> {
        if self.closed.get() {
            return Err("Notification session closed");
        }
        let mut preferences = self.preferences();
        preferences.edit(edit)?;
        self.configure(preferences);
        Ok(())
    }

    pub(super) fn event_count(&self) -> u64 {
        self.host.borrow().as_ref().map_or(0, Host::event_count)
    }

    #[cfg(test)]
    pub(super) fn with_host<T>(&self, operation: impl FnOnce(&mut Host) -> T) -> Option<T> {
        self.host.borrow_mut().as_mut().map(operation)
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
