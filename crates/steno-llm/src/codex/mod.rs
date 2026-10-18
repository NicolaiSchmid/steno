//! The Codex backend: OpenAI's Responses API with a ChatGPT sign-in read
//! from the Codex CLI's `auth.json`.

mod client;
mod credentials;
mod jwt;

pub use client::{CodexError, CodexResponsesClient};
pub use credentials::{CodexCredentialError, CodexCredentialStore, CodexCredentials, Now};
pub use jwt::JwtClaims;

pub use crate::wire::CodexModel;
