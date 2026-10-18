//! Steno's core: the domain types, the pluggable boundaries and their fakes,
//! the SQLite store and the settings. Every other Steno crate depends on it.
//! The database file is shared with the Swift app until cutover, so every
//! encoding mirrors it; `migrations/README.md` has the schema change procedure.

pub mod content_hash;
pub mod crash_log;
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
    SecretPlace, SecretStore, SpeakerMemory, SpeechEngine, TranscriptCleaner, async_trait,
};
pub use recording_layout::RecordingLayout;
pub use store::{
    AdmissionCommit, ChangeMark, DeletedMeeting, ExpiredAsset, SearchHit, SettingProblem,
    StageRateRow, Store, StoreError,
};
pub use string_enum::UnknownCase;
