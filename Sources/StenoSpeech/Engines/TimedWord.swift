import Foundation
import StenoCore

/// One decoder token with its timing, as FluidAudio's `TokenTiming` reports
/// it: SentencePiece pieces where a leading `▁` marks a word start.
public struct TimedToken: Sendable, Equatable {
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
}

/// StenoCore's `WordTiming`, named so files that import FluidAudio or
/// WhisperKit (which export their own `WordTiming`) can still reach it.
/// `StenoCore.WordTiming` would resolve to the `StenoCore` enum.
typealias CoreWordTiming = WordTiming

/// One word with its timing, the unit `TranscriptSegmenter` works on. Both
/// engines produce it: Parakeet through `TokenAggregator`, WhisperKit from
/// its own word timestamps.
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
