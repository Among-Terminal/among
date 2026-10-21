//! Owned audio only: no notification ingress, preference writes or other channels.

mod player;
pub mod runtime;
mod wav;

pub use player::{Audio, AudioError, Notice, PlaybackKind, Request};
pub use wav::{Sound, WavError, WavInfo, MAX_BYTES, MAX_RATE, MAX_SECONDS};
