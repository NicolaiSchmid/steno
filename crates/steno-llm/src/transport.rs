//! What both clients share below the wire format: one attempt raced against
//! the clock, the `Retry-After` header, the backoff between attempts and
//! the redaction of secrets from every error.
//! Swift: `Sources/StenoLLM/Transport.swift`.

use std::collections::HashMap;
use std::future::Future;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use steno_core::{LlmResponse, async_trait};

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

/// The structured output mode a client remembers across requests: it
/// starts at the endpoint's and only ever steps down.
#[derive(Debug)]
pub(crate) struct RememberedMode(Mutex<StructuredOutputMode>);

impl RememberedMode {
    pub(crate) fn new(mode: StructuredOutputMode) -> Self {
        RememberedMode(Mutex::new(mode))
    }

    pub(crate) fn get(&self) -> StructuredOutputMode {
        *self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// One step down from `from`, the mode the rejected request went out
    /// under; `false` when there is no weaker mode and the 400 stands. The
    /// step is taken, and announced to `observer`, only while the
    /// remembered mode still is `from`: a late 400 from a concurrent
    /// request that started under an older mode must neither bounce the
    /// mode back up nor announce the same downgrade twice. Either way the
    /// request is worth resending.
    pub(crate) fn step_down(
        &self,
        from: StructuredOutputMode,
        observer: Option<&Observer>,
    ) -> bool {
        let Some(next) = from.downgraded() else {
            return false;
        };
        let moved = {
            let mut mode = self.0.lock().unwrap_or_else(PoisonError::into_inner);
            if *mode == from {
                *mode = next;
                true
            } else {
                false
            }
        };
        if moved {
            notify(observer, LlmClientEvent::ModeDowngraded(next));
        }
        true
    }
}

/// A `reqwest::ClientBuilder` with the TLS provider in place: reqwest is
/// built here without one (`rustls-no-provider`), so this installs rustls's
/// `ring` provider process-wide once (a second install is refused and
/// ignored; whichever provider is in place is used) and hands back the
/// builder. Every client passed to a `with_http` should start here, or TLS
/// panics at the first `https` request.
pub fn http_client_builder() -> reqwest::ClientBuilder {
    static PROVIDER: std::sync::OnceLock<()> = std::sync::OnceLock::new();
    PROVIDER.get_or_init(|| {
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
    reqwest::Client::builder()
}

/// The HTTP client both clients and the credential store share by default:
/// [`http_client_builder`] with reqwest's default settings.
#[must_use]
pub fn default_http_client() -> reqwest::Client {
    http_client_builder()
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
    let response = client
        .execute(request)
        .await
        .map_err(|error| transport_error(&error, secrets))?;
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
        .map_err(|error| transport_error(&error, secrets))?
        .to_vec();
    Ok(HttpReply {
        status,
        headers,
        body,
    })
}

/// reqwest's own timeout (the wall-clock backstop) as [`LlmError::Timeout`],
/// anything else as a redacted [`LlmError::Transport`].
fn transport_error(error: &reqwest::Error, secrets: &[String]) -> LlmError {
    if error.is_timeout() {
        LlmError::Timeout
    } else {
        LlmError::Transport(redact(&error_chain(error), secrets))
    }
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

/// What one attempt of a client came to.
pub(crate) enum Attempt {
    Done(LlmResponse),
    /// Sent again without counting as an attempt: a mode downgrade, a
    /// parameter adjustment, a refreshed sign-in.
    Resend,
    /// The failure to back off from, or to return when the policy is spent.
    Failed(LlmError),
}

/// The loop both clients run: `attempt` with the attempt number, 1 first,
/// until it is done; a resend goes straight back round, a failure
/// announces a timeout and goes through [`back_off`]. An `Err` from
/// `attempt` ends the loop at once.
pub(crate) async fn run_attempts<E, F, Fut>(
    retry: &RetryPolicy,
    clock: &dyn Clock,
    observer: Option<&Observer>,
    mut attempt: F,
) -> Result<LlmResponse, E>
where
    E: From<LlmError>,
    F: FnMut(u32) -> Fut,
    Fut: Future<Output = Result<Attempt, E>>,
{
    let mut number: u32 = 1;
    loop {
        let failure = match attempt(number).await? {
            Attempt::Done(response) => return Ok(response),
            Attempt::Resend => continue,
            Attempt::Failed(failure) => failure,
        };
        if failure == LlmError::Timeout {
            notify(observer, LlmClientEvent::TimedOut { attempt: number });
        }
        back_off(failure, number, retry, clock, observer).await?;
        number += 1;
    }
}

/// A 429 as [`LlmError::RateLimited`] with the reply's `Retry-After`.
pub(crate) fn rate_limited(reply: &HttpReply) -> LlmError {
    LlmError::RateLimited {
        retry_after: retry_after(reply.headers.get("retry-after").map(String::as_str)),
    }
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

/// The first `limit` characters of `text` with every secret removed;
/// redacted before the cut, so a secret straddling it leaves no prefix.
#[must_use]
pub fn redacted_prefix(text: &str, secrets: &[String], limit: usize) -> String {
    redact(text, secrets).chars().take(limit).collect()
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
