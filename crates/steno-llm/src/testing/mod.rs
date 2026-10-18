//! Test doubles: a loopback stub server with canned scripts, and a manual
//! clock. Behind the `testing` feature.

mod clock;
mod scripts;
mod server;

pub use clock::ManualClock;
pub use scripts::{Responder, Scripts, default_usage, event_stream, parse_segments, scripts};
pub use server::{Behaviour, RecordedRequest, StubChatServer, StubResponse};

/// Wall time before a hanging test double fails the test.
pub const STALL_DEADLINE: std::time::Duration = std::time::Duration::from_secs(10);

/// `future`'s output, or a panic naming what `stalled` describes after
/// [`STALL_DEADLINE`], so a hang fails loudly.
async fn or_stall_panic<F: Future>(future: F, stalled: impl FnOnce() -> String) -> F::Output {
    tokio::time::timeout(STALL_DEADLINE, future)
        .await
        .unwrap_or_else(|_| panic!("{} within {STALL_DEADLINE:?} of wall time", stalled()))
}

/// Waits until `done` holds, re-checking whenever `changed` fires; the
/// permit is armed before each check so no notification is lost.
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
