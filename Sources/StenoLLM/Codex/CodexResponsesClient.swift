import Foundation
import StenoCore

#if canImport(FoundationNetworking)
  import FoundationNetworking
#endif

/// The second `LanguageModel`: the Responses API on OpenAI's Codex backend
/// with the ChatGPT sign-in from `CodexCredentialStore`. Same shape as
/// `OpenAICompatibleClient` (per-attempt timeout on the injected clock,
/// exponential retries, structured output mode fallback remembered per
/// client, every secret redacted from every error), with three differences
/// the backend forces: the answer arrives as an event stream and is
/// buffered whole, a 401 refreshes the sign-in once before it counts, and
/// no output ceiling or temperature is sent because the backend rejects
/// them. Identifies itself as Steno (`User-Agent`, `originator`).
public actor CodexResponsesClient: LLMClient {
  public nonisolated let endpoint: LLMEndpoint
  private let credentials: CodexCredentialStore
  private let session: URLSession
  private let retry: RetryPolicy
  private let clock: any Clock<Duration>
  private let observer: (@Sendable (LLMClientEvent) -> Void)?
  private var mode: StructuredOutputMode
  /// One per client, so the backend can group a meeting's requests.
  private let sessionID = UUID().uuidString.lowercased()

  /// The `client_version` the model list is filtered by: the server hides
  /// models newer than the Codex version named, so a high sentinel shows
  /// them all. A capability filter, not who we are; that is in the headers.
  static let modelListClientVersion = "99.0.0"
  static let originator = "steno"

  public init(
    endpoint: LLMEndpoint,
    credentials: CodexCredentialStore,
    session: URLSession = .shared,
    retry: RetryPolicy = .default,
    clock: any Clock<Duration> = ContinuousClock(),
    observer: (@Sendable (LLMClientEvent) -> Void)? = nil
  ) {
    self.endpoint = endpoint
    self.credentials = credentials
    self.session = session
    self.retry = retry
    self.clock = clock
    self.observer = observer
    self.mode = endpoint.structuredOutputMode
  }

  public var resolvedMode: StructuredOutputMode { mode }

  // MARK: LanguageModel

  public func complete(_ request: LLMRequest) async throws -> LLMResponse {
    var attempt = 1
    var refreshedAfterUnauthorized = false
    while true {
      try Task.checkCancellation()
      let signIn = try await credentials.current()
      let wire = try makeRequest(request, mode: mode, signIn: signIn)
      observer?(.request(attempt: attempt, purpose: request.purpose, mode: mode))
      let failure: LLMError
      do {
        let reply = try await perform(wire, secrets: signIn.secrets)
        observer?(.response(attempt: attempt, status: reply.status))
        if (200..<300).contains(reply.status) {
          return try parse(reply, secrets: signIn.secrets)
        }
        if reply.status == 401, !refreshedAfterUnauthorized {
          // The file may hold a token the CLI already rotated; one refresh,
          // then a 401 is the answer.
          refreshedAfterUnauthorized = true
          _ = try await credentials.refreshed()
          continue
        }
        if reply.status == 400, request.responseFormat.kind != .text,
          Self.complainsAboutTextFormat(reply.bodyText), let next = mode.downgraded
        {
          mode = next
          observer?(.modeDowngraded(to: next))
          continue
        }
        failure = classify(reply, secrets: signIn.secrets)
      } catch let error as LLMError {
        if error == .timeout { observer?(.timedOut(attempt: attempt)) }
        failure = error
      }
      try await LLMTransport.backOff(
        after: failure, attempt: attempt, retry: retry, clock: clock, observer: observer)
      attempt += 1
    }
  }

  /// The models the backend offers this account, listed ones first in the
  /// server's order. Throws the credential error when there is no sign-in.
  public func listModels() async throws -> [CodexModel] {
    let signIn = try await credentials.current()
    let reply = try await perform(modelsRequest(signIn: signIn), secrets: signIn.secrets)
    guard (200..<300).contains(reply.status) else { throw classify(reply, secrets: signIn.secrets) }
    do {
      return try WireJSON.decode(CodexModelList.self, from: reply.body).models
    } catch {
      throw LLMError.transport(
        "undecodable model list: \(Self.redact(reply.bodyText, signIn.secrets))")
    }
  }

  /// The sign-in's account line (no network), the model list (tolerated
  /// when it fails) and one tiny structured completion. Any failure of the
  /// completion is thrown.
  public func probe() async throws -> EndpointProbe {
    let signIn = try await credentials.current()
    var modelListed: Bool?
    let roundTrip = try await clock.measure {
      if let models = try? await listModels() {
        modelListed = models.contains { $0.slug == endpoint.model }
      }
      _ = try await complete(OpenAICompatibleClient.probeRequest)
    }
    return EndpointProbe(
      modelListed: modelListed, resolvedMode: mode, roundTrip: roundTrip,
      accountLine: signIn.accountLine)
  }

  // MARK: Requests

  typealias Reply = HTTPReply

  /// `low` for the many small cleanup chunks and the probe, `medium` for the
  /// summary passes.
  static func reasoningEffort(for purpose: String) -> String {
    purpose.hasPrefix("cleanup") || purpose == "probe" ? "low" : "medium"
  }

  static func textFormat(for format: LLMResponseFormat, mode: StructuredOutputMode)
    -> ResponsesRequest.Format?
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

  static func body(
    for request: LLMRequest, model: String, mode: StructuredOutputMode
  ) -> ResponsesRequest {
    let system = request.messages.filter { $0.role == .system }.map(\.content)
    let rest = request.messages.filter { $0.role != .system }
    return ResponsesRequest(
      model: model,
      instructions: system.isEmpty ? nil : system.joined(separator: "\n\n"),
      input: rest.map(ResponsesInputItem.init),
      reasoning: .init(effort: reasoningEffort(for: request.purpose)),
      text: textFormat(for: request.responseFormat, mode: mode).map { .init(format: $0) })
  }

  private func makeRequest(
    _ request: LLMRequest, mode: StructuredOutputMode, signIn: CodexCredentials
  ) throws -> URLRequest {
    var urlRequest = URLRequest(url: endpoint.responsesURL)
    urlRequest.httpMethod = "POST"
    addHeaders(to: &urlRequest, purpose: request.purpose, signIn: signIn)
    urlRequest.setValue("application/json", forHTTPHeaderField: "Content-Type")
    urlRequest.setValue("text/event-stream", forHTTPHeaderField: "Accept")
    urlRequest.httpBody = try WireJSON.encode(
      Self.body(for: request, model: endpoint.model, mode: mode))
    return urlRequest
  }

  private func modelsRequest(signIn: CodexCredentials) -> URLRequest {
    var components = URLComponents(url: endpoint.modelsURL, resolvingAgainstBaseURL: false)!
    components.queryItems = [
      URLQueryItem(name: "client_version", value: Self.modelListClientVersion)
    ]
    var request = URLRequest(url: components.url!)
    request.httpMethod = "GET"
    addHeaders(to: &request, purpose: "models", signIn: signIn)
    request.setValue("application/json", forHTTPHeaderField: "Accept")
    return request
  }

  private func addHeaders(to request: inout URLRequest, purpose: String, signIn: CodexCredentials) {
    request.setValue("steno/\(StenoCore.version)", forHTTPHeaderField: "User-Agent")
    request.setValue(Self.originator, forHTTPHeaderField: "originator")
    request.setValue(purpose, forHTTPHeaderField: "X-Steno-Purpose")
    request.setValue("Bearer \(signIn.accessToken)", forHTTPHeaderField: "Authorization")
    request.setValue(signIn.accountID, forHTTPHeaderField: "ChatGPT-Account-ID")
    request.setValue(sessionID, forHTTPHeaderField: "session-id")
  }

  /// One attempt raced against `endpoint.requestTimeout` on the clock.
  private func perform(_ request: URLRequest, secrets: [String]) async throws -> Reply {
    try await LLMTransport.perform(
      request, session: session, clock: clock, timeout: endpoint.requestTimeout, secrets: secrets)
  }

  // MARK: Replies

  private func parse(_ reply: Reply, secrets: [String]) throws -> LLMResponse {
    if ServerSentEvents.looksLikeEventStream(
      contentType: reply.headers["content-type"], body: reply.body)
    {
      return try Self.parseStream(reply.body, redact: { Self.redact($0, secrets) })
    }
    // A backend that answered the whole response object at once.
    guard let response = try? WireJSON.decode(ResponsesResponse.self, from: reply.body) else {
      throw LLMError.transport(
        "undecodable response body: \(Self.redact(reply.bodyText, secrets))")
    }
    return try Self.result(
      from: response, items: response.output ?? [], redact: { Self.redact($0, secrets) })
  }

  /// The buffered event stream to one response: message items from
  /// `response.output_item.done`, the status and usage from
  /// `response.completed` or `response.incomplete`; `response.failed` and
  /// `error` throw. A stream that ends without a terminal event is a
  /// transport failure (the connection was cut).
  static func parseStream(_ body: Data, redact: (String) -> String) throws -> LLMResponse {
    var items: [ResponsesOutputItem] = []
    var terminal: ResponsesResponse?
    for event in ServerSentEvents.parse(body) {
      guard
        let decoded = try? WireJSON.decode(ResponsesStreamEvent.self, from: Data(event.data.utf8))
      else { continue }
      switch event.event ?? decoded.type {
      case "response.output_item.done":
        if let item = decoded.item { items.append(item) }
      case "response.completed", "response.incomplete":
        terminal = decoded.response
      case "response.failed":
        let message = decoded.response?.error?.message ?? "the response failed"
        throw LLMError.transport(redact(message))
      case "error":
        throw LLMError.transport(redact(decoded.message ?? decoded.code ?? "stream error"))
      default:
        continue
      }
    }
    guard let terminal else {
      throw LLMError.transport("stream closed before response.completed")
    }
    return try result(
      from: terminal, items: items.isEmpty ? (terminal.output ?? []) : items, redact: redact)
  }

  static func result(
    from response: ResponsesResponse, items: [ResponsesOutputItem], redact: (String) -> String
  ) throws -> LLMResponse {
    let messages = items.filter { $0.type == "message" }
    if let refusal = messages.compactMap(\.refusal).first, !refusal.isEmpty {
      throw LLMError.refused(redact(refusal))
    }
    let finish: LLMFinishReason =
      switch (response.status, response.incompleteDetails?.reason) {
      case ("completed", _): .stop
      case ("incomplete", "max_output_tokens"?): .length
      case ("incomplete", "content_filter"?): .contentFilter
      case (nil, _): .stop
      default: .other
      }
    return LLMResponse(
      text: messages.map(\.text).joined(),
      finishReason: finish,
      usage: LLMUsage(
        promptTokens: response.usage?.inputTokens ?? 0,
        completionTokens: response.usage?.outputTokens ?? 0, requests: 1),
      model: response.model)
  }

  /// A 429 is a rate limit unless the body names the plan's usage limit,
  /// which no backoff cures within a meeting; that and every other status
  /// are `http`, not retried unless 408 or 5xx.
  private func classify(_ reply: Reply, secrets: [String]) -> LLMError {
    let envelope = try? WireJSON.decode(CodexErrorEnvelope.self, from: reply.body)
    let message = Self.redact(envelope?.message ?? String(reply.bodyText.prefix(500)), secrets)
    if let kind = envelope?.kind, Self.planLimitKinds.contains(kind) {
      return .http(status: reply.status, body: "ChatGPT plan limit reached: \(message)")
    }
    if reply.status == 429 {
      return .rateLimited(retryAfter: LLMTransport.retryAfter(reply.headers["retry-after"]))
    }
    return .http(status: reply.status, body: message)
  }

  static let planLimitKinds: Set<String> = ["usage_limit_reached", "usage_not_included"]

  /// A 400 that names the structured output request (the Responses API
  /// spells it `text.format`): the cue to fall back one mode.
  static func complainsAboutTextFormat(_ body: String) -> Bool {
    body.lowercased().contains("text.format")
      || OpenAICompatibleClient.complainsAboutResponseFormat(body)
  }

  private nonisolated static func redact(_ text: String, _ secrets: [String]) -> String {
    LLMTransport.redact(text, secrets: secrets)
  }
}

extension CodexCredentials {
  /// What must never appear in an error: both tokens and the account id.
  var secrets: [String] { [accessToken, refreshToken, accountID] }
}
