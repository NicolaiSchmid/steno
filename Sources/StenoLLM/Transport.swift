import Foundation
import StenoCore

#if canImport(FoundationNetworking)
  import FoundationNetworking
#endif

/// One HTTP reply as the clients see it.
struct HTTPReply: Sendable {
  var status: Int
  /// Header names lowercased.
  var headers: [String: String]
  var body: Data

  var bodyText: String { String(decoding: body.prefix(4_096), as: UTF8.self) }
}

/// What both `LanguageModel` clients share below the wire format: one
/// attempt raced against the clock, the `Retry-After` header, the backoff
/// between attempts and the redaction of secrets from every error.
enum LLMTransport {
  /// One attempt: the request raced against `timeout` on the clock, with
  /// `URLRequest.timeoutInterval` set to the wall-clock backstop.
  /// Cancellation of the caller cancels the transfer and rethrows
  /// `CancellationError`; the timeout throws `LLMError.timeout`.
  static func perform(
    _ request: URLRequest, session: URLSession, clock: any Clock<Duration>, timeout: Duration,
    secrets: [String]
  ) async throws -> HTTPReply {
    let backstopped: URLRequest = {
      var copy = request
      copy.timeoutInterval = wallClockBackstop(for: timeout)
      return copy
    }()
    return try await withThrowingTaskGroup(of: HTTPReply?.self) { group in
      group.addTask {
        try await send(backstopped, session: session, secrets: secrets)
      }
      group.addTask {
        // Checked first so an already-cancelled child never registers a
        // sleeper that nothing wakes.
        try Task.checkCancellation()
        try await clock.sleep(for: timeout)
        return nil
      }
      defer { group.cancelAll() }
      guard let first = try await group.next() else { throw LLMError.timeout }
      guard let reply = first else { throw LLMError.timeout }
      return reply
    }
  }

  /// `URLRequest.timeoutInterval`: a wall-clock backstop well past the
  /// clock-driven timeout (twice it, at least 30 s), so a stuck socket
  /// cannot outlive the process when the injected clock never advances.
  /// The transfer then fails with `URLError.timedOut`, which `send` reports
  /// as `LLMError.timeout` like the clock-driven timeout.
  static func wallClockBackstop(for timeout: Duration) -> TimeInterval {
    max(timeout / .seconds(1) * 2, 30)
  }

  private static func send(_ request: URLRequest, session: URLSession, secrets: [String])
    async throws -> HTTPReply
  {
    let data: Data
    let response: URLResponse
    do {
      (data, response) = try await session.data(for: request)
    } catch {
      if Task.isCancelled { throw CancellationError() }
      if let urlError = error as? URLError {
        if urlError.code == .cancelled { throw CancellationError() }
        if urlError.code == .timedOut { throw LLMError.timeout }
      }
      throw LLMError.transport(redact(String(describing: error), secrets: secrets))
    }
    guard let http = response as? HTTPURLResponse else {
      throw LLMError.transport("not an HTTP response")
    }
    var headers: [String: String] = [:]
    for (name, value) in http.allHeaderFields {
      if let name = name as? String, let value = value as? String {
        headers[name.lowercased()] = value
      }
    }
    return HTTPReply(status: http.statusCode, headers: headers, body: data)
  }

  /// Between attempts: rethrows `failure` when it is final (not retryable,
  /// or `attempt` was the last), else announces the policy's backoff and
  /// sleeps it on the clock.
  static func backOff(
    after failure: LLMError, attempt: Int, retry: RetryPolicy, clock: any Clock<Duration>,
    observer: (@Sendable (LLMClientEvent) -> Void)?
  ) async throws {
    guard failure.isRetryable, attempt < retry.maxAttempts else { throw failure }
    var retryAfter: Duration?
    if case .rateLimited(let after) = failure { retryAfter = after }
    let delay = retry.delay(beforeRetry: attempt, retryAfter: retryAfter)
    observer?(.retrying(after: delay, attempt: attempt, reason: failure))
    try await clock.sleep(for: delay)
  }

  /// `Retry-After` in delta seconds, capped at an hour before it becomes a
  /// `Duration` (`Duration.seconds(Double)` traps past about 1.7e20 s, and
  /// `Double("inf")` parses). Anything that is not a finite, non-negative
  /// number, including the HTTP-date form, falls back to the policy's
  /// backoff.
  static let retryAfterCap: Double = 3_600

  static func retryAfter(_ header: String?) -> Duration? {
    guard let header, let seconds = Double(header.trimmingCharacters(in: .whitespaces)),
      seconds.isFinite, seconds >= 0
    else { return nil }
    return .seconds(min(seconds, retryAfterCap))
  }

  /// Removes every secret wherever a server or transport echoed it.
  static func redact(_ text: String, secrets: [String]) -> String {
    var result = text
    for secret in secrets where !secret.isEmpty {
      result = result.replacingOccurrences(of: secret, with: "[redacted]")
    }
    return result
  }
}
