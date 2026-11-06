use crate::prefs::general::AudioReport;
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
    delivery: among::plugin_notifications::DeliveryAuthorization,
    core: Weak<RefCell<Host>>,
}

pub(super) struct Shared {
    pub audio: Option<Audio>,
    active: RefCell<Option<Active>>,
    pub ready: Cell<bool>,
    pub loading_owner: Cell<Option<Channel>>,
    pub status: RefCell<AudioReport>,
    latest_playback: Cell<Option<u64>>,
    busy: Cell<bool>,
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
                Ok(_) => AudioReport::Idle,
                Err(error) => AudioReport::Problem(error.to_string()),
            };
            let shared = Rc::new(Self {
                ready: Cell::new(audio.is_ok()),
                loading_owner: Cell::new(None),
                audio: audio.ok(),
                active: RefCell::new(None),
                status: RefCell::new(status),
                latest_playback: Cell::new(None),
                busy: Cell::new(false),
            });
            *slot.borrow_mut() = Rc::downgrade(&shared);
            shared
        })
    }

    pub fn ingress(
        &self,
        host: &Rc<RefCell<Host>>,
        bridge: &Rc<RefCell<among::plugin_notifications::Backend>>,
        channel: Channel,
        now: std::time::Instant,
    ) {
        let job = host.borrow_mut().take_ready(channel, now);
        host.borrow_mut().discard_pending(channel);
        let Some(job) = job else {
            return;
        };
        let Some(authorization) = host.borrow().authorization(job) else {
            return;
        };
        let Some(delivery) = bridge
            .try_borrow()
            .ok()
            .and_then(|bridge| bridge.authorize_delivery(job, now))
        else {
            return;
        };
        if !delivery.is_current(&host.borrow(), now) {
            return;
        }
        let Some(audio) = &self.audio else {
            return;
        };
        audio.set_event_admitted(
            self.ready.get()
                && authorization.is_current()
                && delivery
                    .origin_lease()
                    .as_ref()
                    .is_none_or(|lease| lease.is_current()),
        );
        if let Ok(Request::Started(id)) = audio.event() {
            self.latest_playback.set(Some(id));
            self.busy.set(false);
            *self.active.borrow_mut() = Some(Active {
                id,
                channel,
                event: job.event(),
                authorization,
                delivery,
                core: Rc::downgrade(host),
            });
        }
    }

    pub fn revalidate(&self, channel: Channel, policy: Policy, focus: Focus) {
        let mut active = self.active.borrow_mut();
        if let Some(event) = active.as_ref() {
            if !event.authorization.is_current()
                || !event.core.upgrade().is_some_and(|core| {
                    core.try_borrow().is_ok_and(|core| {
                        event.delivery.is_current(&core, std::time::Instant::now())
                    })
                })
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
            Request::Started(id) => {
                self.latest_playback.set(Some(id));
                self.busy.set(false);
                self.active.borrow_mut().take();
                Ok(())
            }
            Request::Busy => {
                self.busy.set(true);
                let text = "The previous sound is still stopping. Nothing is queued; press Play again in a moment.";
                *self.status.borrow_mut() = AudioReport::Problem(text.into());
                Err(text.into())
            }
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
        self.latest_playback.set(None);
        self.loading_owner.set(None);
        self.ready.set(true);
        Ok(())
    }

    pub fn state(&self) -> crate::prefs::general::AudioState {
        use crate::prefs::general::AudioState;
        let Some(audio) = &self.audio else {
            return AudioState::Unavailable;
        };
        let snapshot = audio.decoder_snapshot();
        if snapshot
            .as_ref()
            .is_some_and(|state| state.phase == among::audio::DecoderPhase::Quarantined)
        {
            return AudioState::Unconfirmed;
        }
        if self.loading_owner.get().is_some() {
            return AudioState::Reading;
        }
        if snapshot
            .as_ref()
            .is_some_and(|state| state.phase != among::audio::DecoderPhase::Finished)
            && !audio.is_active()
            || snapshot.is_none() && self.busy.get()
        {
            return AudioState::Retiring;
        }
        if !self.ready.get() {
            return AudioState::Unavailable;
        }
        if audio.is_active() {
            AudioState::Active
        } else {
            AudioState::Ready
        }
    }

    pub fn cancel(&self) {
        if let Some(audio) = &self.audio {
            audio.cancel();
            self.active.borrow_mut().take();
            if self.loading_owner.take().is_some() {
                self.ready.set(false);
            }
            *self.status.borrow_mut() = AudioReport::Info(
                "Stopping. The decoder is not yet confirmed retired; nothing is queued.".into(),
            );
        }
    }

    pub fn poll(&self) {
        if let Some(audio) = &self.audio {
            for notice in audio.drain_notices() {
                self.observe_notice(notice);
            }
        }
    }

    fn observe_notice(&self, notice: Notice) {
        let operation = match &notice {
            Notice::Requested { id, .. }
            | Notice::DecoderEnded { id }
            | Notice::Failed { id, .. }
            | Notice::Cancelled { id } => Some(*id),
            _ => None,
        };
        if operation.is_some() && operation != self.latest_playback.get() {
            return;
        }
        let report = match notice {
            Notice::Requested { .. } => {
                AudioReport::Info("Playback requested; output is not confirmed".into())
            }
            Notice::DecoderEnded { .. } => {
                AudioReport::Info("Decoder ended; output is not confirmed".into())
            }
            Notice::Failed { reason, .. } => AudioReport::Problem(reason.into()),
            Notice::Cancelled { .. } => AudioReport::Info(
                "Stopped. Nothing was queued; press Play again to start over.".into(),
            ),
            Notice::Selected { .. } => {
                AudioReport::Info("WAV accepted as an immutable snapshot".into())
            }
            Notice::SelectionFailed { error, .. } => AudioReport::Problem(error.to_string()),
        };
        *self.status.borrow_mut() = report;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn old_operation_notices_cannot_replace_new_preview_diagnostics() {
        let shared = Shared {
            audio: None,
            active: RefCell::new(None),
            ready: Cell::new(false),
            loading_owner: Cell::new(None),
            status: RefCell::new(AudioReport::Info(
                "New Preview requested; output unconfirmed".into(),
            )),
            latest_playback: Cell::new(Some(42)),
            busy: Cell::new(false),
        };
        for notice in [
            Notice::Cancelled { id: 17 },
            Notice::DecoderEnded { id: 17 },
            Notice::Failed {
                id: 17,
                reason: "stale failure",
            },
            Notice::Requested {
                id: 17,
                kind: among::audio::PlaybackKind::Event,
            },
        ] {
            shared.observe_notice(notice);
            assert_eq!(
                *shared.status.borrow(),
                AudioReport::Info("New Preview requested; output unconfirmed".into())
            );
        }
        shared.observe_notice(Notice::Requested {
            id: 42,
            kind: among::audio::PlaybackKind::Preview,
        });
        assert_eq!(
            *shared.status.borrow(),
            AudioReport::Info("Playback requested; output is not confirmed".into())
        );
        shared.observe_notice(Notice::Cancelled { id: 42 });
        assert!(shared.status.borrow().text().contains("press Play again"));
        // A failure is a problem, not information: the page shows only problems
        // as text.
        shared.observe_notice(Notice::Failed {
            id: 42,
            reason: "decoder failed",
        });
        assert_eq!(
            *shared.status.borrow(),
            AudioReport::Problem("decoder failed".into())
        );
    }
}
