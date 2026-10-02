//! Test doubles: a loopback stub server with canned scripts, and a manual
//! clock. Behind the `testing` feature; nothing here ships in the app.
//! Swift: `Sources/StenoLLM/Testing/`.

mod clock;
mod scripts;
mod server;

pub use clock::ManualClock;
pub use scripts::{Responder, Scripts, default_usage, event_stream, parse_segments, scripts};
pub use server::{Behaviour, RecordedRequest, StubChatServer, StubResponse};

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
