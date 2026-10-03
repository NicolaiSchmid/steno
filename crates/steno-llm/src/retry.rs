//! Exponential backoff for retryable failures.
//! Swift: `Sources/StenoLLM/RetryPolicy.swift`.

use std::time::Duration;

/// Exponential backoff for retryable failures (408, 429, 5xx, transport
/// errors, timeouts). Deterministic: no jitter, so a test on the manual
/// clock of the `testing` module knows exactly how far to advance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetryPolicy {
    /// Total attempts including the first; 1 disables retries.
    pub max_attempts: u32,
    pub base_delay: Duration,
    pub max_delay: Duration,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        RetryPolicy::new(3, Duration::from_secs(2), Duration::from_secs(30))
    }
}

impl RetryPolicy {
    /// One attempt, no waiting; for tests that never expect a retry.
    pub const NONE: RetryPolicy = RetryPolicy {
        max_attempts: 1,
        base_delay: Duration::from_secs(2),
        max_delay: Duration::from_secs(30),
    };

    #[must_use]
    pub fn new(max_attempts: u32, base_delay: Duration, max_delay: Duration) -> Self {
        RetryPolicy {
            max_attempts: max_attempts.max(1),
            base_delay,
            max_delay,
        }
    }

    /// The default policy with another attempt count.
    #[must_use]
    pub fn with_max_attempts(max_attempts: u32) -> Self {
        RetryPolicy {
            max_attempts: max_attempts.max(1),
            ..RetryPolicy::default()
        }
    }

    /// The wait before retry number `retry` (1 for the first retry): a
    /// server's `Retry-After` when it sent one, else
    /// `base_delay * 2^(retry-1)`, both clamped to `max_delay`.
    #[must_use]
    pub fn delay(&self, retry: u32, retry_after: Option<Duration>) -> Duration {
        if let Some(retry_after) = retry_after {
            return retry_after.min(self.max_delay);
        }
        let exponent = retry.saturating_sub(1).min(30);
        self.base_delay
            .saturating_mul(1 << exponent)
            .min(self.max_delay)
    }
}
