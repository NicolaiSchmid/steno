import Foundation
import StenoCore

/// Ready-made `StubResponse`s for the situations the client must survive:
/// a plain completion, a rate limit with `Retry-After`, a server error, a
/// 400 rejecting `response_format`, fenced or invalid JSON, a truncated
/// answer, a refusal and a model list. `StubResponse.hang` and `.drop`
/// cover a connection that never answers or closes early.
public enum Scripts {
  /// A successful completion whose message content is `text`.
  public static func completion(
    _ text: String,
    finishReason: String? = "stop",
    usage: LLMUsage? = LLMUsage(promptTokens: 10, completionTokens: 5, requests: 1),
    model: String = "stub-model"
  ) -> StubResponse {
    .json(
      ChatCompletionResponse(
        id: "chatcmpl-stub", model: model,
        choices: [
          .init(
            index: 0, message: .init(content: text), finishReason: finishReason)
        ],
        usage: usage.map {
          .init(
            promptTokens: $0.promptTokens, completionTokens: $0.completionTokens,
            totalTokens: $0.promptTokens + $0.completionTokens)
        }))
  }

  /// A completion whose content is `value` encoded as JSON.
  public static func json<T: Encodable>(
    _ value: T, usage: LLMUsage? = LLMUsage(promptTokens: 10, completionTokens: 5, requests: 1)
  ) -> StubResponse {
    completion(encoded(value), usage: usage)
  }

  /// `value` as JSON inside a Markdown fence with a prose prefix.
  public static func fenced<T: Encodable>(_ value: T) -> StubResponse {
    completion("Here is the JSON you asked for:\n```json\n\(encoded(value))\n```\n")
  }

  private static func encoded<T: Encodable>(_ value: T) -> String {
    String(decoding: (try? WireJSON.encode(value)) ?? Data("{}".utf8), as: UTF8.self)
  }

  /// A completion cut off by the server's token limit.
  public static func truncated(_ partialText: String) -> StubResponse {
    completion(partialText, finishReason: "length")
  }

  /// OpenAI's structured-output refusal: no content, a `refusal` string.
  public static func refusal(_ reason: String) -> StubResponse {
    .json(
      ChatCompletionResponse(
        model: "stub-model",
        choices: [.init(message: .init(content: nil, refusal: reason), finishReason: "stop")]))
  }

  public static func rateLimited(retryAfterSeconds: Int? = nil) -> StubResponse {
    rateLimited(retryAfter: retryAfterSeconds.map { "\($0)" })
  }

  /// A 429 whose `Retry-After` header is `retryAfter` verbatim, for the
  /// values a client must survive (`inf`, `1e300`, an HTTP date).
  public static func rateLimited(retryAfter header: String?) -> StubResponse {
    var headers: [String: String] = [:]
    if let header { headers["Retry-After"] = header }
    return .json(
      ChatErrorEnvelope(error: .init(message: "Rate limit reached", type: "rate_limit_error")),
      status: 429, headers: headers)
  }

  /// A completion body written verbatim, for shapes the wire types cannot
  /// produce (a null or partial `usage`).
  public static func rawCompletion(_ body: String) -> StubResponse {
    StubResponse(
      status: 200, headers: ["Content-Type": "application/json"], body: Data(body.utf8))
  }

  public static func serverError(_ status: Int = 500) -> StubResponse {
    .json(ChatErrorEnvelope(error: .init(message: "The server had an error")), status: status)
  }

  public static func unauthorized() -> StubResponse {
    .json(
      ChatErrorEnvelope(
        error: .init(message: "Incorrect API key provided", type: "invalid_request_error")),
      status: 401)
  }

  /// The 400 Groq and OpenRouter send when a model lacks structured output.
  public static func rejectsResponseFormat() -> StubResponse {
    .json(
      ChatErrorEnvelope(
        error: .init(
          message: "'response_format' of type 'json_schema' is not supported with this model",
          type: "invalid_request_error", code: "response_format_unsupported")),
      status: 400)
  }

  public static func badRequest(_ message: String) -> StubResponse {
    .json(ChatErrorEnvelope(error: .init(message: message)), status: 400)
  }

  /// OpenAI's 400 for a request field a model does not take, the field named
  /// in `error.param`: `max_tokens` and `temperature` on reasoning models.
  public static func rejectsParameter(_ param: String) -> StubResponse {
    .json(
      ChatErrorEnvelope(
        error: .init(
          message:
            "Unsupported parameter: '\(param)' is not supported with this model.",
          type: "invalid_request_error", param: param, code: "unsupported_parameter")),
      status: 400)
  }

