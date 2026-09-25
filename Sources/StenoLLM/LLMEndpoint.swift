import Foundation
import StenoCore

/// How the client asks for JSON. `.auto` starts at `.jsonSchema` and falls
/// back per endpoint on a 400 that names `response_format`: to
/// `.jsonObject`, then to `.promptOnly` (the schema is always in the prompt
/// as well, so every mode yields decodable output on a capable model).
public enum StructuredOutputMode: String, Sendable, Codable, Equatable, CaseIterable {
  case auto
  case jsonSchema
  case jsonObject
  case promptOnly

  /// The next weaker mode; nil from `.promptOnly`.
  public var downgraded: StructuredOutputMode? {
    switch self {
    case .auto, .jsonSchema: .jsonObject
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
    structuredOutputMode: StructuredOutputMode = .auto
  ) {
    self.baseURL = baseURL
    self.model = model
    self.contextTokens = contextTokens
    self.maxOutputTokens = maxOutputTokens
    self.maxConcurrentRequests = maxConcurrentRequests
    self.requestTimeout = requestTimeout
    self.structuredOutputMode = structuredOutputMode
  }

  /// `llmBaseURL`, `llmModel` and `llmContextTokens` from the settings;
  /// throws `LLMError.notConfigured` naming the missing one.
  public init(settings: Settings) throws {
    guard let baseURL = settings.llmBaseURL else {
      throw LLMError.notConfigured("llmBaseURL")
    }
    guard let model = settings.llmModel, !model.isEmpty else {
      throw LLMError.notConfigured("llmModel")
    }
    self.init(baseURL: baseURL, model: model, contextTokens: max(settings.llmContextTokens, 1_024))
  }

  /// True when settings name both a base URL and a model.
  public static func isConfigured(_ settings: Settings) -> Bool {
    settings.llmBaseURL != nil && !(settings.llmModel ?? "").isEmpty
  }

  public var chatCompletionsURL: URL { baseURL.appendingPathComponent("chat/completions") }
  public var modelsURL: URL { baseURL.appendingPathComponent("models") }
}

/// What `OpenAICompatibleClient.probe()` learned about an endpoint.
public struct EndpointProbe: Sendable, Equatable {
  /// Some HTTP answer came back from the server.
  public var reachable: Bool
  /// Whether `GET /models` lists the configured model; nil when the server
  /// has no model list.
  public var modelListed: Bool?
  public var resolvedMode: StructuredOutputMode
  public var roundTrip: Duration

  public init(
    reachable: Bool, modelListed: Bool?, resolvedMode: StructuredOutputMode, roundTrip: Duration
  ) {
    self.reachable = reachable
    self.modelListed = modelListed
    self.resolvedMode = resolvedMode
    self.roundTrip = roundTrip
  }
}
