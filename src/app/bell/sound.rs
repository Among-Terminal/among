use among::{
    audio::{Audio, Notice, Request, Sound},
    notifications::{Authorization, Channel, Event, Focus, Host, Policy},
};
use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};

struct Active {
    id: u64,
    channel: Channel,
    event: Event,
    authorization: Authorization,
}

pub(super) struct Shared {
    pub audio: Option<Audio>,
    active: RefCell<Option<Active>>,
    pub ready: Cell<bool>,
    pub loading_owner: Cell<Option<Channel>>,
    pub status: RefCell<String>,
}

thread_local! {
    static SHARED: RefCell<Weak<Shared>> = const { RefCell::new(Weak::new()) };
}

impl Shared {
    pub fn application() -> Rc<Self> {
        SHARED.with(|slot| {
            if let Some(shared) = slot.borrow().upgrade() {
                return shared;
            }
            let audio = Audio::new();
            let status = match &audio {
                Ok(_) => "Built-in WAV selected; audio events are off by default".into(),
                Err(error) => error.to_string(),
            };
            let shared = Rc::new(Self {
                ready: Cell::new(audio.is_ok()),
                loading_owner: Cell::new(None),
                audio: audio.ok(),
                active: RefCell::new(None),
                status: RefCell::new(status),
            });
            *slot.borrow_mut() = Rc::downgrade(&shared);
            shared
        })
    }

    pub fn ingress(&self, host: &mut Host, channel: Channel, now: std::time::Instant) {
        let job = host.take_ready(channel, now);
        host.discard_pending(channel);
        let Some(job) = job else {
            return;
        };
        let Some(authorization) = host.authorization(job) else {
            return;
        };
        let Some(audio) = &self.audio else {
            return;
        };
        audio.set_event_admitted(self.ready.get() && authorization.is_current());
        if let Ok(Request::Started(id)) = audio.event() {
            *self.active.borrow_mut() = Some(Active {
                id,
                channel,
                event: job.event(),
                authorization,
            });
        }
    }

    pub fn revalidate(&self, channel: Channel, policy: Policy, focus: Focus) {
        let mut active = self.active.borrow_mut();
        if let Some(event) = active.as_ref() {
            if !event.authorization.is_current()
                || (event.channel == channel && !policy.permits(event.event.source(), focus))
            {
                if let Some(audio) = &self.audio {
                    audio.cancel_event(event.id);
                }
                active.take();
            }
        }
    }

    pub fn cancel_channel(&self, channel: Channel) {
        let mut active = self.active.borrow_mut();
        if let Some(event) = active.as_ref().filter(|event| event.channel == channel) {
            if let Some(audio) = &self.audio {
                audio.cancel_event(event.id);
            }
            active.take();
        }
    }

    pub fn cancel_selection(&self, channel: Channel) {
        if self.loading_owner.get() == Some(channel) {
            if let Some(audio) = &self.audio {
                audio.cancel();
            }
            self.loading_owner.set(None);
            self.ready.set(false);
        }
    }

    pub fn preview(&self) -> Result<(), String> {
        if !self.ready.get() {
            return Err("No accepted audio source is ready".into());
        }
        let audio = self.audio.as_ref().ok_or("Audio unavailable")?;
        let result = audio.preview().map_err(|error| error.to_string())?;
        match result {
            Request::Started(_) => {
                self.active.borrow_mut().take();
                Ok(())
            }
            Request::Busy => Err("A sound selection is still being read".into()),
            Request::Suppressed => Err("Preview unavailable".into()),
        }
    }

    pub fn builtin(&self) -> Result<(), String> {
        let sound = Sound::builtin().map_err(|error| error.to_string())?;
        self.audio
            .as_ref()
            .ok_or("Audio unavailable")?
            .select(sound)
            .map_err(|error| error.to_string())?;
        self.active.borrow_mut().take();
        self.loading_owner.set(None);
        self.ready.set(true);
        Ok(())
    }

    pub fn poll(&self) {
        if let Some(audio) = &self.audio {
            for notice in audio.drain_notices() {
                let text = match notice {
                    Notice::Requested { .. } => {
                        "Playback requested; output is not confirmed".into()
                    }
                    Notice::DecoderEnded { .. } => "Decoder ended; output is not confirmed".into(),
                    Notice::Failed { reason, .. } => reason.into(),
                    Notice::Cancelled { .. } => {
                        "Playback cancelled; external output cannot be undone".into()
                    }
                    Notice::Selected { .. } => "WAV accepted as an immutable snapshot".into(),
                    Notice::SelectionFailed { error, .. } => error.to_string(),
                };
                *self.status.borrow_mut() = text;
            }
        }
    }
}
