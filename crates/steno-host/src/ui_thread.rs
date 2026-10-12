//! The thread the shell draws on, marked so a host call made there fails
//! in a debug build. Every host call can wait on the store (up to its 5 s
//! busy timeout while another process holds the write lock), on a native
//! prompt or on the recorder's stop, and on the thread that runs the event
//! loop that wait freezes every window, the tray and the floating panels.
//! So the shell marks that thread once ([`mark_ui_thread`], before its
//! event loop starts), and each of its host entry points checks it
//! ([`assert_off_ui_thread`]). The one host call the main thread makes is
//! the launch, during setup, before any window shows; it does not go
//! through those entry points (the host's `Threads` section).
//!
//! The mark is per thread, so a test marks a thread of its own and the
//! others stay unmarked. Rust only: the Swift app's host was `@MainActor`
//! by design.

use std::cell::Cell;

thread_local! {
    static MARKED: Cell<bool> = const { Cell::new(false) };
}

/// Marks the calling thread as the one the shell draws on: its event loop.
pub fn mark_ui_thread() {
    MARKED.set(true);
}

/// Whether the calling thread is the one [`mark_ui_thread`] marked.
#[must_use]
pub fn on_ui_thread() -> bool {
    MARKED.get()
}

/// Fails a debug build when `call` runs on the marked thread; a release
/// build runs it anyway, late rather than not at all.
pub fn assert_off_ui_thread(call: &str) {
    debug_assert!(
        !on_ui_thread(),
        "{call} ran on the UI thread: a host call must run on a blocking thread"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Only the marked thread asserts, and only in a debug build: a call
    /// from any other thread goes through.
    #[test]
    fn a_host_call_on_the_marked_ui_thread_asserts() {
        std::thread::spawn(|| {
            assert!(!on_ui_thread());
            assert_off_ui_thread("recording.toggle");
            mark_ui_thread();
            assert!(on_ui_thread());
            let outcome = std::panic::catch_unwind(|| assert_off_ui_thread("recording.toggle"));
            if cfg!(debug_assertions) {
                let payload = outcome.expect_err("the marked thread asserts");
                let message = payload
                    .downcast_ref::<String>()
                    .map(String::as_str)
                    .unwrap_or_default();
                assert!(
                    message.starts_with("recording.toggle ran on the UI thread"),
                    "{message}"
                );
            } else {
                assert!(outcome.is_ok());
            }
        })
        .join()
        .unwrap();
        std::thread::spawn(|| {
            assert!(!on_ui_thread(), "the mark stays on its own thread");
            assert_off_ui_thread("recording.toggle");
        })
        .join()
        .unwrap();
    }
}
