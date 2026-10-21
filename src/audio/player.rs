use super::{Sound, WavError, WavInfo};
use gtk::prelude::*;
use std::{
    cell::RefCell,
    collections::VecDeque,
    fmt,
    path::PathBuf,
    rc::Rc,
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
    time::{Duration, Instant},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlaybackKind {
    Preview,
    Event,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Request {
    Started(u64),
    Suppressed,
    Busy,
}

/// Decoder completion is not a claim that the desktop produced sound. Output
/// acceptance belongs to an independently captured sink, not a GTK property.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Notice {
    Requested { id: u64, kind: PlaybackKind },
    DecoderEnded { id: u64 },
    Failed { id: u64, reason: &'static str },
    Cancelled { id: u64 },
    Selected { generation: u64 },
    SelectionFailed { generation: u64, error: WavError },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioError {
    Wav(WavError),
    Closed,
    Busy,
    NotInitialized,
    AlreadyOwned,
}
impl fmt::Display for AudioError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Wav(error) => error.fmt(f),
            Self::Closed => f.write_str("Audio has been shut down."),
            Self::Busy => f.write_str("A sound selection is already being read."),
            Self::NotInitialized => f.write_str("Initialize GTK on the main thread before audio."),
            Self::AlreadyOwned => f.write_str("The application already has an audio owner."),
        }
    }
}
impl std::error::Error for AudioError {}

struct Active {
    id: u64,
    kind: PlaybackKind,
    media: gtk::MediaFile,
    deadline: Instant,
    _sound: Sound,
}
impl Drop for Active {
    fn drop(&mut self) {
        self.media.pause();
        self.media.clear();
    }
}

struct State {
    selected: Sound,
    active: Option<Active>,
    generation: u64,
    admitted: bool,
    loading: bool,
    closed: bool,
    notices: VecDeque<Notice>,
}
impl State {
    fn notice(&mut self, notice: Notice) {
        if self.notices.len() == 32 {
            self.notices.pop_front();
        }
        self.notices.push_back(notice);
    }
    fn stop(&mut self) {
        if let Some(active) = self.active.take() {
            self.notice(Notice::Cancelled { id: active.id });
            drop(active);
        }
    }
    fn poll(&mut self) {
        let Some(active) = &self.active else {
            return;
        };
        // Error wins over prepared/ended: both can be true for failed backends.
        let outcome = if active.media.error().is_some() {
            Some(Notice::Failed {
                id: active.id,
                reason: "The WAV decoder or audio output is unavailable.",
            })
        } else if active.media.is_ended() {
            Some(Notice::DecoderEnded { id: active.id })
        } else if Instant::now() >= active.deadline {
            Some(Notice::Failed {
                id: active.id,
                reason: "Audio did not finish within its playback deadline.",
            })
        } else {
            None
        };
        if let Some(notice) = outcome {
            self.active.take();
            self.notice(notice);
        }
    }
}

/// One main-thread owner, one active MediaFile, zero queued playbacks and at
/// most one outstanding bounded file read. Preview replaces an active sound;
/// busy event requests coalesce instead of retaining stale notification work.
pub struct Audio {
    state: Rc<RefCell<State>>,
    timer: Option<glib::SourceId>,
}

thread_local! {
    static OWNER: RefCell<std::rc::Weak<RefCell<State>>> = const { RefCell::new(std::rc::Weak::new()) };
}

static FILE_READ_BUSY: AtomicBool = AtomicBool::new(false);
// A cancelled ID from a destroyed owner must not identify the next owner's
// first sound. IDs are transient and never serialized into preferences/sessions.
static NEXT_OPERATION: AtomicU64 = AtomicU64::new(1);
struct FileRead;
impl Drop for FileRead {
    fn drop(&mut self) {
        FILE_READ_BUSY.store(false, Ordering::Release);
    }
}

impl Audio {
    pub fn new() -> Result<Self, AudioError> {
        if !gtk::is_initialized_main_thread() {
            return Err(AudioError::NotInitialized);
        }
        if OWNER.with(|owner| owner.borrow().upgrade().is_some()) {
            return Err(AudioError::AlreadyOwned);
        }
        let state = Rc::new(RefCell::new(State {
            selected: Sound::builtin().map_err(AudioError::Wav)?,
            active: None,
            generation: 0,
            admitted: false,
            loading: false,
            closed: false,
            notices: VecDeque::new(),
        }));
        let weak = Rc::downgrade(&state);
        OWNER.with(|owner| *owner.borrow_mut() = weak.clone());
        let timer = glib::timeout_add_local(Duration::from_millis(20), move || {
            let Some(state) = weak.upgrade() else {
                return glib::ControlFlow::Break;
            };
            state.borrow_mut().poll();
            glib::ControlFlow::Continue
        });
        Ok(Self {
            state,
            timer: Some(timer),
        })
    }

