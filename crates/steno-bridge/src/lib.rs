//! The JSON contract between the host and the web UI: topics, methods,
//! snapshots, params and the envelope. The fixtures in
//! `apps/macos/web/fixtures/bridge/` are the oracle: `tests/fixtures.rs` proves
//! each decodes here and re-encodes byte for byte. A nested Swift type
//! `Outer.Inner` is named `<Topic><Inner>` here (`PhoneListenerState`).

pub mod commands;
pub mod dispatcher;
pub mod envelope;
pub mod settings;
pub mod snapshots;

pub use commands::*;
pub use dispatcher::*;
pub use envelope::*;
pub use settings::*;
pub use snapshots::*;

/// The printers: `to_canonical_string` is the fixtures' pretty style,
/// `to_compact_string` the dispatcher's one-line style.
pub mod json {
    pub use steno_core::json::{to_canonical_string, to_compact_string};
}
