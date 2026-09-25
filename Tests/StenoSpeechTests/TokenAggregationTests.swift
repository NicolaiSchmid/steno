import Foundation
import StenoCore
import Testing

@testable import StenoSpeech

@Suite struct TokenAggregationTests {
  private func token(_ text: String, _ start: TimeInterval, _ end: TimeInterval, _ c: Float = 1)
    -> TimedToken
  {
    TimedToken(text: text, start: start, end: end, confidence: c)
  }

  @Test func aWordOverThreeTokens() {
    let words = TokenAggregator().words(from: [
      token("▁Pro", 0.0, 0.1, 0.9), token("dukt", 0.1, 0.2, 0.8), token("strategie", 0.2, 0.4, 0.7),
    ])
    #expect(words == [TimedWord(text: "Produktstrategie", start: 0.0, end: 0.4, confidence: 0.8)])
  }

  @Test func punctuationGluesToThePreviousWord() {
    let words = TokenAggregator().words(from: [
      token("▁Hallo", 0, 0.2), token(",", 0.2, 0.25), token("▁Welt", 0.3, 0.5),
      token(".", 0.5, 0.55),
    ])
    #expect(words.map(\.text) == ["Hallo,", "Welt."])
    #expect(words.last?.end == 0.55)
  }

  @Test func aLeadingMarkerOnlyStartsTheNextWord() {
    let words = TokenAggregator().words(from: [
      token("▁", 0, 0.05), token("O", 0.05, 0.1), token("K", 0.1, 0.15), token("▁", 0.2, 0.2),
      token("go", 0.2, 0.3),
    ])
    #expect(words.map(\.text) == ["OK", "go"])
    #expect(words.first?.start == 0.05)
  }

  @Test func emptyTokensAreDropped() {
    let words = TokenAggregator().words(from: [
      token("", 0, 0.1), token("▁a", 0.1, 0.2), token(" ", 0.2, 0.3), token("b", 0.3, 0.4),
    ])
    #expect(words.map(\.text) == ["ab"])
  }

  @Test func firstTokenWithoutMarkerStillStartsAWord() {
    let words = TokenAggregator().words(from: [token("hi", 0, 0.1), token("▁there", 0.2, 0.3)])
    #expect(words.map(\.text) == ["hi", "there"])
  }
}

@Suite struct TranscriptSegmenterTests {
  private func words(_ specs: [(String, TimeInterval, TimeInterval)]) -> [TimedWord] {
    specs.map { TimedWord(text: $0.0, start: $0.1, end: $0.2) }
  }

  @Test func splitsOnGaps() {
    let segments = TranscriptSegmenter().segments(
      fromWords: words([("eins", 0, 0.3), ("zwei", 0.4, 0.7), ("drei", 1.6, 1.9)]))
    #expect(segments.map(\.text) == ["eins zwei", "drei"])
    #expect(segments.map(\.start) == [0, 1.6])
    #expect(segments.map(\.end) == [0.7, 1.9])
    #expect(segments.first?.wordTimings?.count == 2)
    #expect(segments.allSatisfy { $0.language == nil })
  }

  @Test func splitsAfterSentencePunctuation() {
    let segments = TranscriptSegmenter().segments(
      fromWords: words([("Ja.", 0, 0.2), ("Gut", 0.3, 0.5), ("so?", 0.6, 0.8), ("Fein!", 0.9, 1.1)])
    )
    #expect(segments.map(\.text) == ["Ja.", "Gut so?", "Fein!"])
  }

  @Test func capsSegmentsAtThirtySeconds() {
    let long = (0..<40).map { index in
      TimedWord(text: "w\(index)", start: Double(index), end: Double(index) + 0.9)
    }
    let segments = TranscriptSegmenter().segments(fromWords: long)
    #expect(segments.count == 2)
    #expect(segments[0].end - segments[0].start <= 30)
    #expect(segments[0].wordTimings?.count == 30)
    #expect(segments[1].text.hasPrefix("w30"))
  }

  @Test func emptyInputGivesNoSegments() {
    #expect(TranscriptSegmenter().segments(fromWords: []).isEmpty)
    #expect(TranscriptSegmenter().segments(fromWords: words([("", 0, 1)])).isEmpty)
  }
}

@Suite struct LanguageTaggerTests {
  private let tagger = LanguageTagger(recognizer: StopwordLanguageRecognizer())

  private func segment(_ text: String, _ start: TimeInterval, _ end: TimeInterval) -> RawSegment {
    RawSegment(start: start, end: end, text: text)
  }

  @Test func tagsGermanAndEnglishSegments() {
    let tagged = tagger.tag([
      segment("Wir haben das heute nicht mit dem Kunden besprochen", 0, 4),
      segment("We should ship the new onboarding flow this week", 4, 8),
    ])
    #expect(tagged.map(\.language) == ["de", "en"])
  }

  @Test func shortSegmentsInheritThePreviousLanguage() {
    let tagged = tagger.tag([
      segment("Wir haben das heute nicht besprochen", 0, 3),
      segment("Okay cool", 3, 4),
      segment("We should ship the flow this week and not wait", 4, 8),
      segment("Genau", 8, 9),
    ])
    #expect(tagged.map(\.language) == ["de", "de", "en", "en"])
  }

  @Test func aLeadingShortSegmentTakesTheNextTaggedLanguage() {
    let tagged = tagger.tag([
      segment("Hm ja", 0, 1),
      segment("We should ship the flow this week and not wait", 1, 5),
    ])
    #expect(tagged.map(\.language) == ["en", "en"])
  }

  @Test func hintCoversUndecidableText() {
    let tagged = tagger.tag([segment("Kubernetes Grafana Prometheus Terraform", 0, 2)], hint: "de")
    #expect(tagged.first?.language == "de")
    let untagged = tagger.tag([segment("Kubernetes Grafana Prometheus Terraform", 0, 2)])
    #expect(untagged.first?.language == nil)
  }

  @Test func dominantLanguageIsWeightedByDuration() {
    let segments = [
      RawSegment(start: 0, end: 10, text: "", language: "de"),
      RawSegment(start: 10, end: 13, text: "", language: "en"),
      RawSegment(start: 13, end: 16, text: "", language: "en"),
    ]
    #expect(tagger.dominantLanguage(of: segments) == "de")
    #expect(tagger.dominantLanguage(of: []) == nil)
    #expect(LanguageTagger.languageFlips(in: segments) == 1)
    #expect(LanguageTagger.languageFlips(in: []) == 0)
  }

  @Test func candidatesLimitTheAnswer() {
    let englishOnly = LanguageTagger(candidates: ["en"], recognizer: StopwordLanguageRecognizer())
    let tagged = englishOnly.tag([
      segment("Wir haben das heute nicht mit dem Kunden besprochen", 0, 4)
    ])
    #expect(tagged.first?.language == nil)
  }

  #if canImport(NaturalLanguage)
    @Test func naturalLanguageRecognizerAgreesOnClearSentences() {
      let recognizer = NaturalLanguageRecognizer()
      let candidates: Set<LanguageTag> = ["de", "en"]
      #expect(
        recognizer.recognize(
          "Wir sollten die Produktstrategie morgen mit dem ganzen Team besprechen.",
          candidates: candidates, hint: nil) == "de")
      #expect(
        recognizer.recognize(
          "Let us review the onboarding numbers before the customer call tomorrow.",
          candidates: candidates, hint: nil) == "en")
      #expect(
        recognizer.recognize(
          "Bonjour tout le monde, comment allez-vous?", candidates: ["fr"], hint: nil)
          == "fr")
    }
  #endif
}
