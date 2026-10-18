//! Steno's language model layer: the two [`LanguageModel`] clients
//! ([`OpenAiCompatibleClient`] and [`CodexResponsesClient`]), the transcript
//! cleanup pass and the meeting summary pass. [`LlmEndpoint::from_settings`]
//! gives the endpoint; one client `Arc` then feeds both passes.
//!
//! Privacy: this crate sends text only, never audio. Secrets travel only in
//! headers and the token refresh, and are redacted from every error.

// Product names trip `doc_markdown`; none of them is code.
#![allow(clippy::doc_markdown)]

pub mod budget;
pub mod chunker;
pub mod cleanup;
pub mod codex;
pub mod concurrency;
pub mod decoder;
mod endpoint;
mod error;
pub mod inputs;
pub mod labels;
pub mod language;
mod openai;
mod retry;
pub mod schema;
pub mod summary;
#[cfg(feature = "testing")]
pub mod testing;
pub mod transport;
pub mod wire;

pub use steno_core::LanguageModel;

pub use budget::{BudgetPolicy, TokenBudget};
pub use chunker::{TranscriptChunk, TranscriptChunker};
pub use cleanup::{CleanupDraft, CleanupPromptBuilder, LlmTranscriptCleaner};
pub use codex::{
    CodexCredentialError, CodexCredentialStore, CodexCredentials, CodexError, CodexModel,
    CodexResponsesClient, JwtClaims, Now,
};
pub use decoder::StructuredOutputDecoder;
pub use endpoint::{EndpointProbe, LlmClient, LlmEndpoint, StructuredOutputMode};
pub use error::LlmError;
pub use openai::OpenAiCompatibleClient;
pub use retry::RetryPolicy;
pub use schema::JsonSchema;
pub use summary::{
    AnalysisDraft, ChunkNotes, DraftSpeakerName, DraftTask, LlmMeetingSummarizer,
    SummaryPromptBuilder,
};
pub use transport::{Clock, LlmClientEvent, Observer, SystemClock};

/// The `User-Agent` and `originator` sent to every server. The Codex
/// backend was verified against exactly this string; re-verify if it changes.
pub const USER_AGENT: &str = concat!("steno/", env!("CARGO_PKG_VERSION"));
