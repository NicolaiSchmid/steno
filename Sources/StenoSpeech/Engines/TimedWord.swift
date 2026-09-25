import Foundation
import StenoCore

/// StenoCore's `WordTiming`, named so files that import FluidAudio or
/// WhisperKit (which export their own `WordTiming`) can still reach it.
/// `StenoCore.WordTiming` would resolve to the `StenoCore` enum.
typealias CoreWordTiming = WordTiming

/// One timed piece of text, the unit `TranscriptSegmenter` works on. Both
/// engines produce words: Parakeet's decoder tokens (SentencePiece pieces,
/// a leading `▁` marking a word start) arrive as these too and
/// `TokenAggregator` joins them; WhisperKit reports words directly.
public struct TimedWord: Sendable, Equatable {
  public var text: String
  public var start: TimeInterval
  public var end: TimeInterval
  public var confidence: Float

  public init(text: String, start: TimeInterval, end: TimeInterval, confidence: Float = 1) {
    self.text = text
    self.start = start
    self.end = end
    self.confidence = confidence
  }

  public var wordTiming: WordTiming {
    WordTiming(word: text, start: start, end: end)
  }
}
