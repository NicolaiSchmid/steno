import Foundation
import StenoCore

#if canImport(FoundationNetworking)
  import FoundationNetworking
#endif

/// What the client did, for logs, the CLI and tests on `ManualClock`.
public enum LLMClientEvent: Sendable, Equatable {
  case request(attempt: Int, purpose: String, mode: StructuredOutputMode)
  case response(attempt: Int, status: Int)
  case timedOut(attempt: Int)
  /// Fired right before the backoff sleep begins.
  case retrying(after: Duration, attempt: Int, reason: LLMError)
  case modeDowngraded(to: StructuredOutputMode)
  /// A 400 named this request parameter (`error.param`); the request is
  /// resent without it (`temperature`) or with its successor (`max_tokens`
  /// as `max_completion_tokens`) and the client keeps spelling it that way.
  case parameterRejected(String)
}

/// The one `LanguageModel` implementation: `POST {baseURL}/chat/completions`
/// with Bearer auth, a per-attempt timeout and exponential retries on the
/// injected clock, structured output mode fallback remembered per endpoint,
/// and the API key redacted from every error. Text only ever leaves through
/// here.
public actor OpenAICompatibleClient: LanguageModel {
  public nonisolated let endpoint: LLMEndpoint
  private let apiKey: String?
  private let session: URLSession
  private let retry: RetryPolicy
  private let clock: any Clock<Duration>
  private let observer: (@Sendable (LLMClientEvent) -> Void)?
  private var mode: StructuredOutputMode
  /// Parameters a 400 named (`error.param`) and that the client now spells
  /// differently, remembered per client like the mode: `max_tokens` goes as
  /// `max_completion_tokens` (OpenAI's reasoning models), `temperature` is
  /// left out (they accept only the default). No model-name sniffing.
  private var rejectedParameters: Set<String> = []

  public init(
    endpoint: LLMEndpoint,
    apiKey: String?,
    session: URLSession = .shared,
    retry: RetryPolicy = .default,
    clock: any Clock<Duration> = ContinuousClock(),
    observer: (@Sendable (LLMClientEvent) -> Void)? = nil
  ) {
    self.endpoint = endpoint
    self.apiKey = (apiKey?.isEmpty ?? true) ? nil : apiKey
    self.session = session
    self.retry = retry
    self.clock = clock
    self.observer = observer
    self.mode = endpoint.structuredOutputMode
  }

  /// The structured output mode in use after any fallback so far.
  public var resolvedMode: StructuredOutputMode { mode }

  // MARK: LanguageModel

  public func complete(_ request: LLMRequest) async throws -> LLMResponse {
    var attempt = 1
    while true {
      try Task.checkCancellation()
      let wire = try makeRequest(request, mode: mode)
      observer?(.request(attempt: attempt, purpose: request.purpose, mode: mode))
      let failure: LLMError
      do {
        let reply = try await perform(wire)
        observer?(.response(attempt: attempt, status: reply.status))
        if (200..<300).contains(reply.status) {
          return try parse(reply)
        }
        if reply.status == 400, request.responseFormat.kind != .text,
          Self.complainsAboutResponseFormat(reply.bodyText), let next = mode.downgraded
        {
          mode = next
          observer?(.modeDowngraded(to: next))
          continue
        }
        if reply.status == 400, let param = Self.rejectedParameter(reply),
          Self.adjustableParameters.contains(param), !rejectedParameters.contains(param)
        {
          rejectedParameters.insert(param)
          observer?(.parameterRejected(param))
          continue
        }
        failure = classify(reply)
      } catch let error as LLMError {
        if error == .timeout { observer?(.timedOut(attempt: attempt)) }
        failure = error
      }
      guard failure.isRetryable, attempt < retry.maxAttempts else { throw failure }
      var retryAfter: Duration?
      if case .rateLimited(let after) = failure { retryAfter = after }
      let delay = retry.delay(beforeRetry: attempt, retryAfter: retryAfter)
      observer?(.retrying(after: delay, attempt: attempt, reason: failure))
      try await clock.sleep(for: delay)
      attempt += 1
    }
  }

  /// `GET /models` (whether the model is listed; a server without a list is
  /// tolerated) and one tiny structured completion (mode fallback, round
  /// trip). Any failure of the completion is thrown, so a probe that returns
  /// describes an endpoint both passes can use; a wrong base URL, an unknown
  /// model or an undecodable answer is the caller's to show.
  public func probe() async throws -> EndpointProbe {
    var modelListed: Bool?
    let roundTrip = try await clock.measure {
      var request = URLRequest(url: endpoint.modelsURL)
      request.httpMethod = "GET"
      addHeaders(to: &request, purpose: "probe")
      if let reply = try? await perform(request), (200..<300).contains(reply.status),
        let list = try? WireJSON.decode(ModelList.self, from: reply.body)
      {
        modelListed = list.data.contains { $0.id == endpoint.model }
      }
      _ = try await complete(Self.probeRequest)
    }
    return EndpointProbe(modelListed: modelListed, resolvedMode: mode, roundTrip: roundTrip)
  }

  /// Through the same builder as every other schema, so the strict-subset
  /// walk in the tests covers it.
  static let probeSchema = JSONSchema.object(["ok": .boolean()])

  static let probeRequest = LLMRequest(
    messages: [
      LLMMessage(role: .system, content: "Reply with JSON only."),
      LLMMessage(role: .user, content: "Return exactly {\"ok\": true}."),
    ],
    responseFormat: .jsonSchema(name: "probe", schema: probeSchema.jsonValue, strict: true),
    temperature: 0,
    maxTokens: 32,
    purpose: "probe")

  // MARK: Requests

  struct Reply: Sendable {
    var status: Int
    /// Header names lowercased.
    var headers: [String: String]
    var body: Data

    var bodyText: String { String(decoding: body.prefix(4_096), as: UTF8.self) }
  }

  private func makeRequest(_ request: LLMRequest, mode: StructuredOutputMode) throws -> URLRequest {
    let ceiling = request.maxTokens ?? endpoint.maxOutputTokens
    let renamesMaxTokens = rejectedParameters.contains("max_tokens")
    let body = ChatCompletionRequest(
      model: endpoint.model,
      messages: request.messages.map(ChatMessage.init),
      temperature: rejectedParameters.contains("temperature") ? nil : request.temperature,
      maxTokens: renamesMaxTokens ? nil : ceiling,
      maxCompletionTokens: renamesMaxTokens ? ceiling : nil,
      responseFormat: Self.responseFormat(for: request.responseFormat, mode: mode))
    var urlRequest = URLRequest(url: endpoint.chatCompletionsURL)
    urlRequest.httpMethod = "POST"
    addHeaders(to: &urlRequest, purpose: request.purpose)
    urlRequest.setValue("application/json", forHTTPHeaderField: "Content-Type")
    urlRequest.httpBody = try WireJSON.encode(body)
    return urlRequest
  }

  private func addHeaders(to request: inout URLRequest, purpose: String) {
    request.setValue("application/json", forHTTPHeaderField: "Accept")
    request.setValue("steno/\(StenoCore.version)", forHTTPHeaderField: "User-Agent")
    request.setValue(purpose, forHTTPHeaderField: "X-Steno-Purpose")
    if let apiKey {
      request.setValue("Bearer \(apiKey)", forHTTPHeaderField: "Authorization")
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

  /// The wire `response_format` for a request under `mode`; nil sends none.
  static func responseFormat(for format: LLMResponseFormat, mode: StructuredOutputMode)
    -> ChatResponseFormat?
  {
    switch (format, mode) {
    case (.text, _), (_, .promptOnly):
      return nil
    case (.jsonObject, _), (.jsonSchema, .jsonObject):
      return .jsonObject
    case (.jsonSchema(let name, let schema, let strict), .jsonSchema):
      return .jsonSchema(name: name, schema: schema, strict: strict)
    }
  }

  /// One attempt: the request raced against `requestTimeout` on the clock,
  /// with `URLRequest.timeoutInterval` set to the wall-clock backstop.
  /// Cancellation of the caller cancels the transfer and rethrows
  /// `CancellationError`; the timeout throws `LLMError.timeout`.
  private func perform(_ request: URLRequest) async throws -> Reply {
    let session = self.session
    let clock = self.clock
    let timeout = endpoint.requestTimeout
    let apiKey = self.apiKey
    let backstopped: URLRequest = {
      var copy = request
      copy.timeoutInterval = Self.wallClockBackstop(for: timeout)
      return copy
    }()
    return try await withThrowingTaskGroup(of: Reply?.self) { group in
      group.addTask {
        try await Self.send(backstopped, session: session, apiKey: apiKey)
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

  private nonisolated static func send(_ request: URLRequest, session: URLSession, apiKey: String?)
    async throws -> Reply
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
      throw LLMError.transport(redact(String(describing: error), apiKey: apiKey))
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
    return Reply(status: http.statusCode, headers: headers, body: data)
  }

  // MARK: Replies

  private func parse(_ reply: Reply) throws -> LLMResponse {
    let decoded: ChatCompletionResponse
    do {
      decoded = try WireJSON.decode(ChatCompletionResponse.self, from: reply.body)
    } catch {
      throw LLMError.transport("undecodable completion body: \(redact(reply.bodyText))")
    }
    guard let choice = decoded.choices.first else {
      throw LLMError.transport("completion without choices")
    }
    if let refusal = choice.message.refusal, !refusal.isEmpty {
      throw LLMError.refused(redact(refusal))
    }
    let finish: LLMFinishReason =
      switch choice.finishReason {
      case "stop": .stop
      case "length": .length
      case "content_filter": .contentFilter
      default: .other
      }
    return LLMResponse(
      text: choice.message.content ?? "", finishReason: finish,
      usage: LLMUsage(
        promptTokens: decoded.usage?.promptTokens ?? 0,
        completionTokens: decoded.usage?.completionTokens ?? 0, requests: 1),
      model: decoded.model)
  }

  private func classify(_ reply: Reply) -> LLMError {
    if reply.status == 429 {
      return .rateLimited(retryAfter: Self.retryAfter(reply.headers["retry-after"]))
    }
    return .http(status: reply.status, body: redact(Self.errorMessage(reply)))
  }

  /// The server's `error.message`, else the first 500 characters of the body.
  static func errorMessage(_ reply: Reply) -> String {
    if let envelope = try? WireJSON.decode(ChatErrorEnvelope.self, from: reply.body) {
      return envelope.error.message
    }
    return String(reply.bodyText.prefix(500))
  }

  /// A 400 whose message names the structured output request: the cue to
  /// fall back one mode. Groq, OpenRouter and OpenAI all word it this way.
  static func complainsAboutResponseFormat(_ body: String) -> Bool {
    let lowered = body.lowercased()
    return lowered.contains("response_format") || lowered.contains("json_schema")
      || lowered.contains("json schema") || lowered.contains("structured output")
  }

  /// The parameters a 400 may name that the client can spell differently.
  static let adjustableParameters: Set<String> = ["max_tokens", "temperature"]

  /// `error.param` of a 400 envelope, when the server sent one.
  static func rejectedParameter(_ reply: Reply) -> String? {
    (try? WireJSON.decode(ChatErrorEnvelope.self, from: reply.body))?.error.param
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

  // MARK: Redaction

  private func redact(_ text: String) -> String {
    Self.redact(text, apiKey: apiKey)
  }

  /// Removes the key wherever a server or transport echoed it.
  nonisolated static func redact(_ text: String, apiKey: String?) -> String {
    guard let apiKey, !apiKey.isEmpty else { return text }
    return text.replacingOccurrences(of: apiKey, with: "[redacted]")
  }
}
