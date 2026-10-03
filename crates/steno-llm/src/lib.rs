//! Steno's language model layer: the two [`LanguageModel`] clients, the
//! transcript cleanup pass and the meeting summary pass. Swift:
//! `Sources/StenoLLM`. Plan: `.plans/2026-10-02-rust-core-and-tauri-shell.md`
//! (WP7a).
//!
//! Wiring: [`LlmEndpoint::from_settings`] gives the endpoint, or `None`
//! while summaries are off or not set up. For `LlmProvider::Endpoint`,
//! build the client with [`OpenAiCompatibleClient::from_secret_store`]. For
//! `LlmProvider::Codex`, use `CodexResponsesClient::new(endpoint,
//! Arc::new(CodexCredentialStore::new(CodexCredentialStore::default_home(&env))))`
//! (`env` from `std::env::vars()`; see the [`CodexResponsesClient::new`]
//! example). Then give the same `Arc` and endpoint to
//! [`LlmTranscriptCleaner::new`] and to [`LlmMeetingSummarizer::new`], the
//! latter with the user's time zone.
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
//! - The input side of both passes:
//!   - [`inputs`]: a `MeetingExport` as each pass's input.
//!   - [`labels`]: speaker ids to the labels the model sees, and back.
//!   - [`language`]: the language the summary is written in.
//!   - [`budget`]: tokenizer-free token estimates and the input budgets.
//!   - [`chunker`]: the transcript split into runs that fit one request.
//! - The output side:
//!   - [`schema`]: the JSON Schema builder, limited to what OpenAI's strict
//!     structured outputs accept.
//!   - [`decoder`]: a completion to a typed value, Markdown code fences
//!     tolerated.
//! - [`concurrency`]: bounded fan-out for the chunked passes.
//! - [`wire`]: the request and response shapes of both APIs and the
//!   server-sent events parser.
//! - `retry` ([`RetryPolicy`]): exponential backoff for retryable failures.
//! - [`transport`]: the HTTP attempt raced against the [`Clock`], the
//!   attempt loop, the client events, the HTTP client builder and the
//!   redaction of secrets.
//! - `testing` (feature `testing`): the loopback stub server, the canned
//!   scripts and the manual clock.
//!
//! Privacy: plan invariant 3 lets only a `Destination` and this crate open
//! a network connection, and this crate sends text only. `tests/privacy.rs`
//! asserts each of the following for every client it applies to. A
//! completion body is JSON prompt text built from the transcript, names and
//! template wording; it never holds a file path, an audio byte, a speaker
//! or meeting id, or a segment's `raw_text`. Secrets travel only in headers
//! and in the token refresh: the API key read from the `SecretStore` only
//! in `Authorization`, the Codex access token and account id only in their
//! two headers, the refresh token only in the refresh's body. The Codex
//! tokens are read from `auth.json` (in `$CODEX_HOME`, else `~/.codex`),
//! and a refresh writes them back to it with mode 0600 (on Unix only),
//! leaving no temporary file. No secret, however often or wherever in a
//! body a server echoes it, reaches an error, a `Debug` form or an observer
//! event: a body is redacted whole before it is cut. A key shorter than
//! eight bytes is a placeholder, not a secret, and is left as it is.
//!
//! Tests: `cargo test -p steno-llm`; the dev-dependency on the crate itself
//! turns the `testing` feature on.

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
