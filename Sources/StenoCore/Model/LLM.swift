import Foundation

/// Token accounting summed over every LLM call of a meeting.
public struct LLMUsage: Codable, Sendable, Equatable, Hashable {
  public var promptTokens: Int
  public var completionTokens: Int
  public var requests: Int

  public init(promptTokens: Int, completionTokens: Int, requests: Int) {
    self.promptTokens = promptTokens
    self.completionTokens = completionTokens
    self.requests = requests
  }

  public static let zero = LLMUsage(promptTokens: 0, completionTokens: 0, requests: 0)

  public static func + (lhs: LLMUsage, rhs: LLMUsage) -> LLMUsage {
    LLMUsage(
      promptTokens: lhs.promptTokens + rhs.promptTokens,
      completionTokens: lhs.completionTokens + rhs.completionTokens,
      requests: lhs.requests + rhs.requests
    )
  }
}

public enum LLMRole: String, Codable, Sendable, Equatable, Hashable {
  case system
  case user
  case assistant
}

public struct LLMMessage: Codable, Sendable, Equatable, Hashable {
  public var role: LLMRole
  public var content: String

  public init(role: LLMRole, content: String) {
    self.role = role
    self.content = content
  }
}

public enum LLMResponseFormat: Codable, Sendable, Equatable, Hashable {
  case text
  case jsonObject
  case jsonSchema(name: String, schema: JSONValue, strict: Bool)

  public enum Kind: String, CaseIterable, Codable, Sendable {
    case text, jsonObject, jsonSchema
  }

  public var kind: Kind {
    switch self {
    case .text: .text
    case .jsonObject: .jsonObject
    case .jsonSchema: .jsonSchema
    }
  }

  private struct Schema: Codable {
    var name: String
    var schema: JSONValue
    var strict: Bool
  }

  public init(from decoder: any Decoder) throws {
    let (kind, payload) = try CaseCoding.decode(Kind.self, from: decoder)
    switch kind {
    case .text: self = .text
    case .jsonObject: self = .jsonObject
    case .jsonSchema:
      let schema = try CaseCoding.decodePayload(Schema.self, from: payload, case: kind)
      self = .jsonSchema(name: schema.name, schema: schema.schema, strict: schema.strict)
    }
  }

  public func encode(to encoder: any Encoder) throws {
    switch self {
    case .jsonSchema(let name, let schema, let strict):
      try CaseCoding.encode(
        kind, payload: Schema(name: name, schema: schema, strict: strict), to: encoder)
    default: try CaseCoding.encode(kind, to: encoder)
    }
  }
}

/// One completion request to an OpenAI-compatible endpoint. `purpose` names
/// the pass ("cleanup", "summary") for logs and usage accounting.
public struct LLMRequest: Codable, Sendable, Equatable, Hashable {
  public var messages: [LLMMessage]
  public var responseFormat: LLMResponseFormat
  public var temperature: Double?
  public var maxTokens: Int?
  public var purpose: String

  public init(
    messages: [LLMMessage],
    responseFormat: LLMResponseFormat = .text,
    temperature: Double? = nil,
    maxTokens: Int? = nil,
    purpose: String
  ) {
    self.messages = messages
    self.responseFormat = responseFormat
    self.temperature = temperature
    self.maxTokens = maxTokens
    self.purpose = purpose
  }
}

public enum LLMFinishReason: String, Codable, Sendable, Equatable, Hashable {
  case stop
  case length
  case contentFilter
  case other
}

public struct LLMResponse: Codable, Sendable, Equatable, Hashable {
  public var text: String
  public var finishReason: LLMFinishReason
  public var usage: LLMUsage?
  public var model: String?

  public init(
    text: String, finishReason: LLMFinishReason, usage: LLMUsage? = nil, model: String? = nil
  ) {
    self.text = text
    self.finishReason = finishReason
    self.usage = usage
    self.model = model
  }
}
