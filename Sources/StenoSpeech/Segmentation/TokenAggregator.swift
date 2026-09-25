import Foundation

/// Joins SentencePiece tokens into words. A token starting with `▁` begins
/// a new word; punctuation-only tokens glue to the word before them; tokens
/// that are empty after stripping the marker only carry a word boundary.
public struct TokenAggregator: Sendable {
  public static let wordBoundary: Character = "\u{2581}"

  public init() {}

  public func words(from tokens: [TimedWord]) -> [TimedWord] {
    var words: [TimedWord] = []
    var current: TimedWord?
    var confidences: [Float] = []
    var boundaryPending = true

    func flush() {
      if var word = current, !word.text.isEmpty {
        word.confidence =
          confidences.isEmpty ? 1 : confidences.reduce(0, +) / Float(confidences.count)
        words.append(word)
      }
      current = nil
      confidences = []
    }

    for token in tokens {
      var text = token.text
      let startsWord = text.first == Self.wordBoundary
      if startsWord { text.removeFirst() }
      text = text.trimmingCharacters(in: .whitespaces)
      guard !text.isEmpty else {
        // A bare marker: the next piece starts a word.
        if startsWord { boundaryPending = true }
        continue
      }
      let punctuationOnly = text.unicodeScalars.allSatisfy {
        CharacterSet.punctuationCharacters.contains($0) || CharacterSet.symbols.contains($0)
      }
      if punctuationOnly, let word = current {
        current = TimedWord(
          text: word.text + text, start: word.start, end: max(word.end, token.end),
          confidence: word.confidence)
        confidences.append(token.confidence)
        boundaryPending = false
        continue
      }
      if startsWord || boundaryPending || current == nil {
        flush()
        current = TimedWord(text: text, start: token.start, end: token.end)
        confidences = [token.confidence]
      } else if let word = current {
        current = TimedWord(
          text: word.text + text, start: word.start, end: max(word.end, token.end),
          confidence: word.confidence)
        confidences.append(token.confidence)
      }
      boundaryPending = false
    }
    flush()
    return words
  }
}
