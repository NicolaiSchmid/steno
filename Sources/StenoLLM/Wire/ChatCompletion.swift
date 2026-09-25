import Foundation
import StenoCore

// The OpenAI chat completions wire format, the one shape every supported
// server speaks: `POST {baseURL}/chat/completions` and `GET {baseURL}/models`.
// Keys are spelled as the API spells them; nothing here is stored and
// nothing leaves the module: callers see `LLMRequest` and `LLMResponse`.

/// The request body of `POST /chat/completions`. `maxTokens` is the field
/// every compatible server takes; `maxCompletionTokens` is OpenAI's
/// successor, sent instead once a server has rejected `max_tokens` by name.
struct ChatCompletionRequest: Codable, Sendable, Equatable {
  var model: String
  var messages: [ChatMessage]
  var temperature: Double?
  var maxTokens: Int?
  var maxCompletionTokens: Int?
  var responseFormat: ChatResponseFormat?

  enum CodingKeys: String, CodingKey {
    case model
    case messages
    case temperature
    case maxTokens = "max_tokens"
    case maxCompletionTokens = "max_completion_tokens"
    case responseFormat = "response_format"
  }
}

struct ChatMessage: Codable, Sendable, Equatable {
  var role: String
  var content: String
}

extension ChatMessage {
  init(_ message: LLMMessage) {
    self.init(role: message.role.rawValue, content: message.content)
  }
}

/// `response_format`: `{"type": "json_object"}` or
/// `{"type": "json_schema", "json_schema": {"name", "schema", "strict"}}`.
struct ChatResponseFormat: Codable, Sendable, Equatable {
  struct Schema: Codable, Sendable, Equatable {
    var name: String
    var schema: JSONValue
    var strict: Bool
  }

  var type: String
  var jsonSchema: Schema?

  static let jsonObject = ChatResponseFormat(type: "json_object")

  static func jsonSchema(name: String, schema: JSONValue, strict: Bool) -> ChatResponseFormat {
    ChatResponseFormat(
      type: "json_schema", jsonSchema: Schema(name: name, schema: schema, strict: strict))
  }

  enum CodingKeys: String, CodingKey {
    case type
    case jsonSchema = "json_schema"
  }
}

/// The response body of `POST /chat/completions`. Only the fields Steno reads.
struct ChatCompletionResponse: Codable, Sendable, Equatable {
  struct Choice: Codable, Sendable, Equatable {
    var index: Int?
    var message: Message
    var finishReason: String?

    enum CodingKeys: String, CodingKey {
      case index
      case message
      case finishReason = "finish_reason"
    }
  }

  /// `content` is a string on every server Steno targets; a few return an
  /// array of `{"type": "text", "text": ...}` parts, which decode to their
  /// concatenated text. `refusal` is OpenAI's structured-output refusal.
  struct Message: Codable, Sendable, Equatable {
    var role: String? = "assistant"
    var content: String?
    var refusal: String?

    enum CodingKeys: String, CodingKey {
      case role
      case content
      case refusal
    }
  }

  /// Every count is optional: proxies and some local servers send a partial
  /// or null `usage`, and a good answer must never fail on its bookkeeping.
  struct Usage: Codable, Sendable, Equatable {
    var promptTokens: Int?
    var completionTokens: Int?
    var totalTokens: Int?

    enum CodingKeys: String, CodingKey {
      case promptTokens = "prompt_tokens"
      case completionTokens = "completion_tokens"
      case totalTokens = "total_tokens"
    }
  }

  var id: String?
  var model: String?
  var choices: [Choice]
  var usage: Usage?
}

extension ChatCompletionResponse.Message {
  private struct Part: Decodable {
    var type: String?
    var text: String?
  }

  init(from decoder: any Decoder) throws {
    let container = try decoder.container(keyedBy: CodingKeys.self)
    role = try container.decodeIfPresent(String.self, forKey: .role)
    refusal = try container.decodeIfPresent(String.self, forKey: .refusal)
    if let text = try? container.decodeIfPresent(String.self, forKey: .content) {
      content = text
    } else if let parts = try? container.decodeIfPresent([Part].self, forKey: .content) {
      content = parts.compactMap(\.text).joined()
    }
  }
}

/// `{"error": {"message": ..., "type": ..., "param": ..., "code": ...}}`;
/// `code` is a string on OpenAI and an integer on some servers, so it is read
/// as any JSON value. `param` names the rejected request field on a 400.
struct ChatErrorEnvelope: Codable, Sendable, Equatable {
  struct Detail: Codable, Sendable, Equatable {
    var message: String
    var type: String?
    var param: String?
    var code: JSONValue?
  }

  var error: Detail
}

/// The response body of `GET /models`.
struct ModelList: Codable, Sendable, Equatable {
  struct Model: Codable, Sendable, Equatable {
    var id: String
  }

  var data: [Model]
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
