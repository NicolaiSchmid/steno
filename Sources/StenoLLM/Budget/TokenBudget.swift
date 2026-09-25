import Foundation
import StenoCore

/// Tokenizer-free budgeting. English prose runs about 4 bytes per token on
/// current tokenizers, German 2.8 to 3.3; Steno divides UTF-8 bytes by 3.6
/// for English and by 3.0 for everything else (German, mixed, unknown), a
/// deliberate 10 to 30 percent overestimate. Real counts come back in the
/// server's `usage`.
public struct TokenBudget: Sendable, Equatable {
  public var contextTokens: Int
  /// Tokens kept free for the model's answer.
  public var reservedOutputTokens: Int
  /// Tokens the system prompt and message framing take.
  public var promptOverheadTokens: Int

  public init(contextTokens: Int, reservedOutputTokens: Int, promptOverheadTokens: Int) {
    self.contextTokens = contextTokens
    self.reservedOutputTokens = reservedOutputTokens
    self.promptOverheadTokens = promptOverheadTokens
  }

  /// What is left for the transcript or notes; never negative.
  public var inputBudget: Int {
    max(0, contextTokens - reservedOutputTokens - promptOverheadTokens)
  }

  public func fits(_ tokens: Int) -> Bool {
    tokens <= inputBudget
  }

  public static func bytesPerToken(_ language: LanguageTag?) -> Double {
    language?.primarySubtag == "en" ? 3.6 : 3.0
  }

  public static func estimateTokens(_ text: String, language: LanguageTag?) -> Int {
    let bytes = text.utf8.count
    guard bytes > 0 else { return 0 }
    return Int((Double(bytes) / bytesPerToken(language)).rounded(.up))
  }
}

extension LanguageTag {
  /// The language subtag alone, lowercased: `de` for `de-CH`.
  public var primarySubtag: String {
    rawValue.split(separator: "-").first.map { $0.lowercased() } ?? rawValue.lowercased()
  }
}

extension LLMResponse {
  /// What one call cost; a body without `usage` still counts as a request,
  /// so per-meeting totals stay honest about the number of calls.
  var countedUsage: LLMUsage {
    usage ?? LLMUsage(promptTokens: 0, completionTokens: 0, requests: 1)
  }
}
