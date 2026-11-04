//! Owned audio only: no notification ingress, preference writes or other channels.

mod authorization;
mod execution;
mod player;
mod pulse;
pub mod runtime;
mod wav;

pub use execution::finish_shutdown as finish_execution_shutdown;
pub use execution::{
    dispatch as decoder_dispatch, initialize as initialize_execution, DecoderPhase, DecoderSnapshot,
};
pub use player::{Audio, AudioError, Notice, PlaybackKind, Request};
pub use wav::{Sound, WavError, WavInfo, MAX_BYTES, MAX_RATE, MAX_SECONDS};
