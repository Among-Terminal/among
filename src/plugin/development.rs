//! Plugin development mode of this process.
//!
//! The mode comes only from `--plugin-development` on the command line of the
//! primary instance, and is fixed once before the first window: it is never
//! saved, never read from settings and never changed by a later invocation. A
//! secondary process that asks for it is refused before it can forward anything
//! to the running primary, so an ordinary primary cannot be switched on.

use std::sync::OnceLock;

/// The long command-line option, registered with GApplication.
pub const OPTION: &str = "plugin-development";

/// Printed by a secondary invocation that asked for development mode.
pub const SECONDARY_REFUSAL: &str = "among: --plugin-development applies only when it starts the first Among instance; quit the running instance and start it again with the flag";

static MODE: OnceLock<bool> = OnceLock::new();

/// Fix the mode of the primary instance. Returns false if it was already
/// fixed, which leaves the first value in place.
pub fn fix(enabled: bool) -> bool {
    MODE.set(enabled).is_ok()
}

/// Whether this primary instance was started in development mode. A process
/// that never fixed its mode, such as a unit test, is ordinary.
pub fn enabled() -> bool {
    MODE.get().copied().unwrap_or(false)
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_mode_is_fixed_once_and_ordinary_until_then() {
        // The process-wide value is shared by every test in this binary, so
        // only the order "unset, then fixed once" is checked here; launch order
        // and secondary refusal are checked with real processes by
        // `among-tools check-plugin-development`.
        let before = super::enabled();
        super::fix(before);
        assert_eq!(super::enabled(), before);
        assert!(!super::fix(!before));
        assert_eq!(super::enabled(), before);
    }
}
