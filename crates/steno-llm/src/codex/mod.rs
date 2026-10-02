//! The Codex backend: OpenAI's Responses API with a ChatGPT sign-in read
//! from the Codex CLI's `auth.json`. Wire format and quirks:
//! `.plans/2026-09-29-codex-chatgpt-provider.md`.

mod client;
mod credentials;
mod jwt;

pub use client::{CodexError, CodexResponsesClient};
pub use credentials::{CodexCredentialError, CodexCredentialStore, CodexCredentials};
pub use jwt::JwtClaims;

pub use crate::wire::CodexModel;
