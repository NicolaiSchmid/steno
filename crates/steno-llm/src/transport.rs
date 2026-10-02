//! What both clients share below the wire format: one attempt raced against
//! the clock, the `Retry-After` header, the backoff between attempts and
//! the redaction of secrets from every error.
//! Swift: `Sources/StenoLLM/Transport.swift`.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use steno_core::async_trait;

use crate::{LlmError, RetryPolicy, StructuredOutputMode};

/// One HTTP reply as the clients see it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpReply {
    pub status: u16,
    /// Header names lowercased.
    pub headers: HashMap<String, String>,
    pub body: Vec<u8>,
}

impl HttpReply {
    /// The first 4 096 bytes of the body as text.
    #[must_use]
    pub fn body_text(&self) -> String {
        let end = self.body.len().min(4_096);
        String::from_utf8_lossy(&self.body[..end]).into_owned()
    }

    #[must_use]
    pub fn is_success(&self) -> bool {
        (200..300).contains(&self.status)
    }
}

/// The clock the clients wait on: the request timeout and the backoff. The
/// app runs on [`SystemClock`]; tests inject the manual clock of the
/// `testing` module and advance it by hand.
#[async_trait]
pub trait Clock: Send + Sync {
    async fn sleep(&self, duration: Duration);

    /// Time since an arbitrary origin, for round-trip measurement.
    fn now(&self) -> Duration;
}

/// Tokio's timers and `Instant`.
#[derive(Debug)]
pub struct SystemClock {
    origin: Instant,
}

impl Default for SystemClock {
    fn default() -> Self {
        SystemClock {
            origin: Instant::now(),
        }
    }
}

#[async_trait]
impl Clock for SystemClock {
    async fn sleep(&self, duration: Duration) {
        tokio::time::sleep(duration).await;
    }

    fn now(&self) -> Duration {
        self.origin.elapsed()
    }
}

/// What the client did, for logs, the CLI and tests on a manual clock.
#[derive(Debug, Clone, PartialEq)]
pub enum LlmClientEvent {
    Request {
        attempt: u32,
        purpose: String,
        mode: StructuredOutputMode,
    },
    Response {
        attempt: u32,
        status: u16,
    },
    TimedOut {
        attempt: u32,
    },
    /// Fired right before the backoff sleep begins.
    Retrying {
        after: Duration,
        attempt: u32,
        reason: LlmError,
    },
    ModeDowngraded(StructuredOutputMode),
    /// A 400 named this request parameter (`error.param`); the request is
    /// resent without it (`temperature`) or with its successor
    /// (`max_tokens` as `max_completion_tokens`) and the client keeps
    /// spelling it that way.
    ParameterRejected(String),
}

/// Receives every [`LlmClientEvent`] of a client.
pub type Observer = Arc<dyn Fn(LlmClientEvent) + Send + Sync>;

pub(crate) fn notify(observer: Option<&Observer>, event: LlmClientEvent) {
    if let Some(observer) = observer {
        observer(event);
    }
}