  /// A responder that plays an OpenAI reasoning model: it rejects
  /// `max_tokens` (wanting `max_completion_tokens`) and any `temperature`,
  /// each by name, and otherwise returns `completion`.
  public static func reasoningModel(completion: StubResponse)
    -> @Sendable (RecordedRequest) ->
    StubResponse?
  {
    { request in
      guard let chat = request.chat else { return completion }
      if chat.maxTokens != nil { return rejectsParameter("max_tokens") }
      if chat.temperature != nil { return rejectsParameter("temperature") }
      return completion
    }
  }

  public static func models(_ ids: [String]) -> StubResponse {
    .json(ModelList(data: ids.map { .init(id: $0) }))
  }

  /// A responder that plays a perfect cleanup model: it reads the numbered
  /// segments out of the request and answers with them, each passed through
  /// `transform(index, text)`. Segments the transform returns nil for are
  /// dropped from the answer, which lets a test script a wrong count.
  public static func cleanupEcho(
    usage: LLMUsage = LLMUsage(promptTokens: 100, completionTokens: 50, requests: 1),
    transform: @escaping @Sendable (Int, String) -> String? = { $1 }
  ) -> @Sendable (RecordedRequest) -> StubResponse? {
    { request in
      guard let user = request.chat?.messages.last(where: { $0.role == "user" }) else {
        return nil
      }
      let draft = CleanupDraft(
        segments: parseSegments(user.content).compactMap { index, text in
          transform(index, text).map { CleanupDraft.Segment(index: index, text: $0) }
        })
      return json(draft, usage: usage)
    }
  }

  /// `[n] Label: text` lines after the "Segments to correct" marker.
  public static func parseSegments(_ userMessage: String) -> [(index: Int, text: String)] {
    var segments: [(Int, String)] = []
    var started = false
    for line in userMessage.split(separator: "\n", omittingEmptySubsequences: false) {
      if line.hasPrefix("Segments to correct") {
        started = true
        continue
      }
      guard started, line.hasPrefix("["), let close = line.firstIndex(of: "]"),
        let index = Int(line[line.index(after: line.startIndex)..<close])
      else { continue }
      let rest = line[line.index(after: close)...]
      guard let colon = rest.firstIndex(of: ":") else { continue }
      let text = rest[rest.index(after: colon)...].trimmingCharacters(in: .whitespaces)
      segments.append((index, text))
    }
    return segments
  }

  /// A responder that answers `GET /models` with `models`, rejects
  /// `response_format` kinds in `rejecting` with a 400, and otherwise returns
  /// `completion`. Models the servers that honour `json_object` but not
  /// `json_schema`.
  public static func server(
    models: [String] = ["stub-model"],
    rejecting: Set<String> = [],
    completion: StubResponse
  ) -> @Sendable (RecordedRequest) -> StubResponse? {
    { request in
      if request.method == "GET", request.path.hasSuffix("/models") {
        return Scripts.models(models)
      }
      if let format = request.chat?.responseFormat?.type, rejecting.contains(format) {
        return rejectsResponseFormat()
      }
      return completion
    }
  }
}

// MARK: - Codex backend

extension Scripts {
  /// One `text/event-stream` body as the Codex backend streams a finished
  /// response: created, one message item done, then `response.completed`
  /// (or `response.incomplete` with `incompleteReason`).
  public static func responsesStream(
    _ text: String,
    status: String = "completed",
    incompleteReason: String? = nil,
    usage: LLMUsage? = LLMUsage(promptTokens: 10, completionTokens: 5, requests: 1),
    model: String = "gpt-stub"
  ) -> StubResponse {
    let item = ResponsesOutputItem(
      type: "message", id: "msg_stub", role: "assistant",
      content: [.init(type: "output_text", text: text)])
    let response = ResponsesResponse(
      id: "resp_stub", status: status, model: model, output: [item],
      usage: usage.map { .init(inputTokens: $0.promptTokens, outputTokens: $0.completionTokens) },
      incompleteDetails: incompleteReason.map { .init(reason: $0) })
    let terminal = status == "completed" ? "response.completed" : "response.incomplete"
    return eventStream([
      ("response.created", ResponsesStreamEvent(type: "response.created", response: response)),
      (
        "response.output_item.done",
        ResponsesStreamEvent(type: "response.output_item.done", item: item)
      ),
      (terminal, ResponsesStreamEvent(type: terminal, response: response)),
    ])
  }

