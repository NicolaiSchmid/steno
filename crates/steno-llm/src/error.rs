//! The model-service failures of both clients and both passes.

use std::time::Duration;

/// The model-service failures of both clients and both passes; sign-in
/// failures are [`CodexCredentialError`]'s. Bodies are redacted before they
/// get here; the model's answer text is not, and `InvalidJson` may quote it.
/// `Display` is lowercase and technical.
///
/// [`CodexCredentialError`]: crate::CodexCredentialError
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum LlmError {
    /// A non-2xx answer that is not a rate limit; `body` is redacted.
    #[error("HTTP {status}: {body}")]
    Http { status: u16, body: String },
    /// The request never completed: DNS, connection refused, dropped socket.
    #[error("transport error: {0}")]
    Transport(String),
    #[error("request timed out")]
    Timeout,
    /// A 429; `retry_after` is the server's `Retry-After` when it sent one.
    #[error("{}", rate_limited_message(retry_after.as_ref()))]
    RateLimited { retry_after: Option<Duration> },
    #[error("the model returned invalid JSON: {0}")]
    InvalidJson(String),
    /// `finish_reason: length`: the answer was cut off.
    #[error("the model's answer was cut off by the token limit")]
    Truncated,
    /// The model declined; OpenAI's `refusal` field.
    #[error("the model refused: {0}")]
    Refused(String),
    /// The transcript does not fit two levels of map and reduce.
    #[error("transcript too long: about {estimated_tokens} tokens against a budget of {budget}")]
    TranscriptTooLong { estimated_tokens: i64, budget: i64 },
    /// The HTTP client could not be built (for example no trusted root
    /// certificates), so no request is sent.
    #[error("{0}")]
    HttpClientUnavailable(String),
}

fn rate_limited_message(retry_after: Option<&Duration>) -> String {
    match retry_after {
        Some(after) => format!("rate limited, retry after {after:?}"),
        None => "rate limited".to_owned(),
    }
}

impl LlmError {
    /// Whether another attempt could succeed without changing the request.
    #[must_use]
    pub fn is_retryable(&self) -> bool {
        match self {
            LlmError::Http { status, .. } => *status == 408 || (500..=599).contains(status),
            LlmError::Transport(_) | LlmError::Timeout | LlmError::RateLimited { .. } => true,
            _ => false,
        }
    }

    /// A failure of the answer: worth one retry with the reason, no backoff.
    #[must_use]
    pub fn is_answer_problem(&self) -> bool {
        matches!(
            self,
            LlmError::InvalidJson(_) | LlmError::Truncated | LlmError::Refused(_)
        )
    }
}
