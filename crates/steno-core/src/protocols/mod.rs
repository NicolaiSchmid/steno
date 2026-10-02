//! The pluggable boundaries, one trait per file, one for one with
//! `Sources/StenoCore/Protocols`. Two implementations before generalising
//! further; the pipeline holds each as `Arc<dyn Trait>`.
//!
//! - [`SpeechEngine`]: speech to text for one lane.
//! - [`Diarizer`]: who spoke when, for one lane.
//! - [`EchoCanceller`]: the one synchronous, real-time boundary.
//! - [`LanguageModel`]: one completion; sends text and only text.
//! - [`Destination`]: a one-way push target.
//! - [`SecretStore`] and [`SecretKey`]: the API key and whatever follows it.
//! - [`SpeakerMemory`]: known voices across meetings.
//! - [`AudioDecoder`], [`TranscriptCleaner`], [`MeetingSummarizer`],
//!   [`DeliveryDispatcher`], [`HandoverIntake`]: the pipeline's stage
//!   boundaries (`PipelineBoundaries.swift`).
//!
//! # The async form
//!
//! Every asynchronous trait is written with [`async_trait`](macro@async_trait), decided once
//! here: native `async fn` in traits (Rust 1.75) is not dyn-compatible, and
//! the pipeline needs `Arc<dyn SpeechEngine>` and friends, so the methods
//! are boxed futures with a `Send` bound. Implementations put
//! `#[async_trait]` on their `impl` block and write `async fn` as usual.
//! The cost is one allocation per call on boundaries that take seconds to
//! minutes, which is nothing; the audio thread never crosses one of these
//! (see [`EchoCanceller`]).
//!
//! # Implementing a boundary
//!
//! `#[async_trait]` on the `impl`, `async fn` inside, and the result is
//! held as `Arc<dyn Trait>`:
//!
//! ```
//! use std::sync::Arc;
//!
//! use steno_core::{
//!     BoundaryResult, CleanupInput, CleanupOutput, LlmUsage, TranscriptCleaner, async_trait,
//! };
//!
//! /// A cleaner that trims each line and spends no tokens.
//! struct Trim;
//!
//! #[async_trait]
//! impl TranscriptCleaner for Trim {
//!     async fn clean(&self, input: &CleanupInput) -> BoundaryResult<CleanupOutput> {
//!         let mut segments = input.segments.clone();
//!         for segment in &mut segments {
//!             segment.text = segment.text.trim().to_owned();
//!         }
//!         Ok(CleanupOutput {
//!             segments,
//!             failed_chunks: Vec::new(),
//!             usage: LlmUsage::ZERO,
//!         })
//!     }
//! }
//!
//! # #[tokio::main(flavor = "current_thread")]
//! # async fn main() -> BoundaryResult<()> {
//! let cleaner: Arc<dyn TranscriptCleaner> = Arc::new(Trim);
//! let input = CleanupInput {
//!     segments: Vec::new(),
//!     language: None,
//!     participants: Vec::new(),
//!     speakers: Vec::new(),
//!     known_people: Vec::new(),
//! };
//! let output = cleaner.clean(&input).await?;
//! assert_eq!(output.usage, LlmUsage::ZERO);
//! # Ok(())
//! # }
//! ```
//!
//! # Errors
//!
//! Swift's protocols throw `any Error`; the Rust form is [`BoxError`], so
//! an engine, a destination or a client returns its own `thiserror` type
//! through `?` and the pipeline reports it as text. Boundaries that must
//! not fail (`DeliveryDispatcher`) return no `Result`.
//!
//! # Languages
//!
//! Swift distinguishes `LanguageTag` (the stored string) from
//! `Locale.Language` (Foundation's type at the engine boundary) and
//! converts between them at `SpeechEngine`. Rust has no Foundation, so the
//! engine boundary speaks [`LanguageTag`](crate::LanguageTag) directly: one
//! BCP-47 newtype, no conversion.

mod audio_decoder;
mod delivery_dispatcher;
mod destination;
mod diarizer;
mod echo_canceller;
mod handover_intake;
mod language_model;
mod meeting_summarizer;
mod secret_store;
mod speaker_memory;
mod speech_engine;
mod transcript_cleaner;

pub use async_trait::async_trait;

pub use audio_decoder::AudioDecoder;
pub use delivery_dispatcher::DeliveryDispatcher;
pub use destination::Destination;
pub use diarizer::Diarizer;
pub use echo_canceller::EchoCanceller;
pub use handover_intake::HandoverIntake;
pub use language_model::LanguageModel;
pub use meeting_summarizer::MeetingSummarizer;
pub use secret_store::{SecretKey, SecretStore};
pub use speaker_memory::{DEFAULT_MATCH_MARGIN, SpeakerMemory};
pub use speech_engine::SpeechEngine;
pub use transcript_cleaner::TranscriptCleaner;

/// What a boundary reports when it fails: any error, sendable across
/// threads, printed for the user. Swift's `any Error`.
pub type BoxError = Box<dyn std::error::Error + Send + Sync + 'static>;

/// `Result` over [`BoxError`].
pub type BoundaryResult<T> = Result<T, BoxError>;

#[cfg(test)]
mod tests {
    use super::*;

    /// Compiles only when `T` is a trait object the pipeline can hold in an
    /// `Arc`: dyn-compatible, `Send` and `Sync`.
    fn assert_shared_object<T: Send + Sync + ?Sized>() {}

    /// The one boundary the audio thread owns: dyn-compatible and `Send`.
    fn assert_owned_object<T: Send + ?Sized>() {}

    #[test]
    fn boundaries_are_trait_objects_the_pipeline_can_share() {
        assert_shared_object::<dyn SpeechEngine>();
        assert_shared_object::<dyn Diarizer>();
        assert_shared_object::<dyn LanguageModel>();
        assert_shared_object::<dyn Destination>();
        assert_shared_object::<dyn SecretStore>();
        assert_shared_object::<dyn SpeakerMemory>();
        assert_shared_object::<dyn AudioDecoder>();
        assert_shared_object::<dyn TranscriptCleaner>();
        assert_shared_object::<dyn MeetingSummarizer>();
        assert_shared_object::<dyn DeliveryDispatcher>();
        assert_shared_object::<dyn HandoverIntake>();
        assert_owned_object::<dyn EchoCanceller>();
    }
}
