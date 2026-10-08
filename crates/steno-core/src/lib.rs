//! Steno's core: the domain types, the pluggable boundaries and their
//! fakes, the SQLite store that shares its file with the Swift app, and
//! the settings. Plan: `.plans/2026-10-02-rust-core-and-tauri-shell.md`.
//!
//! - [`model`]: the domain types, one module per Swift file in
//!   `Sources/StenoCore/Model`, plus the value types the boundaries
//!   exchange (audio buffers, raw segments, diarization results, stage
//!   inputs and outputs, templates).
//! - [`protocols`]: the pluggable boundaries (`SpeechEngine`, `Diarizer`,
//!   `EchoCanceller`, `LanguageModel`, `Destination`, `SecretStore`,
//!   `SpeakerMemory` and the pipeline stage traits), one trait per file,
//!   with the async-trait decision documented once in its module doc.
//! - [`store`]: the SQLite store and its migrations; [`store::convert`]
//!   holds the column codecs a query outside the crate uses.
//! - `testing` (feature `testing`): deterministic fakes for every
//!   boundary, so the pipeline, the CLI and the shell test without models.
//!   The feature also adds `Store::probe_commits`, which lets a test read
//!   each write transaction's pragmas right before it commits.
//! - [`json`]: the `StenoJSON` convention and the date and UUID codecs;
//!   [`json::printer`] holds the Foundation-style printer `meeting.json` and
//!   the bridge use.
//! - [`summary`]: the summary document as Markdown, names substituted.
//! - [`string_enum`](mod@string_enum): the macro every Swift `String` enum is spelled with.
//! - [`paths`]: where the database lives on each platform, and the file URL
//!   codec the store's audio paths use.
//! - [`platform`]: the OS the app runs on, which is also the OS its
//!   calls were recorded on.
//! - [`content_hash`]: the SHA-256 every receipt carries.
//! - [`database_lock`]: the lock that keeps one process per database.
//! - [`recording_layout`]: where one meeting's audio files live.
//!
//! Two rules hold the crate together. It depends on nothing else of ours
//! (every other crate depends on it), so the pipeline, the CLI and the
//! shell share one definition of each type. And the database file is
//! shared with the Swift app until cutover, so every encoding mirrors it:
//! UUIDs as uppercase text, dates in GRDB's `yyyy-MM-dd HH:mm:ss.SSS` UTC
//! form in columns and ISO 8601 in JSON, JSON columns in the `StenoJSON`
//! convention (sorted keys, no escaped slashes), embeddings as
//! little-endian `f32` blobs. A schema change is one PR touching both
//! sides; `migrations/README.md` has the procedure.

pub mod content_hash;
pub mod database_lock;
pub mod json;
pub mod model;
pub mod paths;
pub mod platform;
pub mod protocols;
pub mod recording_layout;
pub mod store;
pub mod string_enum;
pub mod summary;
#[cfg(any(test, feature = "testing"))]
pub mod testing;

pub use database_lock::{DatabaseLock, DatabaseLockError};
pub use model::*;
pub use paths::StenoPaths;
pub use platform::Platform;
pub use protocols::{
    AudioDecoder, BoundaryResult, BoxError, DEFAULT_MATCH_MARGIN, DeliveryDispatcher, Destination,
    Diarizer, EchoCanceller, HandoverIntake, LanguageModel, MeetingSummarizer, SecretKey,
    SecretStore, SpeakerMemory, SpeechEngine, TranscriptCleaner, async_trait,
};
pub use recording_layout::RecordingLayout;
pub use store::{DeletedMeeting, SearchHit, StageRateRow, Store, StoreError};
pub use string_enum::UnknownCase;
