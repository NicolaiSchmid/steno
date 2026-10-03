//! Steno's language model layer: the two [`LanguageModel`] clients, the
//! transcript cleanup pass and the meeting summary pass. Swift:
//! `Sources/StenoLLM`. Plan: `.plans/2026-10-02-rust-core-and-tauri-shell.md`
//! (WP7a).
//!
//! - `endpoint` ([`LlmEndpoint`], [`StructuredOutputMode`], [`EndpointProbe`],
//!   [`LlmClient`]): where the model lives, how much it holds, how JSON is
//!   asked for, and the trait both clients implement.
//! - `openai` ([`OpenAiCompatibleClient`]): `POST {base}/chat/completions`
//!   with Bearer auth, for LM Studio, Ollama, Groq, OpenRouter and OpenAI.
//! - [`codex`]: OpenAI's Codex backend with a ChatGPT sign-in; `client`
//!   ([`CodexResponsesClient`]) speaks the Responses API, `credentials`
//!   ([`CodexCredentialStore`]) reads and refreshes the CLI's `auth.json`,
//!   `jwt` ([`JwtClaims`]) reads the claims in its tokens.
//! - [`cleanup`]: pass 1, the transcript in chunks through the model, fixing
//!   speech-to-text mistakes while keeping count, order and wording.
//! - [`summary`]: pass 2, title, structured summary, decisions, tasks and
//!   speaker names, single-shot or map and reduce.
//! - [`budget`]: tokenizer-free token estimates and the input budgets.
//! - [`chunker`]: the transcript split into runs that fit one request.
//! - [`schema`]: the JSON Schema builder, limited to the strict subset.
//! - [`decoder`]: a completion to a typed value, fences tolerated.
//! - [`inputs`]: a `MeetingExport` as each pass's input.
//! - [`labels`]: speaker ids to the labels the model sees, and back.
//! - [`language`]: the language the summary is written in.
//! - [`concurrency`]: bounded fan-out for the chunked passes.
//! - `retry` ([`RetryPolicy`]): exponential backoff for retryable failures.
//! - [`transport`]: one attempt raced against the [`Clock`], `Retry-After`,
//!   the backoff, the redaction of secrets, and the HTTP client builder.
//! - [`wire`]: the request and response shapes of both APIs and the
//!   server-sent events parser.
//! - `testing` (behind the feature of that name): the loopback stub server,
//!   the canned scripts and the manual clock.
//!
//! Privacy: this crate is the only code besides a `Destination` that opens
//! a network connection, and it sends text only. Every request body is a
//! prompt built from transcript text, names and template wording; no file
//! path, audio byte, speaker id or raw transcript ever reaches a request,
//! and the stub server tests in `tests/privacy.rs` assert it. No secret
//! reaches an error or a `Debug` form either.
//!
//! Tests: `cargo test -p steno-llm`; the stub server, scripts and manual
//! clock come with the `testing` feature, which the dev-dependency on the
//! crate itself turns on.

// Product names (OpenAI, ChatGPT, OpenRouter) trip `doc_markdown` on some
// thirty doc lines; none of them is code.
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

/// The `User-Agent` and `originator` this crate identifies itself with:
/// `steno/<crate version>`. Swift sends the app version in its place; the
/// Codex backend was verified against exactly this string (plan
/// `.plans/2026-09-29-codex-chatgpt-provider.md`), so the Tauri shell that
/// wants the app version on the wire changes it here and re-verifies.
pub const USER_AGENT: &str = concat!("steno/", env!("CARGO_PKG_VERSION"));
