//! Deterministic implementations of every boundary, so the pipeline, the CLI
//! and the shell test without models, a network or a keyring. Behind the
//! `testing` feature. Every fake records its calls in a [`CallLog`], and one
//! with a `failure: Option<String>` field fails every call as a
//! [`FakeFailure`].
//!
//! # Example
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
    /// The `Err` a fake returns for `failure: Some(message)`.
    pub fn check(failure: Option<&String>) -> crate::BoundaryResult<()> {
        match failure {
            Some(message) => Err(Box::new(FakeFailure(message.clone()))),
            None => Ok(()),
        }
    }
}

/// A fake whose test panicked mid-call keeps what it recorded before.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}