  /// A stream that ends in `response.failed` with `message`.
  public static func responsesFailed(_ message: String) -> StubResponse {
    let response = ResponsesResponse(
      id: "resp_stub", status: "failed", error: .init(code: "server_error", message: message))
    return eventStream([
      ("response.created", ResponsesStreamEvent(type: "response.created", response: response)),
      ("response.failed", ResponsesStreamEvent(type: "response.failed", response: response)),
    ])
  }

  /// A stream with a `refusal` part instead of text.
  public static func responsesRefusal(_ reason: String) -> StubResponse {
    let item = ResponsesOutputItem(
      type: "message", id: "msg_stub", role: "assistant",
      content: [.init(type: "refusal", refusal: reason)])
    let response = ResponsesResponse(id: "resp_stub", status: "completed", output: [item])
    return eventStream([
      (
        "response.output_item.done",
        ResponsesStreamEvent(type: "response.output_item.done", item: item)
      ),
      ("response.completed", ResponsesStreamEvent(type: "response.completed", response: response)),
    ])
  }

  /// A stream that ends in a top-level `error` event with `message`, the
  /// shape the backend uses for a failure that has no response object.
  public static func responsesErrorEvent(_ message: String, code: String = "server_error")
    -> StubResponse
  {
    eventStream([
      (
        "response.created",
        ResponsesStreamEvent(
          type: "response.created",
          response: ResponsesResponse(id: "resp_stub", status: "in_progress"))
      ),
      ("error", ResponsesStreamEvent(type: "error", code: code, message: message)),
    ])
  }

  /// A `text/event-stream` body written verbatim, for shapes the typed
  /// events cannot produce (data-only events, `[DONE]`, unknown names).
  public static func rawEventStream(_ body: String) -> StubResponse {
    StubResponse(
      status: 200, headers: ["Content-Type": "text/event-stream"], body: Data(body.utf8))
  }

  /// A stream cut before its terminal event.
  public static func responsesTruncatedStream() -> StubResponse {
    eventStream([
      (
        "response.created",
        ResponsesStreamEvent(
          type: "response.created",
          response: ResponsesResponse(id: "resp_stub", status: "in_progress"))
      )
    ])
  }

  static func eventStream(_ events: [(String, ResponsesStreamEvent)]) -> StubResponse {
    var body = ""
    for (name, event) in events {
      let data = String(decoding: (try? WireJSON.encode(event)) ?? Data("{}".utf8), as: UTF8.self)
      body += "event: \(name)\ndata: \(data)\n\n"
    }
    return StubResponse(
      status: 200, headers: ["Content-Type": "text/event-stream"], body: Data(body.utf8))
  }

  /// The Codex backend's 400 for a request field it does not take.
  public static func codexUnsupportedParameter(_ name: String) -> StubResponse {
    StubResponse(
      status: 400, headers: ["Content-Type": "application/json"],
      body: Data(#"{"detail":"Unsupported parameter: \#(name)"}"#.utf8))
  }

  /// The plan's usage limit: a 429 whose `error.type` names it, which the
  /// client must not retry.
  public static func codexUsageLimit() -> StubResponse {
    .json(
      ChatErrorEnvelope(
        error: .init(
          message: "You have hit your usage limit.", type: "usage_limit_reached",
          code: "usage_limit_reached")),
      status: 429, headers: ["Retry-After": "3600"])
  }

  public static func codexModels(_ models: [CodexModel]) -> StubResponse {
    .json(CodexModelList(models: models))
  }

  /// A refreshed token triple from the OAuth token endpoint. Every field is
  /// optional there; nil for `refresh` plays an endpoint that did not rotate
  /// the refresh token.
  public static func tokenRefresh(access: String, refresh: String? = nil, id: String? = nil)
    -> StubResponse
  {
    var body: [String: String] = ["access_token": access]
    if let refresh { body["refresh_token"] = refresh }
    if let id { body["id_token"] = id }
    return .json(body)
  }

  /// The token endpoint refusing a refresh token for good.
  public static func tokenRefreshRejected(code: String, status: Int = 400) -> StubResponse {
    .json(
      [
        "error": "invalid_grant", "error_code": code,
        "error_description": "The refresh token is \(code).",
      ],
      status: status)
  }
}
