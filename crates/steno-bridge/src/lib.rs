//! The JSON contract between the host and the web UI: topics, methods,
//! snapshots, params and the envelope. Swift (`Sources/StenoBridge`) is the
//! source of truth until cutover; the fixtures in
//! `apps/macos/web/fixtures/bridge/` are the oracle both sides encode to, and
//! `tests/fixtures.rs` proves every one of them decodes here and re-encodes
//! byte for byte.
//!
//! Type mapping from Swift: `Int`/`Int64` are `i64`, `Double` is `f64`,
//! `Date` is `chrono::DateTime<Utc>` written as `2026-09-29T12:48:00.000Z`,
//! `UUID` is `uuid::Uuid` written upper case, a nil optional is an omitted
//! key, `String` enums are string enums with the same raw values.
//!
//! WP1 of `.plans/2026-10-02-rust-core-and-tauri-shell.md`.

pub mod commands;
pub mod dispatcher;
pub mod envelope;
pub mod json;
pub mod settings;
pub mod snapshots;

pub use commands::*;
pub use dispatcher::{BridgeHost, Decoding, Dispatcher, EventSink, EventSinkExt, Outcome};
pub use envelope::*;
pub use settings::*;
pub use snapshots::*;

/// The name of the script message handler and of the `window` object the
/// page installs; shared with the web transports.
pub const MESSAGE_HANDLER_NAME: &str = "steno";
