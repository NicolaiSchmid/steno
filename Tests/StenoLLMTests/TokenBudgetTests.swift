import Foundation
import StenoCore
import Testing

@testable import StenoLLM

@Suite struct TokenBudgetTests {
  @Test func germanThousandWordsEstimateBetween1200And2500Tokens() throws {
    let text = try String(contentsOf: Fixtures.url("llm/text/de-1000-words.txt"), encoding: .utf8)
    let words = text.split(whereSeparator: \.isWhitespace).count
    #expect(words == 1000)
    let tokens = TokenBudget.estimateTokens(text, language: "de")
    // Three bytes per token on German prose: 1.2 to 2.5 tokens per word, a
    // deliberate overestimate against real tokenizers (about 1.4 to 1.8).
    #expect(tokens >= 1200 && tokens <= 2500, "\(tokens) tokens for \(words) words")
    #expect(tokens == Int((Double(text.utf8.count) / 3.0).rounded(.up)))
  }

  @Test func englishDividesByMoreAndUnknownIsConservative() {
    let text = String(repeating: "The quick brown fox jumps over the lazy dog. ", count: 20)
    let english = TokenBudget.estimateTokens(text, language: "en-US")
    let german = TokenBudget.estimateTokens(text, language: "de")
    let unknown = TokenBudget.estimateTokens(text, language: nil)
    #expect(english < german)
    #expect(unknown == german)
    #expect(english == Int((Double(text.utf8.count) / 3.6).rounded(.up)))
    #expect(TokenBudget.estimateTokens("", language: "de") == 0)
    #expect(TokenBudget.estimateTokens("ä", language: "de") == 1)
    #expect(TokenBudget.bytesPerToken("EN") == 3.6)
    #expect(TokenBudget.bytesPerToken("fr") == 3.0)
  }

  @Test func inputBudgetIsWhatRemainsAndNeverNegative() {
    let endpoint = LLMEndpoint(
      baseURL: URL(string: "http://127.0.0.1:1/v1")!, model: "m", contextTokens: 8_000)
    let budget = TokenBudget(
      endpoint: endpoint, reservedOutputTokens: 2_000, promptOverheadTokens: 1_500)
    #expect(budget.inputBudget == 4_500)
    #expect(budget.fits(4_500))
    #expect(!budget.fits(4_501))
    let tiny = TokenBudget(
      contextTokens: 1_000, reservedOutputTokens: 800, promptOverheadTokens: 500)
    #expect(tiny.inputBudget == 0)
  }

  @Test func primarySubtagDropsRegionAndScript() {
    #expect(LanguageTag("de-CH").primarySubtag == "de")
    #expect(LanguageTag("zh-Hant-TW").primarySubtag == "zh")
    #expect(LanguageTag("EN").primarySubtag == "en")
  }
}
