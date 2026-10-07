//! Deterministic implementations of every boundary, so the pipeline, the
//! CLI and the shell test without models, a network or a keyring. Behind
//! the `testing` cargo feature and always present in this crate's own
//! tests. The boundary fakes of `Sources/StenoCore/Testing`, minus the
//! dispatcher fake and `RecordingAudioDecoder` (`FakeAudio.swift`);
//! `FileSecretStore`, `ManualClock`, `Gate` and the `on*` hooks are not
//! ported either, and [`sample_data`] is not Swift's `SampleData` (own
//! title, ids and dates).
//!
//! Every fake records what it was asked in a [`CallLog`] named for what it
//! records (`transcriptions`, `summaries`, `admissions`), and a fake with a
//! `failure: Option<String>` field fails every call with that message as a
//! [`FakeFailure`]. The `on*` hooks that run inside a call (`onTranscribe`,
//! `onPrepare`) are the pipeline crate's to add when its tests need them.
//!
//! - [`FakeSpeechEngine`]: one segment per `segment_seconds`.
//! - [`FakeDiarizer`]: `cluster_count` speakers round-robin over
//!   `turn_seconds` turns, or a closure's answer.
//! - [`FakeLanguageModel`]: a queue of canned responses, [`Exhausted`]
//!   when it runs dry.
//! - [`PassthroughCleaner`]: the segments untouched, a fixed usage.
//! - [`FakeSummarizer`]: one bullet per template section, or a canned
//!   summary.
//! - [`FakeDestination`]: `meeting.json` under a temporary root, with
//!   `fail_until` transient failures first.
//! - [`FakeHandoverIntake`]: admissions recorded, a fixed or fresh id.
//! - [`InMemorySecretStore`]: a map.
//! - [`InMemorySpeakerMemory`]: cosine ranking over a list of people.
//! - [`sample_data`]: the meeting, person and export the fakes share.
//! - [`database_one_version_behind`] and [`recorded_migrations`]: a
//!   database an older build left, and its migrations, without the store.
//! - [`WriteLockHold`]: another connection's write transaction, held.
//!
//! # Example
//!
//! A fake answers like the real boundary and remembers what it was asked:
//!
//! ```
//! use steno_core::testing::FakeLanguageModel;
//! use steno_core::{
//!     LanguageModel, LlmFinishReason, LlmMessage, LlmRequest, LlmResponse, LlmResponseFormat,
//!     LlmRole,
//! };
//!
//! # #[tokio::main(flavor = "current_thread")]
//! # async fn main() -> steno_core::BoundaryResult<()> {
//! let model = FakeLanguageModel::new([LlmResponse {
//!     text: "Hallo".to_owned(),
//!     finish_reason: LlmFinishReason::Stop,
//!     usage: None,
//!     model: None,
//! }]);
//! let request = LlmRequest {
//!     messages: vec![LlmMessage {
//!         role: LlmRole::User,
//!         content: "Say hello".to_owned(),
//!     }],
//!     response_format: LlmResponseFormat::Text,
//!     temperature: None,
//!     max_tokens: None,
//!     purpose: "greeting".to_owned(),
//! };
//!
//! assert_eq!(model.complete(&request).await?.text, "Hallo");
//! assert_eq!(model.requests.entries(), vec![request.clone()]);
//! assert!(model.complete(&request).await.is_err(), "the queue ran dry");
//! # Ok(())
//! # }
//! ```

mod call_log;
mod databases;
mod fake_delivery;
mod fake_llm;
mod fake_speech;
mod in_memory_secret_store;
mod in_memory_speaker_memory;
pub mod sample_data;

use std::sync::{Mutex, MutexGuard, PoisonError};

use thiserror::Error;

pub use call_log::CallLog;
pub use databases::{WriteLockHold, database_one_version_behind, recorded_migrations};
pub use fake_delivery::{Admission, FakeDestination, FakeHandoverIntake, Transient};
pub use fake_llm::{Exhausted, FakeLanguageModel, FakeSummarizer, PassthroughCleaner};
pub use fake_speech::{DiarizationFn, FakeDiarizer, FakeSpeechEngine, TranscribeCall};
pub use in_memory_secret_store::InMemorySecretStore;
pub use in_memory_speaker_memory::InMemorySpeakerMemory;

/// The error a fake returns when a test asked it to fail.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("{0}")]
pub struct FakeFailure(pub String);

impl FakeFailure {
    /// The `Err` a fake returns for `failure: Some(message)`, so a fake in
    /// another crate fails the same way.
    pub fn check(failure: Option<&String>) -> crate::BoundaryResult<()> {
        match failure {
            Some(message) => Err(Box::new(FakeFailure(message.clone()))),
            None => Ok(()),
        }
    }
}

/// A poisoned fake is still a fake: a test that panicked mid-call keeps
/// what the others recorded before.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}
