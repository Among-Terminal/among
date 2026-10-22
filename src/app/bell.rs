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
    host: Rc<RefCell<Host>>,
    bridge: Rc<RefCell<among::plugin_notifications::Backend>>,
    workspaces: RefCell<BTreeMap<u64, u64>>,
    preferences: RefCell<Preferences>,
    commands: RefCell<command::Adapter>,
    sound: Rc<sound::Shared>,
    audio_channel: among::notifications::Channel,
    closed: Cell<bool>,
}

impl Backend {
    pub(super) fn new(window: &adw::ApplicationWindow) -> Result<Rc<Self>, Failure> {
        let mut host = Host::new()?;
        let audio_channel =
            host.register_channel(among::notifications::Policy::DELIVERY_DEFAULT)?;
        let host = Rc::new(RefCell::new(host));
        let bridge = Rc::new(RefCell::new(among::plugin_notifications::Backend::new(
            host.clone(),
            Focus::default(),
        )));
        let backend = Rc::new(Self {
            window: window.downgrade(),
            terminals: RefCell::new(BTreeMap::new()),
            host,
            bridge,
            workspaces: RefCell::new(BTreeMap::new()),
            preferences: RefCell::new(Preferences::default()),
            commands: RefCell::new(command::Adapter::new()),
            sound: sound::Shared::application(),
            audio_channel,
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
        let source = match self.host.borrow_mut().attach(pane) {
            Ok(source) => source,
            Err(_) => {
                eprintln!("among: bell source identity exhausted; source not attached");
                return;
            }
        };
        self.terminals
            .borrow_mut()
            .insert(pane, terminal.downgrade());
        // no VTE beep bypasses the disabled-by-default host audio channel.
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
        if self.closed.get() {
            return;
        }
        let event = self.host.borrow_mut().bell(source, focus, now);
        if let Ok(event) = event {
            let workspace = self.workspaces.borrow().get(&source.pane()).copied();
            let mut bridge = self.bridge.borrow_mut();
            bridge.set_focus(focus);
            if let Err(reason) = bridge.observe_host_event(event, workspace, focus) {
                bridge.reject(reason);
            }
            drop(bridge);
            self.sound
                .ingress(&self.host, &self.bridge, self.audio_channel, now);
        }
    }

    pub(super) fn observe_focus(&self) {
        let focus = self.focus();
        self.host.borrow_mut().acknowledge(focus);
        self.bridge.borrow_mut().set_focus(focus);
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
                    .then(|| self.host.borrow().source(*pane))
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
            {
                let mut host = self.host.borrow_mut();
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
        self.bridge.borrow_mut().shutdown();
        {
            let mut host = self.host.borrow_mut();
            host.remove_channel(self.audio_channel);
            self.commands.borrow_mut().shutdown(&mut host);
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
        self.sync_provider_policies();
        self.commands.borrow_mut().poll(&self.host, &self.bridge);
        if !self.closed.get() {
            self.sound
                .ingress(&self.host, &self.bridge, self.audio_channel, Instant::now());
        }
        self.sound.poll();
    }

    pub(super) fn is_stopped(&self) -> bool {
        self.commands.borrow().is_stopped()
    }
    pub(super) fn preferences(&self) -> Preferences {
        let mut preferences = self.preferences.borrow().clone();
        for channel in self.provider_inventory() {
            let key = channel.declaration.key;
            if let Ok(key) = among::notifications::providers::Key::new(&key.identity, &key.channel)
            {
                preferences.providers.entry(key).or_insert(channel.policy);
            }
        }
        preferences
    }
    pub(super) fn command_status(&self) -> String {
        let state = self.commands.borrow().status();
        if let Some(completion) = self
            .host
            .borrow()
            .command_completions()
            .next_back()
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
    pub(super) fn notification_backend(&self) -> Rc<RefCell<among::plugin_notifications::Backend>> {
        self.bridge.clone()
    }
    pub(super) fn provider_inventory(&self) -> Vec<among::plugin_notifications::ChannelView> {
        self.bridge.borrow().inventory()
    }
    pub(super) fn set_workspaces(&self, workspaces: BTreeMap<u64, u64>) {
        *self.workspaces.borrow_mut() = workspaces;
    }
    pub(super) fn sync_provider_policies(&self) {
        let inventory = self.provider_inventory();
        let preferences = self.preferences.borrow();
        let mut bridge = self.bridge.borrow_mut();
        for channel in inventory {
            let key = &channel.declaration.key;
            let policy = among::notifications::providers::Key::new(&key.identity, &key.channel)
                .ok()
                .and_then(|key| preferences.providers.get(&key).copied())
                .unwrap_or(among::notifications::Policy::DELIVERY_DEFAULT);
            if channel.policy != policy {
                if let Err(reason) = bridge.set_policy(key, policy) {
                    bridge.reject(reason);
                }
            }
        }
    }
    pub(super) fn audio_status(&self) -> String {
        if let Some(snapshot) = self
            .sound
            .audio
            .as_ref()
            .and_then(|audio| audio.decoder_snapshot())
        {
            match snapshot.phase {
                among::audio::DecoderPhase::Retiring => return "Decoder retirement is pending. Rejected playback is not queued; Preview requires an explicit new request after confirmation.".into(),
                among::audio::DecoderPhase::Quarantined => return "Decoder cleanup is unconfirmed; capacity is quarantined. No new playback is admitted.".into(),
                _ => {}
            }
        }
        self.sound.status.borrow().clone()
    }
    pub(super) fn preview(&self) -> Result<(), String> {
        if self.closed.get() {
            return Err("Notification session closed".into());
        }
        self.sound.preview()
    }
    pub(super) fn cancel_audio(&self) {
        if !self.closed.get() {
            self.sound.cancel();
        }
    }
    pub(super) fn audio_state(&self) -> crate::prefs::general::AudioState {
        self.sound.state()
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
        {
            let mut host = self.host.borrow_mut();
            host.set_visual_policy(preferences.visual);
            if self.preferences.borrow().audio != preferences.audio {
                let _ = host.set_channel_policy(self.audio_channel, preferences.audio);
                self.sound.cancel_channel(self.audio_channel);
            }
            self.commands.borrow_mut().configure(
                &mut host,
                &self.preferences.borrow(),
                &preferences,
            );
            *self.preferences.borrow_mut() = preferences;
        }
        self.sync_provider_policies();
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
        self.host.borrow().event_count()
    }

    #[cfg(test)]
    pub(super) fn with_host<T>(&self, operation: impl FnOnce(&mut Host) -> T) -> Option<T> {
        Some(operation(&mut self.host.borrow_mut()))
    }

    pub(super) fn diagnostics(&self) -> Vec<Diagnostic> {
        self.host.borrow().diagnostics().iter().copied().collect()
    }

    pub(super) fn attention(&self, pane: u64) -> Attention {
        let host = self.host.borrow();
        host.source(pane)
            .map(|source| host.attention(source, Instant::now()))
            .unwrap_or_default()
    }

    pub(super) fn aggregate(&self, members: impl IntoIterator<Item = u64>) -> Aggregate {
        self.host.borrow().aggregate(members, Instant::now())
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
