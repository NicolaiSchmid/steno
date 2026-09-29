import Foundation
import StenoCore

/// How the client asks for JSON. It starts at the endpoint's mode and falls
/// back per endpoint on a 400 that names `response_format`: `.jsonSchema` to
/// `.jsonObject` to `.promptOnly` (the schema is always in the prompt as
/// well, so every mode yields decodable output on a capable model).
public enum StructuredOutputMode: String, Sendable, Codable, Equatable, CaseIterable {
  case jsonSchema
  case jsonObject
  case promptOnly

  /// The next weaker mode; nil from `.promptOnly`.
  public var downgraded: StructuredOutputMode? {
    switch self {
    case .jsonSchema: .jsonObject
    case .jsonObject: .promptOnly
    case .promptOnly: nil
    }
  }
}

/// Where the model lives and how much it can hold. Deliberately not
/// `Codable`: the API key is never part of a value type, it goes into
/// `OpenAICompatibleClient.init` alone.
public struct LLMEndpoint: Sendable, Equatable {
  /// The OpenAI-compatible root, `http://127.0.0.1:1234/v1` for LM Studio.
  public var baseURL: URL
  public var model: String
  /// The model's context window; input budgets derive from it.
  public var contextTokens: Int
  /// The most tokens one completion may produce.
  public var maxOutputTokens: Int
  /// Cleanup chunks in flight at once.
  public var maxConcurrentRequests: Int
  /// Per attempt, on the injected clock.
  public var requestTimeout: Duration
  public var structuredOutputMode: StructuredOutputMode

  public init(
    baseURL: URL,
    model: String,
    contextTokens: Int = 32_000,
    maxOutputTokens: Int = 4_096,
    maxConcurrentRequests: Int = 2,
    requestTimeout: Duration = .seconds(240),
    structuredOutputMode: StructuredOutputMode = .jsonSchema
  ) {
    self.baseURL = baseURL
    self.model = model
    self.contextTokens = contextTokens
    self.maxOutputTokens = maxOutputTokens
    self.maxConcurrentRequests = maxConcurrentRequests
    self.requestTimeout = requestTimeout
    self.structuredOutputMode = structuredOutputMode
  }

  /// The endpoint the settings describe, or nil while summaries are off.
  /// For `.endpoint`: `llmBaseURL`, `llmModel` and `llmContextTokens`, nil
  /// until both the URL and the model are set. For `.codex`: nil until a
  /// model is picked and the user has confirmed the credential use
  /// (`codexConfirmedAt`), so every "configured" check stays one call.
  public init?(settings: Settings) {
    switch settings.llmProvider {
    case .endpoint:
      guard let baseURL = settings.llmBaseURL, let model = settings.llmModel, !model.isEmpty
      else { return nil }
      self.init(
        baseURL: baseURL, model: model, contextTokens: max(settings.llmContextTokens, 1_024))
    case .codex:
      guard settings.codexConfirmedAt != nil, let model = settings.codexModel, !model.isEmpty
      else { return nil }
      self = .codex(model: model, contextTokens: settings.codexContextTokens)
    }
  }

  /// OpenAI's Codex backend, the root the Codex CLI talks to with a ChatGPT
  /// sign-in. `CodexResponsesClient` appends `/responses` and `/models`.
  public static let codexBackendURL = URL(string: "https://chatgpt.com/backend-api/codex")!

  /// Whether this endpoint is the Codex backend (Responses API, Codex
  /// credentials) rather than a chat completions server.
  public var isCodexBackend: Bool { baseURL == Self.codexBackendURL }

  /// The Codex backend with `model`. The backend takes no output ceiling, so
  /// `maxOutputTokens` only shapes prompts and budgets; 16k leaves the
  /// summary room without starving the input.
  public static func codex(model: String, contextTokens: Int) -> LLMEndpoint {
    LLMEndpoint(
      baseURL: codexBackendURL, model: model, contextTokens: max(contextTokens, 1_024),
      maxOutputTokens: 16_000)
  }

  public var chatCompletionsURL: URL { baseURL.appendingPathComponent("chat/completions") }
  public var responsesURL: URL { baseURL.appendingPathComponent("responses") }
  public var modelsURL: URL { baseURL.appendingPathComponent("models") }
}

/// What a client's `probe()` learned about an endpoint whose probe
/// completion succeeded; a probe that could not complete throws.
public struct EndpointProbe: Sendable, Equatable {
  /// Whether `GET /models` lists the configured model; nil when the server
  /// has no model list.
  public var modelListed: Bool?
  public var resolvedMode: StructuredOutputMode
  public var roundTrip: Duration
  /// "name@example.com (Plus)" from the Codex sign-in; nil for an endpoint.
  public var accountLine: String? = nil
}

/// A `LanguageModel` bound to one `LLMEndpoint` that can check its own
/// setup: the two clients, so the wiring that picks one by provider needs
/// no cast to probe it.
public protocol LLMClient: LanguageModel {
  var endpoint: LLMEndpoint { get }
  func probe() async throws -> EndpointProbe
}
