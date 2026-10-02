//! Steno's language model layer: the two [`LanguageModel`] clients (an
//! OpenAI-compatible chat completions endpoint and OpenAI's Codex backend
//! with a ChatGPT sign-in), the transcript cleanup pass and the meeting
//! summary pass. Swift: `Sources/StenoLLM`. Plan:
//! `.plans/2026-10-02-rust-core-and-tauri-shell.md` (WP7).
//!
//! Privacy: this crate is the only code besides a `Destination` that opens
//! a network connection, and it sends text only. Every request body is a
//! prompt built from transcript text, names and template wording; no file
//! path, audio byte, speaker id or raw transcript ever reaches a request,
//! and the stub server tests in `tests/privacy.rs` assert it.
//!
//! - [`OpenAiCompatibleClient`] and [`CodexResponsesClient`]: the clients,
//!   both [`LlmClient`]s with a per-attempt timeout on an injected
//!   [`Clock`], exponential retries, structured output mode fallback and
//!   every secret redacted from every error.
//! - [`CodexCredentialStore`]: `$CODEX_HOME/auth.json` read and refreshed
//!   the way the Codex CLI does it.
//! - [`LlmTranscriptCleaner`] and [`LlmMeetingSummarizer`]: the passes,
//!   over any [`LanguageModel`].
//! - `testing` (behind the feature of that name): the loopback stub server,
//!   the canned scripts and the manual clock.

// Product names (OpenAI, ChatGPT, Codex CLI) and API field names appear in
// most doc comments here; backticks on every one would read as code.
#![allow(clippy::doc_markdown)]

pub mod budget;
pub mod chunker;
pub mod cleanup;
pub mod codex;
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
pub mod support;
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
    CodexResponsesClient, JwtClaims,
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

/// The `User-Agent` and `originator` this crate identifies itself with.
pub const USER_AGENT: &str = concat!("steno/", env!("CARGO_PKG_VERSION"));
