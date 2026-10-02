//! The payload of a JSON Web Token as the Codex sign-in stores them.
//! Swift: `Sources/StenoLLM/Codex/JWTClaims.swift`.

use base64::Engine;
use chrono::{DateTime, Utc};
use serde_json::Value;

/// The middle segment of a JWT, base64url without padding, a JSON object.
/// Nothing here verifies a signature; Steno only reads the claims the Codex
/// CLI itself reads (`exp`, `email`, the account and plan under the OpenAI
/// auth claim) and the server stays the judge of the token.
pub struct JwtClaims;

impl JwtClaims {
    pub const OPENAI_AUTH_CLAIM: &'static str = "https://api.openai.com/auth";
    pub const OPENAI_PROFILE_CLAIM: &'static str = "https://api.openai.com/profile";

    /// The decoded payload, or `None` when `token` is not three
    /// dot-separated segments whose middle one decodes to a JSON object.
    #[must_use]
    pub fn payload(token: &str) -> Option<Value> {
        let parts: Vec<&str> = token.split('.').collect();
        if parts.len() != 3 || parts[1].is_empty() {
            return None;
        }
        let bytes = Self::base64url_decode(parts[1])?;
        let value: Value = serde_json::from_slice(&bytes).ok()?;
        value.is_object().then_some(value)
    }

    /// `exp` as a date; `None` when absent or not a number.
    #[must_use]
    pub fn expiry(token: &str) -> Option<DateTime<Utc>> {
        let seconds = Self::payload(token)?.get("exp")?.as_f64()?;
        // `exp` is seconds since the epoch, well inside `i64` milliseconds.
        #[allow(clippy::cast_possible_truncation)]
        let millis = (seconds * 1_000.0).round() as i64;
        DateTime::from_timestamp_millis(millis)
    }

    /// `email`, else the profile claim's `email`.
    #[must_use]
    pub fn email(token: &str) -> Option<String> {
        let payload = Self::payload(token)?;
        if let Some(email) = payload.get("email").and_then(Value::as_str) {
            return Some(email.to_owned());
        }
        payload
            .get(Self::OPENAI_PROFILE_CLAIM)?
            .get("email")?
            .as_str()
            .map(str::to_owned)
    }

    /// `chatgpt_account_id` under the OpenAI auth claim.
    #[must_use]
    pub fn account_id(token: &str) -> Option<String> {
        Self::auth_claim_string(token, "chatgpt_account_id")
    }

    /// `chatgpt_plan_type` under the OpenAI auth claim: "free", "plus",
    /// "pro", "business", "enterprise", "edu" or whatever the server adds
    /// next.
    #[must_use]
    pub fn plan_type(token: &str) -> Option<String> {
        Self::auth_claim_string(token, "chatgpt_plan_type")
    }

    fn auth_claim_string(token: &str, key: &str) -> Option<String> {
        let payload = Self::payload(token)?;
        let text = payload.get(Self::OPENAI_AUTH_CLAIM)?.get(key)?.as_str()?;
        (!text.is_empty()).then(|| text.to_owned())
    }

    /// base64url, padding optional.
    #[must_use]
    pub fn base64url_decode(text: &str) -> Option<Vec<u8>> {
        base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(text.trim_end_matches('='))
            .ok()
    }

    /// base64url without padding, for the tests' unsigned tokens.
    #[must_use]
    pub fn base64url_encode(bytes: &[u8]) -> String {
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
    }

    /// A JWT with `payload` and a throwaway header and signature; the store
    /// never checks the signature.
    #[must_use]
    pub fn unsigned_token(payload: &Value) -> String {
        let header = Self::base64url_encode(br#"{"alg":"none","typ":"JWT"}"#);
        let body = Self::base64url_encode(
            steno_core::json::to_column_string(payload)
                .unwrap_or_else(|_| "{}".to_owned())
                .as_bytes(),
        );
        format!("{header}.{body}.signature")
    }
}
