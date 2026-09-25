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
    var headers: [String: String] = [:]
    if let retryAfterSeconds { headers["Retry-After"] = "\(retryAfterSeconds)" }
    return .json(
      ChatErrorEnvelope(error: .init(message: "Rate limit reached", type: "rate_limit_error")),
      status: 429, headers: headers)
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
