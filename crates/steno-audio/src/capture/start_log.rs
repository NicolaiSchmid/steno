//! How loud a backend's `start` logs. While a rebuild's restarts go on
//! (one streak, see `CaptureSession`) the session logs one line when the
//! first of them fails and about one a minute after, with the count, and
//! the lines a start would log on every try (a fallback, the start's
//! timing, a start given up) go to `debug` instead: the session runs the
//! try inside [`quietly`], and the backends log those lines through
//! [`start_log!`]. A backend that starts on a thread of its own reads
//! [`is_quiet`] before the spawn and runs its start there inside
//! [`quietly`] too. Rust only.

use std::cell::Cell;

thread_local! {
    static QUIET: Cell<bool> = const { Cell::new(false) };
}

/// Runs `start` with the starts' own lines at `debug` when `quiet`, and
/// as loud as before after it, a panic included.
pub(crate) fn quietly<T>(quiet: bool, start: impl FnOnce() -> T) -> T {
    struct Restore(bool);
    impl Drop for Restore {
        fn drop(&mut self) {
            QUIET.set(self.0);
        }
    }
    let _restore = Restore(QUIET.replace(quiet));
    start()
}

/// Whether a start on this thread logs its own lines at `debug`.
pub(crate) fn is_quiet() -> bool {
    QUIET.get()
}

/// A start's line at `$level` (`warn`, `info`), or at `debug` inside
/// [`quietly`].
macro_rules! start_log {
    ($level:ident, $($arg:tt)+) => {
        if $crate::capture::start_log::is_quiet() {
            tracing::debug!($($arg)+)
        } else {
            tracing::$level!($($arg)+)
        }
    };
}
pub(crate) use start_log;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quietly_holds_for_its_start_alone() {
        assert!(!is_quiet());
        let inside = quietly(true, || (is_quiet(), quietly(false, is_quiet)));
        assert_eq!(inside, (true, false));
        assert!(!is_quiet());
        let unwound = std::panic::catch_unwind(|| quietly(true, || panic!("a start that panics")));
        assert!(unwound.is_err());
        assert!(!is_quiet(), "restored after a panic too");
    }
}
