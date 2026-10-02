//! The JSON contract between the host and the web UI: topics, methods,
//! snapshots, params and the envelope. Swift (`Sources/StenoBridge`) is the
//! source of truth until cutover; the fixtures in
//! `apps/macos/web/fixtures/bridge/` are the oracle both sides encode to.
//! WP1 of `.plans/2026-10-02-rust-core-and-tauri-shell.md` fills this crate.

/// The name of the script message handler and of the `window` object the
/// page installs; shared with the web transports.
pub const MESSAGE_HANDLER_NAME: &str = "steno";
