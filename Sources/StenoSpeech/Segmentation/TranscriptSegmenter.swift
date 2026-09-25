import Foundation
import StenoCore

/// Turns timed words into `RawSegment`s: a new segment starts after a pause
/// longer than `splitGapSeconds`, after sentence-final punctuation, and
/// whenever the next word would push the segment past `maxSegmentSeconds`.
/// `language` stays nil; `LanguageTagger` fills it.
public struct TranscriptSegmenter: Sendable {
  public var maxSegmentSeconds: TimeInterval
  public var splitGapSeconds: TimeInterval
  public static let sentenceEnd: Set<Character> = [".", "?", "!"]

  public init(maxSegmentSeconds: TimeInterval = 30, splitGapSeconds: TimeInterval = 0.7) {
    self.maxSegmentSeconds = maxSegmentSeconds
    self.splitGapSeconds = splitGapSeconds
  }

  public func segments(fromWords words: [TimedWord]) -> [RawSegment] {
    var segments: [RawSegment] = []
    var current: [TimedWord] = []

    func flush() {
      guard let first = current.first, let last = current.last else { return }
      segments.append(
        RawSegment(
          start: first.start,
          end: max(last.end, first.start),
          text: current.map(\.text).joined(separator: " "),
          wordTimings: current.map(\.wordTiming)))
      current = []
    }

    for word in words where !word.text.isEmpty {
      if let previous = current.last, let first = current.first {
        let gap = word.start - previous.end
        let endsSentence = previous.text.last.map(Self.sentenceEnd.contains) ?? false
        let tooLong = word.end - first.start > maxSegmentSeconds
        if gap > splitGapSeconds || endsSentence || tooLong { flush() }
      }
      current.append(word)
    }
    flush()
    return segments
  }
}
