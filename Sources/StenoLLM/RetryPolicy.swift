import Foundation

/// Exponential backoff for retryable failures (408, 429, 5xx, transport
/// errors, timeouts). Deterministic: no jitter, so a test on `ManualClock`
/// knows exactly how far to advance.
public struct RetryPolicy: Sendable, Equatable {
  /// Total attempts including the first; 1 disables retries.
  public var maxAttempts: Int
  public var baseDelay: Duration
  public var maxDelay: Duration

  public init(
    maxAttempts: Int = 3, baseDelay: Duration = .seconds(2), maxDelay: Duration = .seconds(30)
  ) {
    self.maxAttempts = max(1, maxAttempts)
    self.baseDelay = baseDelay
    self.maxDelay = maxDelay
  }

  public static let `default` = RetryPolicy()
  /// One attempt, no waiting; for tests that never expect a retry.
  public static let none = RetryPolicy(maxAttempts: 1)

  /// The wait before retry number `retry` (1 for the first retry): a
  /// server's `Retry-After` when it sent one, else `baseDelay * 2^(retry-1)`,
  /// both clamped to `maxDelay`.
  public func delay(beforeRetry retry: Int, retryAfter: Duration? = nil) -> Duration {
    if let retryAfter {
      return min(max(retryAfter, .zero), maxDelay)
    }
    var delay = baseDelay
    for _ in 1..<max(retry, 1) {
      delay = delay * 2
      if delay >= maxDelay { break }
    }
    return min(delay, maxDelay)
  }
}
