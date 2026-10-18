//! The pluggable boundaries, one trait per file. The pipeline holds each as
//! `Arc<dyn Trait>`.
//!
//! # The async form
//!
//! Asynchronous traits use [`async_trait`](macro@async_trait) because native
//! `async fn` in traits is not dyn-compatible. An implementor holding a handle
//! that is not `Sync` wraps it in a `Mutex` or a worker thread.
//!
//! # Implementing a boundary
//!
//! `#[async_trait]` on the `impl`, `async fn` inside:
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
//! Boundaries return [`BoxError`]; those that must not fail
//! (`DeliveryDispatcher`) return no `Result`.

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
pub use secret_store::{SecretKey, SecretPlace, SecretStore};
pub use speaker_memory::{DEFAULT_MATCH_MARGIN, SpeakerMemory};
pub use speech_engine::SpeechEngine;
pub use transcript_cleaner::TranscriptCleaner;

/// What a boundary reports when it fails: any sendable error, printed for the user.
pub type BoxError = Box<dyn std::error::Error + Send + Sync + 'static>;

pub type BoundaryResult<T> = Result<T, BoxError>;
