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
    self.mode =
      endpoint.structuredOutputMode == .auto ? .jsonSchema : endpoint.structuredOutputMode
  }

  /// The structured output mode in use after any fallback so far.
  public var resolvedMode: StructuredOutputMode { mode }

  // MARK: LanguageModel

  public func complete(_ request: LLMRequest) async throws -> LLMResponse {
    var attempt = 1
    var downgrades = 0
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
          Self.complainsAboutResponseFormat(reply.bodyText), downgrades < 2,
          let next = mode.downgraded
        {
          mode = next
          downgrades += 1
          observer?(.modeDowngraded(to: next))
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

  /// `GET /models` (reachability, whether the model is listed) and one tiny
  /// structured completion (mode fallback, round trip). Throws the
  /// completion's error when the server never answered anything.
  public func probe() async throws -> EndpointProbe {
    var reachable = false
    var modelListed: Bool?
    var completionError: LLMError?
    let roundTrip = try await clock.measure {
      do {
        var request = URLRequest(url: endpoint.modelsURL)
        request.httpMethod = "GET"
        addHeaders(to: &request, purpose: "probe")
        let reply = try await perform(request)
        reachable = true
        if (200..<300).contains(reply.status),
          let list = try? WireJSON.decode(ModelList.self, from: reply.body)
        {
          modelListed = list.data.contains { $0.id == endpoint.model }
        }
      } catch let error as LLMError {
        completionError = error
      }
      do {
        _ = try await complete(Self.probeRequest)
        reachable = true
        completionError = nil
      } catch let error as LLMError {
        completionError = error
      }
    }
    if let completionError, !reachable { throw completionError }
    if let completionError, case .http(let status, _) = completionError,
      status == 401 || status == 403
    {
      throw completionError
    }
    return EndpointProbe(
      reachable: reachable, modelListed: modelListed, resolvedMode: mode, roundTrip: roundTrip)
  }

  static let probeRequest = LLMRequest(
    messages: [
      LLMMessage(role: .system, content: "Reply with JSON only."),
      LLMMessage(role: .user, content: "Return exactly {\"ok\": true}."),
    ],
    responseFormat: .jsonSchema(
      name: "probe",
      schema: [
        "type": "object", "properties": ["ok": ["type": "boolean"]], "required": ["ok"],
        "additionalProperties": false,
      ],
      strict: true),
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
    let body = ChatCompletionRequest(
      model: endpoint.model,
      messages: request.messages.map(ChatMessage.init),
      temperature: request.temperature,
      maxTokens: request.maxTokens ?? endpoint.maxOutputTokens,
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
    request.setValue(purpose, forHTTPHeaderField: StenoLLM.purposeHeader)
    if let apiKey {
      request.setValue("Bearer \(apiKey)", forHTTPHeaderField: "Authorization")
    }
    // A wall-clock backstop well past the clock-driven timeout, so a stuck
    // socket cannot outlive the process when the clock never advances.
    request.timeoutInterval = max(Self.seconds(endpoint.requestTimeout) * 2, 30)
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
    case (.jsonSchema(let name, let schema, let strict), .jsonSchema),
      (.jsonSchema(let name, let schema, let strict), .auto):
      return .jsonSchema(name: name, schema: schema, strict: strict)
    }
  }

  /// One attempt: the request raced against `requestTimeout` on the clock.
  /// Cancellation of the caller cancels the transfer and rethrows
  /// `CancellationError`; the timeout throws `LLMError.timeout`.
  private func perform(_ request: URLRequest) async throws -> Reply {
    let session = self.session
    let clock = self.clock
    let timeout = endpoint.requestTimeout
    let apiKey = self.apiKey
    return try await withThrowingTaskGroup(of: Reply?.self) { group in
      group.addTask {
        try await Self.send(request, session: session, apiKey: apiKey)
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
    let usage =
      decoded.usage.map {
        LLMUsage(promptTokens: $0.promptTokens, completionTokens: $0.completionTokens, requests: 1)
      } ?? LLMUsage(promptTokens: 0, completionTokens: 0, requests: 1)
    return LLMResponse(
      text: choice.message.content ?? "", finishReason: finish, usage: usage, model: decoded.model)
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

  /// `Retry-After` in delta seconds; the HTTP-date form is not parsed and
  /// falls back to the policy's backoff.
  static func retryAfter(_ header: String?) -> Duration? {
    guard let header, let seconds = Double(header.trimmingCharacters(in: .whitespaces)),
      seconds >= 0
    else { return nil }
    return .milliseconds(Int(seconds * 1000))
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

  static func seconds(_ duration: Duration) -> TimeInterval {
    let (seconds, attoseconds) = duration.components
    return TimeInterval(seconds) + TimeInterval(attoseconds) / 1e18
  }
}
