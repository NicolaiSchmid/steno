import Foundation

/// Every failure this module reports. Bodies and messages are redacted by
/// the client before they get here, so no case ever carries the API key.
public enum LLMError: Error, Sendable, Equatable, CustomStringConvertible {
  /// A non-2xx answer that is not a rate limit; `body` is the server's
  /// error message or the first bytes of the body.
  case http(status: Int, body: String)
  /// The request never completed: DNS, connection refused, dropped socket.
  case transport(String)
  /// The per-attempt timeout on the injected clock elapsed.
  case timeout
  case rateLimited(retryAfter: Duration?)
  /// The model's text did not decode into the expected type.
  case invalidJSON(String)
  /// `finish_reason: length`: the answer was cut off.
  case truncated
  /// The model declined; OpenAI's `refusal` field.
  case refused(String)
  /// The transcript does not fit two levels of map and reduce.
  case transcriptTooLong(estimatedTokens: Int, budget: Int)

  /// Whether another attempt could succeed without changing the request.
  public var isRetryable: Bool {
    switch self {
    case .http(let status, _): status == 408 || (500...599).contains(status)
    case .transport, .timeout, .rateLimited: true
    default: false
    }
  }

  /// A failure of the answer rather than of the transport: worth one retry
  /// with the reason appended, never a backoff.
  public var isAnswerProblem: Bool {
    switch self {
    case .invalidJSON, .truncated, .refused: true
    default: false
    }
  }

  public var description: String {
    switch self {
    case .http(let status, let body):
      "HTTP \(status): \(body)"
    case .transport(let message):
      "transport error: \(message)"
    case .timeout:
      "request timed out"
    case .rateLimited(let retryAfter):
      if let retryAfter {
        "rate limited, retry after \(retryAfter)"
      } else {
        "rate limited"
      }
    case .invalidJSON(let detail):
      "the model returned invalid JSON: \(detail)"
    case .truncated:
      "the model's answer was cut off by the token limit"
    case .refused(let reason):
      "the model refused: \(reason)"
    case .transcriptTooLong(let estimated, let budget):
      "transcript too long: about \(estimated) tokens against a budget of \(budget)"
    }
  }
}
