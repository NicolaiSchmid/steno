//! Every failure this crate reports.
//! Swift: `Sources/StenoLLM/LLMError.swift`.

use std::time::Duration;

/// Every failure this crate reports. Bodies and messages are redacted by
/// the client before they get here, so no case ever carries a secret.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum LlmError {
    /// A non-2xx answer that is not a rate limit; `body` is the server's
    /// error message or the first bytes of the body.
    #[error("HTTP {status}: {body}")]
    Http { status: u16, body: String },
    /// The request never completed: DNS, connection refused, dropped socket.
    #[error("transport error: {0}")]
    Transport(String),
    /// The per-attempt timeout on the injected clock elapsed.
    #[error("request timed out")]
    Timeout,
    #[error("{}", rate_limited_message(retry_after.as_ref()))]
    RateLimited { retry_after: Option<Duration> },
    /// The model's text did not decode into the expected type.
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

    /// A failure of the answer rather than of the transport: worth one
    /// retry with the reason appended, never a backoff.
    #[must_use]
    pub fn is_answer_problem(&self) -> bool {
        matches!(
            self,
            LlmError::InvalidJson(_) | LlmError::Truncated | LlmError::Refused(_)
        )
    }
}
