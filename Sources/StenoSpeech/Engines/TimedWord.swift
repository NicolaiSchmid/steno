import Foundation
import StenoCore

/// One timed piece of text, the unit `TranscriptSegmenter` works on. Both
/// engines produce words: Parakeet's decoder tokens (SentencePiece pieces,
/// a leading space or `▁` marking a word start) arrive as these too and
/// `TokenAggregator` joins them; WhisperKit reports words directly.
struct TimedWord: Sendable, Equatable {
  var text: String
  var start: TimeInterval
  var end: TimeInterval
  var confidence: Float

  init(text: String, start: TimeInterval, end: TimeInterval, confidence: Float = 1) {
    self.text = text
    self.start = start
    self.end = end
    self.confidence = confidence
  }

  var wordTiming: WordTiming {
    WordTiming(word: text, start: start, end: end)
  }
}
