use super::{
    execution::{self, Operation, Outcome},
    Sound, WavError, WavInfo,
};
use std::{
    cell::RefCell,
    collections::VecDeque,
    fmt,
    path::PathBuf,
    rc::Rc,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc,
    },
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
    Unavailable,
}
impl fmt::Display for AudioError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Wav(error) => error.fmt(f),
            Self::Closed => f.write_str("Audio has been shut down."),
            Self::Busy => f.write_str("A sound selection is already being read."),
            Self::NotInitialized => f.write_str("Initialize GTK on the main thread before audio."),
            Self::AlreadyOwned => f.write_str("The application already has an audio owner."),
            Self::Unavailable => f.write_str("The audio execution boundary is unavailable."),
        }
    }
}
impl std::error::Error for AudioError {}

struct Active {
    id: u64,
    kind: PlaybackKind,
    operation: Arc<Operation>,
    deadline: Instant,
}

struct State {
    selected: Sound,
    active: Option<Active>,
    retiring: Option<Active>,
    retirement_reported: bool,
    generation: u64,
    admitted: bool,
    loading: bool,
    closed: bool,
    notices: VecDeque<Notice>,
    last_operation: Option<Arc<Operation>>,
    reported_uncertain: bool,
}
impl State {
    fn notice(&mut self, notice: Notice) {
        if self.notices.len() == 32 {
            self.notices.pop_front();
        }
        self.notices.push_back(notice);
    }
    fn stop(&mut self) {
        self.poll_retiring();
        if let Some(active) = self.active.take() {
            active.operation.cancel();
            self.retiring = Some(active);
            self.retirement_reported = false;
        }
    }
    fn poll_retiring(&mut self) {
        let Some(retiring) = &self.retiring else {
            return;
        };
        let phase = retiring.operation.snapshot().map(|snapshot| snapshot.phase);
        let outcome = retiring.operation.outcome();
        if !self.retirement_reported {
            if let Some(outcome) = outcome {
                let id = retiring.id;
                let notice = match outcome {
                    Outcome::Cancelled => Notice::Cancelled { id },
                    Outcome::Ended => Notice::DecoderEnded { id },
                    Outcome::Failed(reason) => Notice::Failed { id, reason },
                };
                self.retirement_reported = true;
                if phase == Some(super::DecoderPhase::Quarantined) {
                    self.reported_uncertain = true;
                }
                self.notice(notice);
            }
        }
        if phase == Some(super::DecoderPhase::Finished) {
            self.retiring.take();
        }
    }
    fn poll(&mut self) {
        self.poll_retiring();
        let Some(active) = &self.active else {
            if !self.reported_uncertain {
                if let Some(snapshot) = self
                    .last_operation
                    .as_ref()
                    .and_then(|operation| operation.snapshot())
                    .filter(|snapshot| snapshot.phase == super::DecoderPhase::Quarantined)
                {
                    self.reported_uncertain = true;
                    self.notice(Notice::Failed {
                        id: snapshot.id,
                        reason: "Audio cleanup is unconfirmed; the decoder slot is quarantined.",
                    });
                }
            }
            return;
        };
        let outcome = match active.operation.outcome() {
            Some(Outcome::Ended) => Some(Notice::DecoderEnded { id: active.id }),
            Some(Outcome::Failed(reason)) => Some(Notice::Failed {
                id: active.id,
                reason,
            }),
            Some(Outcome::Cancelled) => Some(Notice::Cancelled { id: active.id }),
            None if Instant::now() >= active.deadline => {
                active.operation.cancel();
                Some(Notice::Failed {
                    id: active.id,
                    reason:
                        "Audio exceeded its 13-second operation deadline; retirement is pending.",
                })
            }
            None => None,
        };
        if let Some(notice) = outcome {
            if active
                .operation
                .snapshot()
                .is_some_and(|snapshot| snapshot.phase == super::DecoderPhase::Quarantined)
            {
                self.reported_uncertain = true;
            }
            self.active.take();
            self.notice(notice);
        }
    }
}

/// One main-thread owner, one supervised decoder slot, zero queued playbacks and at
/// most one outstanding bounded file read. Replacement cancels the old operation;
/// unconfirmed retirement reports Busy rather than retaining a replayable sound.
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
            retiring: None,
            retirement_reported: false,
            generation: 0,
            admitted: false,
            loading: false,
            closed: false,
            notices: VecDeque::new(),
            last_operation: None,
            reported_uncertain: false,
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
        let operation = match execution::start(id, state.selected.clone()) {
            Ok(Some(operation)) => operation,
            Ok(None) => return Ok(Request::Busy),
            Err(reason) => {
                state.notice(Notice::Failed { id, reason });
                return Err(AudioError::Unavailable);
            }
        };
        state.last_operation = Some(operation.clone());
        state.reported_uncertain = false;
        state.active = Some(Active {
            id,
            kind,
            operation,
            deadline: Instant::now() + execution::DEADLINE,
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

    /// bounded diagnostic snapshot of this owner's last operation. A PID is
    /// observational only; cancellation always uses the owned operation token.
    pub fn decoder_snapshot(&self) -> Option<super::DecoderSnapshot> {
        self.state
            .borrow()
            .last_operation
            .as_ref()
            .and_then(|operation| operation.snapshot())
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
