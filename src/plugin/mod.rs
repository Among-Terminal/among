//! Out-of-process plugins.
//!
//! A plugin is a child process speaking newline-delimited JSON. It is never
//! loaded into this process, and that is the whole point: a plugin that
//! segfaults takes only itself down, while the live terminals — which hold the
//! user's running work — stay up.
//!
//! The cost is a process spawn per plugin and a messaging round trip, both of
//! which are invisible next to the work a status plugin actually does.

pub(crate) mod data;
pub mod host;
pub mod inventory;
pub mod manifest;
mod process;
pub mod protocol;
pub mod schema;
mod transport;

pub use protocol::{Context, EventKind, IncomingSummary, TerminalInfo};