/// The HTTP client both clients and the credential store share by default:
/// rustls with the `ring` provider, installed process-wide once (a second
/// install is refused and ignored; whichever provider is in place is used).
#[must_use]
pub fn default_http_client() -> reqwest::Client {
    static PROVIDER: std::sync::OnceLock<()> = std::sync::OnceLock::new();
    PROVIDER.get_or_init(|| {
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
    reqwest::Client::builder()
        .build()
        .expect("a reqwest client with the default settings")
}

/// `Retry-After` in delta seconds is capped at an hour before it becomes a
/// [`Duration`].
pub const RETRY_AFTER_CAP: Duration = Duration::from_secs(3_600);

/// The reqwest timeout: a wall-clock backstop well past the clock-driven
/// timeout (twice it, at least 30 s), so a stuck socket cannot outlive the
/// process when the injected clock never advances. The transfer then fails
/// with reqwest's timeout, which [`perform`] reports as
/// [`LlmError::Timeout`] like the clock-driven one.
#[must_use]
pub fn wall_clock_backstop(timeout: Duration) -> Duration {
    timeout.saturating_mul(2).max(Duration::from_secs(30))
}

/// One attempt: the request raced against `timeout` on the clock, with the
/// wall-clock backstop as the transfer's own timeout. Dropping the future
/// cancels the transfer.
pub async fn perform(
    client: &reqwest::Client,
    mut request: reqwest::Request,
    clock: &dyn Clock,
    timeout: Duration,
    secrets: &[String],
) -> Result<HttpReply, LlmError> {
    *request.timeout_mut() = Some(wall_clock_backstop(timeout));
    tokio::select! {
        reply = send(client, request, secrets) => reply,
        () = clock.sleep(timeout) => Err(LlmError::Timeout),
    }
}

async fn send(
    client: &reqwest::Client,
    request: reqwest::Request,
    secrets: &[String],
) -> Result<HttpReply, LlmError> {
    let response = client.execute(request).await.map_err(|error| {
        if error.is_timeout() {
            LlmError::Timeout
        } else {
            LlmError::Transport(redact(&error_chain(&error), secrets))
        }
    })?;
    let status = response.status().as_u16();
    let headers = response
        .headers()
        .iter()
        .map(|(name, value)| {
            (
                name.as_str().to_ascii_lowercase(),
                String::from_utf8_lossy(value.as_bytes()).into_owned(),
            )
        })
        .collect();
    let body = response
        .bytes()
        .await
        .map_err(|error| {
            if error.is_timeout() {
                LlmError::Timeout
            } else {
                LlmError::Transport(redact(&error_chain(&error), secrets))
            }
        })?
        .to_vec();
    Ok(HttpReply {
        status,
        headers,
        body,
    })
}

/// reqwest's message plus every source, so "connection refused" reaches the
/// user and not only "error sending request".
fn error_chain(error: &dyn std::error::Error) -> String {
    let mut text = error.to_string();
    let mut source = error.source();
    while let Some(cause) = source {
        text.push_str(": ");
        text.push_str(&cause.to_string());
        source = cause.source();
    }
    text
}

/// Between attempts: returns `failure` when it is final (not retryable, or
/// `attempt` was the last), else announces the policy's backoff and sleeps
/// it on the clock.
pub async fn back_off(
    failure: LlmError,
    attempt: u32,
    retry: &RetryPolicy,
    clock: &dyn Clock,
    observer: Option<&Observer>,
) -> Result<(), LlmError> {
    if !failure.is_retryable() || attempt >= retry.max_attempts {
        return Err(failure);
    }
    let retry_after = match &failure {
        LlmError::RateLimited { retry_after } => *retry_after,
        _ => None,
    };
    let delay = retry.delay(attempt, retry_after);
    notify(
        observer,
        LlmClientEvent::Retrying {
            after: delay,
            attempt,
            reason: failure,
        },
    );
    clock.sleep(delay).await;
    Ok(())
}

/// `Retry-After` in delta seconds, capped at an hour. Anything that is not
/// a finite, non-negative number, including the HTTP-date form, falls back
/// to the policy's backoff (`None`).
#[must_use]
pub fn retry_after(header: Option<&str>) -> Option<Duration> {
    let seconds: f64 = header?.trim().parse().ok()?;
    if !seconds.is_finite() || seconds < 0.0 {
        return None;
    }
    Some(Duration::from_secs_f64(
        seconds.min(RETRY_AFTER_CAP.as_secs_f64()),
    ))
}

/// Removes every secret wherever a server or transport echoed it.
#[must_use]
pub fn redact(text: &str, secrets: &[String]) -> String {
    let mut result = text.to_owned();
    for secret in secrets.iter().filter(|secret| !secret.is_empty()) {
        result = result.replace(secret.as_str(), "[redacted]");
    }
    result
}
