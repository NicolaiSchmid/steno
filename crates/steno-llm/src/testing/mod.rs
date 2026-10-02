//! Test doubles: a loopback stub server with canned scripts, and a manual
//! clock. Behind the `testing` feature; nothing here ships in the app.
//! Swift: `Sources/StenoLLM/Testing/`.

mod clock;
mod scripts;
mod server;

pub use clock::ManualClock;
pub use scripts::{Responder, Scripts, default_usage, event_stream, parse_segments, scripts};
pub use server::{Behaviour, RecordedRequest, StubChatServer, StubResponse};
