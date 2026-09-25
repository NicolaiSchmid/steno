import Foundation
import StenoCore

/// Turns a completion into a `Decodable` value: `finish_reason: length`
/// is `LLMError.truncated`, a Markdown fence or prose around the JSON is
/// stripped, then `JSONDecoder` decides. No repair heuristics: a failure
/// is `LLMError.invalidJSON` and the caller's one repair round takes over.
public enum StructuredOutputDecoder {
  public static func decode<T: Decodable>(_ type: T.Type, from response: LLMResponse) throws -> T {
    if response.finishReason == .length { throw LLMError.truncated }
    let json = extractJSON(response.text)
    guard !json.isEmpty else { throw LLMError.invalidJSON("empty answer") }
    do {
      return try JSONDecoder().decode(T.self, from: Data(json.utf8))
    } catch let error as DecodingError {
      throw LLMError.invalidJSON(describe(error))
    } catch {
      throw LLMError.invalidJSON(String(describing: error))
    }
  }

  /// The JSON inside `text`: the content of the first ``` fence when there
  /// is one, else everything from the first `{` or `[` to the last matching
  /// `}` or `]`; whitespace trimmed.
  public static func extractJSON(_ text: String) -> String {
    var body = Substring(text)
    if let fence = body.range(of: "```") {
      var afterFence = body[fence.upperBound...]
      // Skip a language tag such as `json` up to the end of the line.
      if let newline = afterFence.firstIndex(of: "\n") {
        let tag = afterFence[..<newline]
        if tag.allSatisfy({ $0.isLetter || $0.isNumber }) {
          afterFence = afterFence[afterFence.index(after: newline)...]
        }
      }
      if let closing = afterFence.range(of: "```") {
        body = afterFence[..<closing.lowerBound]
      } else {
        body = afterFence
      }
    }
    guard let start = body.firstIndex(where: { $0 == "{" || $0 == "[" }) else {
      return body.trimmingCharacters(in: .whitespacesAndNewlines)
    }
    let opener = body[start]
    let closer: Character = opener == "{" ? "}" : "]"
    guard let end = body.lastIndex(of: closer), end > start else {
      return String(body[start...]).trimmingCharacters(in: .whitespacesAndNewlines)
    }
    return String(body[start...end])
  }

  static func describe(_ error: DecodingError) -> String {
    func path(_ context: DecodingError.Context) -> String {
      let keys = context.codingPath.map { key in
        key.intValue.map { "[\($0)]" } ?? key.stringValue
      }
      return keys.isEmpty ? "root" : keys.joined(separator: ".")
    }
    switch error {
    case .keyNotFound(let key, let context):
      return "missing key \(key.stringValue) at \(path(context))"
    case .typeMismatch(let type, let context):
      return "expected \(type) at \(path(context)): \(context.debugDescription)"
    case .valueNotFound(let type, let context):
      return "missing \(type) at \(path(context))"
    case .dataCorrupted(let context):
      return "malformed JSON at \(path(context)): \(context.debugDescription)"
    @unknown default:
      return String(describing: error)
    }
  }
}
