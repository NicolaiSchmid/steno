//! Test doubles: a loopback stub server with canned scripts, and a manual
//! clock. Behind the `testing` feature; nothing here ships in the app.
//! Swift: `Sources/StenoLLM/Testing/`.

mod clock;
mod scripts;
mod server;

pub use clock::ManualClock;
pub use scripts::{Responder, Scripts, default_usage, event_stream, parse_segments, scripts};
pub use server::{Behaviour, RecordedRequest, StubChatServer, StubResponse};

/// Wall time a test double, or a test waiting on one, waits before it
/// fails a test that would hang; only spent when a test is already broken.
pub const STALL_DEADLINE: std::time::Duration = std::time::Duration::from_secs(10);

/// `future`'s output, or a panic naming what `stalled` describes once
/// [`STALL_DEADLINE`] of wall time has passed: a sleep nobody advances or a
/// wait nothing ends fails loudly instead of hanging. Only spent when a test
/// is already broken.
async fn or_stall_panic<F: Future>(future: F, stalled: impl FnOnce() -> String) -> F::Output {
    tokio::time::timeout(STALL_DEADLINE, future)
        .await
        .unwrap_or_else(|_| panic!("{} within {STALL_DEADLINE:?} of wall time", stalled()))
}

/// Waits until `done` holds, re-checking whenever `changed` fires. The
/// permit is armed before each check, so a notification between the check
/// and the wait is not lost.
async fn wait_until(changed: &tokio::sync::Notify, done: impl Fn() -> bool) {
    loop {
        let notified = changed.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();
        if done() {
            return;
        }
        notified.await;
    }
}