    /// The host supplies *current* effective enablement + eligibility. Updating
    /// admission never plays/replays anything and never cancels explicit preview.
    pub fn set_event_admitted(&self, admitted: bool) {
        let mut state = self.state.borrow_mut();
        state.admitted = admitted && !state.closed;
        if !state.admitted
            && state
                .active
                .as_ref()
                .is_some_and(|a| a.kind == PlaybackKind::Event)
        {
            state.stop();
        }
    }

    pub fn selected_info(&self) -> WavInfo {
        self.state.borrow().selected.info()
    }

    pub fn select(&self, sound: Sound) -> Result<(), AudioError> {
        let mut state = self.state.borrow_mut();
        if state.closed {
            return Err(AudioError::Closed);
        }
        state.generation = state.generation.wrapping_add(1);
        state.loading = false;
        state.stop();
        state.selected = sound;
        let generation = state.generation;
        state.notice(Notice::Selected { generation });
        Ok(())
    }

    /// Reads/validates off the main loop. Stale completion cannot replace a
    /// newer selection or call an obsolete UI callback after cancel/shutdown.
    /// Rejected input leaves the last accepted source selected, but never plays it.
    pub fn select_local(
        &self,
        path: PathBuf,
        finished: impl FnOnce(Result<(), WavError>) + 'static,
    ) -> Result<(), AudioError> {
        let generation = {
            let mut state = self.state.borrow_mut();
            if state.closed {
                return Err(AudioError::Closed);
            }
            if state.loading
                || FILE_READ_BUSY
                    .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
                    .is_err()
            {
                return Err(AudioError::Busy);
            }
            state.loading = true;
            state.generation = state.generation.wrapping_add(1);
            state.stop();
            state.generation
        };
        // Retain the slot through the worker itself, even if its Audio owner is
        // dropped and a new one is created while a slow regular-file read ends.
        let read = FileRead;
        let weak = Rc::downgrade(&self.state);
        glib::spawn_future_local(async move {
            let current = weak.upgrade().is_some_and(|state| {
                let state = state.borrow();
                !state.closed && state.generation == generation
            });
            if !current {
                return;
            }
            let result = match gio::spawn_blocking(move || {
                let _read = read;
                Sound::read_local(&path)
            })
            .await
            {
                Ok(result) => result,
                Err(_) => Err(WavError::Unavailable),
            };
            let Some(state) = weak.upgrade() else {
                return;
            };
            let accepted = {
                let mut state = state.borrow_mut();
                if state.closed || state.generation != generation {
                    return;
                }
                state.loading = false;
                match result {
                    Ok(sound) => {
                        state.selected = sound;
                        state.notice(Notice::Selected { generation });
                        Ok(())
                    }
                    Err(error) => {
                        state.notice(Notice::SelectionFailed { generation, error });
                        Err(error)
                    }
                }
            };
            finished(accepted);
        });
        Ok(())
    }

    pub fn preview(&self) -> Result<Request, AudioError> {
        self.start(PlaybackKind::Preview)
    }

    pub fn event(&self) -> Result<Request, AudioError> {
        self.start(PlaybackKind::Event)
    }

    fn start(&self, kind: PlaybackKind) -> Result<Request, AudioError> {
        let mut state = self.state.borrow_mut();
        if state.closed {
            return Err(AudioError::Closed);
        }
        if kind == PlaybackKind::Event && !state.admitted {
            return Ok(Request::Suppressed);
        }
        if state.loading || (kind == PlaybackKind::Event && state.active.is_some()) {
            return Ok(Request::Busy);
        }
        state.stop();
        let id = NEXT_OPERATION.fetch_add(1, Ordering::Relaxed);
        let sound = state.selected.clone();
        let media = gtk::MediaFile::for_filename(sound.decoder_path());
        media.set_loop(false);
        media.set_volume(1.0);
        media.play();
        state.active = Some(Active {
            id,
            kind,
            media,
            deadline: Instant::now() + Duration::from_secs(13),
            _sound: sound,
        });
        state.notice(Notice::Requested { id, kind });
        Ok(Request::Started(id))
    }

    pub fn cancel(&self) {
        let mut state = self.state.borrow_mut();
        state.generation = state.generation.wrapping_add(1);
        state.loading = false;
        state.stop();
    }

    /// A removed/stale terminal can cancel only its own event operation, never
    /// a newer terminal's sound or a targetless explicit preview.
    pub fn cancel_event(&self, id: u64) {
        let mut state = self.state.borrow_mut();
        if state
            .active
            .as_ref()
            .is_some_and(|active| active.kind == PlaybackKind::Event && active.id == id)
        {
            state.stop();
        }
    }

    pub fn drain_notices(&self) -> Vec<Notice> {
        self.state.borrow_mut().notices.drain(..).collect()
    }

    pub fn is_active(&self) -> bool {
        self.state.borrow().active.is_some()
    }

    pub fn shutdown(&self) {
        self.cancel();
        let mut state = self.state.borrow_mut();
        state.admitted = false;
        state.closed = true;
    }
}

impl Drop for Audio {
    fn drop(&mut self) {
        self.shutdown();
        if let Some(timer) = self.timer.take() {
            timer.remove();
        }
    }
}
