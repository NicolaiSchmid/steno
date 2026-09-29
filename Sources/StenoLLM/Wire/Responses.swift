import Foundation
import StenoCore

// The OpenAI Responses API as the Codex backend speaks it:
// `POST {codexBackendURL}/responses`, always streamed, and
// `GET {codexBackendURL}/models`. Keys are spelled as the API spells them.
// Only the fields Steno sends or reads; the backend rejects `temperature`
// and `max_output_tokens` by name, so neither has a place here.

/// The request body of `POST /responses`.
struct ResponsesRequest: Codable, Sendable, Equatable {
  struct Reasoning: Codable, Sendable, Equatable {
    var effort: String
  }

  struct Text: Codable, Sendable, Equatable {
    var format: Format
  }

  /// `{"type": "json_object"}` or
  /// `{"type": "json_schema", "name", "schema", "strict"}` (flat, unlike
  /// chat completions' nested `json_schema`).
  struct Format: Codable, Sendable, Equatable {
    var type: String
    var name: String?
    var schema: JSONValue?
    var strict: Bool?

    static let jsonObject = Format(type: "json_object")

    static func jsonSchema(name: String, schema: JSONValue, strict: Bool) -> Format {
      Format(type: "json_schema", name: name, schema: schema, strict: strict)
    }
  }

  var model: String
  /// The system prompt; the Responses API takes it apart from the input.
  var instructions: String?
  var input: [ResponsesInputItem]
  var toolChoice: String = "auto"
  var parallelToolCalls: Bool = false
  var reasoning: Reasoning?
  var store: Bool = false
  var stream: Bool = true
  var include: [String] = []
  var text: Text?

  enum CodingKeys: String, CodingKey {
    case model
    case instructions
    case input
    case toolChoice = "tool_choice"
    case parallelToolCalls = "parallel_tool_calls"
    case reasoning
    case store
    case stream
    case include
    case text
  }
}

/// One `message` input item: a role and its text parts. User text goes as
/// `input_text`; an earlier assistant answer (the repair round) as
/// `output_text`, which is how the API names assistant-authored content.
struct ResponsesInputItem: Codable, Sendable, Equatable {
  struct Part: Codable, Sendable, Equatable {
    var type: String
    var text: String
  }

  var type: String = "message"
  var role: String
  var content: [Part]

  init(_ message: LLMMessage) {
    role = message.role.rawValue
    content = [
      Part(type: message.role == .assistant ? "output_text" : "input_text", text: message.content)
    ]
  }
}

/// A `response` object as the stream's `response.*` events carry it. Only
/// the fields Steno reads; `output` is used when no item event was seen.
struct ResponsesResponse: Codable, Sendable, Equatable {
  struct IncompleteDetails: Codable, Sendable, Equatable {
    var reason: String?
  }

  struct ResponseError: Codable, Sendable, Equatable {
    var code: String?
    var message: String?
  }

  /// `input_tokens` and `output_tokens`; both optional so a missing usage
  /// never fails a good answer.
  struct Usage: Codable, Sendable, Equatable {
    var inputTokens: Int?
    var outputTokens: Int?

    enum CodingKeys: String, CodingKey {
      case inputTokens = "input_tokens"
      case outputTokens = "output_tokens"
    }
  }

  var id: String?
  var status: String?
  var model: String?
  var output: [ResponsesOutputItem]?
  var usage: Usage?
  var incompleteDetails: IncompleteDetails?
  var error: ResponseError?

  enum CodingKeys: String, CodingKey {
    case id
    case status
    case model
    case output
    case usage
    case incompleteDetails = "incomplete_details"
    case error
  }
}

/// One output item. Steno reads `message` items' `output_text` parts and
/// treats a `refusal` part as the model declining; reasoning and tool items
/// are skipped.
struct ResponsesOutputItem: Codable, Sendable, Equatable {
  struct Part: Codable, Sendable, Equatable {
    var type: String
    var text: String?
    var refusal: String?
  }

  var type: String
  var id: String?
  var role: String?
  var content: [Part]?

  var text: String {
    (content ?? []).filter { $0.type == "output_text" }.compactMap(\.text).joined()
  }

  var refusal: String? {
    (content ?? []).first { $0.type == "refusal" }?.refusal
  }
}

/// One event of the stream, decoded from its `data:` JSON. `type` matches
/// the SSE `event:` name (`response.completed`, `response.output_item.done`,
/// `error`, …).
struct ResponsesStreamEvent: Codable, Sendable, Equatable {
  var type: String
  var response: ResponsesResponse?
  var item: ResponsesOutputItem?
  /// The top-level `error` event's payload.
  var code: String?
  var message: String?
}

/// The Codex backend's error bodies come in two shapes:
/// `{"detail": "Unsupported parameter: …"}` and OpenAI's
/// `{"error": {"message", "type", "code"}}`.
struct CodexErrorEnvelope: Codable, Sendable, Equatable {
  struct Detail: Codable, Sendable, Equatable {
    var message: String?
    var type: String?
    var code: JSONValue?
    var param: String?
  }

  var detail: JSONValue?
  var error: Detail?

  /// The most specific human sentence in the body.
  var message: String? {
    if let text = error?.message, !text.isEmpty { return text }
    switch detail {
    case .string(let text)?: return text
    case .object(let object)?:
      if case .string(let text)? = object["message"] { return text }
      return nil
    default: return nil
    }
  }

  /// `error.type` or `error.code`, lowercased, for the plan-limit check.
  var kind: String? {
    if let type = error?.type, !type.isEmpty { return type.lowercased() }
    if case .string(let code)? = error?.code, !code.isEmpty { return code.lowercased() }
    return nil
  }
}

/// One entry of `GET {codexBackendURL}/models`. `visibility` is "list" for
/// models the picker should show; the rest are hidden or retired.
public struct CodexModel: Codable, Sendable, Equatable, Hashable, Identifiable {
  public var slug: String
  public var displayName: String
  public var visibility: String?
  public var contextWindow: Int?
  public var supportedInAPI: Bool?

  public var id: String { slug }
  public var isListed: Bool { (visibility ?? "list") == "list" }

  public init(
    slug: String, displayName: String, visibility: String? = "list", contextWindow: Int? = nil,
    supportedInAPI: Bool? = nil
  ) {
    self.slug = slug
    self.displayName = displayName
    self.visibility = visibility
    self.contextWindow = contextWindow
    self.supportedInAPI = supportedInAPI
  }

  enum CodingKeys: String, CodingKey {
    case slug
    case displayName = "display_name"
    case visibility
    case contextWindow = "context_window"
    case supportedInAPI = "supported_in_api"
  }
}

struct CodexModelList: Codable, Sendable, Equatable {
  var models: [CodexModel]
}
