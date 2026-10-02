//! Deterministic implementations of every boundary, so the pipeline, the
//! CLI and the shell test without models, a network or a keyring. Behind
//! the `testing` cargo feature (and always present in this crate's own
//! tests). One for one with `Sources/StenoCore/Testing`, except the
//! dispatcher fake, which waits for the store's export.
//!
//! - [`CallLog`]: what a fake was asked to do, from any thread.
//! - [`FakeSpeechEngine`] and [`FakeDiarizer`]: one segment per second,
//!   speakers round-robin.
//! - [`FakeLanguageModel`], [`PassthroughCleaner`], [`FakeSummarizer`]:
//!   canned answers, fixed usage.
//! - [`FakeDestination`] and [`FakeHandoverIntake`]: files under a
//!   temporary root, admissions recorded.
//! - [`InMemorySecretStore`] and [`InMemorySpeakerMemory`]: maps.
//! - [`sample_data`]: a meeting, a person and an export to feed them.
//!
//! Every fake records its calls in a [`CallLog`] and fails on demand through
//! a `failure` field holding a [`FakeFailure`] message. The Swift fakes'
//! `onTranscribe` style hooks (for `ManualClock` and gates) are not ported
//! yet; the pipeline package adds them when its tests need them.

mod call_log;
mod fake_delivery;
mod fake_llm;
mod fake_speech;
mod in_memory_secret_store;
mod in_memory_speaker_memory;
pub mod sample_data;

use std::sync::{Mutex, MutexGuard, PoisonError};

use thiserror::Error;

pub use call_log::CallLog;
pub use fake_delivery::{Admission, FakeDestination, FakeHandoverIntake, Transient};
pub use fake_llm::{Exhausted, FakeLanguageModel, FakeSummarizer, PassthroughCleaner};
pub use fake_speech::{FakeDiarizer, FakeSpeechEngine, TranscribeCall};
pub use in_memory_secret_store::InMemorySecretStore;
pub use in_memory_speaker_memory::InMemorySpeakerMemory;

/// The error a fake returns when a test asked it to fail.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("{0}")]
pub struct FakeFailure(pub String);

impl FakeFailure {
    /// The `Err` a fake returns for `failure: Some(message)`.
    pub(crate) fn check(failure: Option<&String>) -> crate::BoundaryResult<()> {
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
