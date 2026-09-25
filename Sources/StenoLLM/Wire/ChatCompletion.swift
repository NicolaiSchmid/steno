import Foundation
import StenoCore

// The OpenAI chat completions wire format, the one shape every supported
// server speaks: `POST {baseURL}/chat/completions` and `GET {baseURL}/models`.
// Keys are spelled as the API spells them; nothing here is stored.

/// The request body of `POST /chat/completions`.
public struct ChatCompletionRequest: Codable, Sendable, Equatable {
  public var model: String
  public var messages: [ChatMessage]
  public var temperature: Double?
  public var maxTokens: Int?
  public var responseFormat: ChatResponseFormat?

  public init(
    model: String,
    messages: [ChatMessage],
    temperature: Double? = nil,
    maxTokens: Int? = nil,
    responseFormat: ChatResponseFormat? = nil
  ) {
    self.model = model
    self.messages = messages
    self.temperature = temperature
    self.maxTokens = maxTokens
    self.responseFormat = responseFormat
  }

  enum CodingKeys: String, CodingKey {
    case model
    case messages
    case temperature
    case maxTokens = "max_tokens"
    case responseFormat = "response_format"
  }
}

public struct ChatMessage: Codable, Sendable, Equatable {
  public var role: String
  public var content: String

  public init(role: String, content: String) {
    self.role = role
    self.content = content
  }

  public init(_ message: LLMMessage) {
    self.role = message.role.rawValue
    self.content = message.content
  }
}

/// `response_format`: `{"type": "json_object"}` or
/// `{"type": "json_schema", "json_schema": {"name", "schema", "strict"}}`.
public struct ChatResponseFormat: Codable, Sendable, Equatable {
  public struct Schema: Codable, Sendable, Equatable {
    public var name: String
    public var schema: JSONValue
    public var strict: Bool

    public init(name: String, schema: JSONValue, strict: Bool) {
      self.name = name
      self.schema = schema
      self.strict = strict
    }
  }

  public var type: String
  public var jsonSchema: Schema?

  public init(type: String, jsonSchema: Schema? = nil) {
    self.type = type
    self.jsonSchema = jsonSchema
  }

  public static let jsonObject = ChatResponseFormat(type: "json_object")

  public static func jsonSchema(name: String, schema: JSONValue, strict: Bool)
    -> ChatResponseFormat
  {
    ChatResponseFormat(
      type: "json_schema", jsonSchema: Schema(name: name, schema: schema, strict: strict))
  }

  enum CodingKeys: String, CodingKey {
    case type
    case jsonSchema = "json_schema"
  }
}

/// The response body of `POST /chat/completions`. Only the fields Steno reads.
public struct ChatCompletionResponse: Codable, Sendable, Equatable {
  public struct Choice: Codable, Sendable, Equatable {
    public var index: Int?
    public var message: Message
    public var finishReason: String?

    public init(index: Int? = nil, message: Message, finishReason: String?) {
      self.index = index
      self.message = message
      self.finishReason = finishReason
    }

    enum CodingKeys: String, CodingKey {
      case index
      case message
      case finishReason = "finish_reason"
    }
  }

  /// `content` is a string on every server Steno targets; a few return an
  /// array of `{"type": "text", "text": ...}` parts, which decode to their
  /// concatenated text. `refusal` is OpenAI's structured-output refusal.
  public struct Message: Codable, Sendable, Equatable {
    public var role: String?
    public var content: String?
    public var refusal: String?

    public init(role: String? = "assistant", content: String?, refusal: String? = nil) {
      self.role = role
      self.content = content
      self.refusal = refusal
    }

    private struct Part: Decodable {
      var type: String?
      var text: String?
    }

    public init(from decoder: any Decoder) throws {
      let container = try decoder.container(keyedBy: CodingKeys.self)
      role = try container.decodeIfPresent(String.self, forKey: .role)
      refusal = try container.decodeIfPresent(String.self, forKey: .refusal)
      if let text = try? container.decodeIfPresent(String.self, forKey: .content) {
        content = text
      } else if let parts = try? container.decodeIfPresent([Part].self, forKey: .content) {
        content = parts.compactMap(\.text).joined()
      } else {
        content = nil
      }
    }

    enum CodingKeys: String, CodingKey {
      case role
      case content
      case refusal
    }
  }

  public struct Usage: Codable, Sendable, Equatable {
    public var promptTokens: Int
    public var completionTokens: Int
    public var totalTokens: Int?

    public init(promptTokens: Int, completionTokens: Int, totalTokens: Int? = nil) {
      self.promptTokens = promptTokens
      self.completionTokens = completionTokens
      self.totalTokens = totalTokens
    }

    enum CodingKeys: String, CodingKey {
      case promptTokens = "prompt_tokens"
      case completionTokens = "completion_tokens"
      case totalTokens = "total_tokens"
    }
  }

  public var id: String?
  public var model: String?
  public var choices: [Choice]
  public var usage: Usage?

  public init(id: String? = nil, model: String? = nil, choices: [Choice], usage: Usage? = nil) {
    self.id = id
    self.model = model
    self.choices = choices
    self.usage = usage
  }
}

/// `{"error": {"message": ..., "type": ..., "code": ...}}`; `code` is a string
/// on OpenAI and an integer on some servers, so it is read as any JSON value.
public struct ChatErrorEnvelope: Codable, Sendable, Equatable {
  public struct Detail: Codable, Sendable, Equatable {
    public var message: String
    public var type: String?
    public var code: JSONValue?

    public init(message: String, type: String? = nil, code: JSONValue? = nil) {
      self.message = message
      self.type = type
      self.code = code
    }
  }

  public var error: Detail

  public init(error: Detail) {
    self.error = error
  }
}

/// The response body of `GET /models`.
public struct ModelList: Codable, Sendable, Equatable {
  public struct Model: Codable, Sendable, Equatable {
    public var id: String

    public init(id: String) {
      self.id = id
    }
  }

  public var data: [Model]

  public init(data: [Model]) {
    self.data = data
  }
}

/// One JSON coder pair for the wire: no key sorting requirements, but sorted
/// output keeps recorded request bodies byte-stable in tests.
enum WireJSON {
  static func encode<T: Encodable>(_ value: T) throws -> Data {
    let encoder = JSONEncoder()
    encoder.outputFormatting = [.sortedKeys, .withoutEscapingSlashes]
    return try encoder.encode(value)
  }

  static func decode<T: Decodable>(_ type: T.Type, from data: Data) throws -> T {
    try JSONDecoder().decode(type, from: data)
  }
}
