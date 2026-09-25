import Foundation
import StenoCore

/// The model's answer to one cleanup chunk: the same segments by index.
public struct CleanupDraft: Codable, Sendable, Equatable {
  public struct Segment: Codable, Sendable, Equatable {
    public var index: Int
    public var text: String

    public init(index: Int, text: String) {
      self.index = index
      self.text = text
    }
  }

  public var segments: [Segment]

  public init(segments: [Segment]) {
    self.segments = segments
  }
}

/// Checks a draft against its chunk: same count, indices `0..<n` each once,
/// no emptied segment, and a word count within `wordRatio` of the input
/// (or within one word, so "Git Hub" may become "GitHub"). Returns the
/// cleaned texts in segment order.
public struct CleanupValidator: Sendable, Equatable {
  public var wordRatio: ClosedRange<Double>

  public init(wordRatio: ClosedRange<Double> = 0.7...1.3) {
    self.wordRatio = wordRatio
  }

  public struct Rejection: Error, Sendable, Equatable, CustomStringConvertible {
    public var reasons: [String]
    public var description: String { reasons.joined(separator: " ") }
  }

  public func validate(_ draft: CleanupDraft, against chunk: TranscriptChunk) throws -> [String] {
    let expected = chunk.segments.count
    var reasons: [String] = []
    if draft.segments.count != expected {
      reasons.append("Expected \(expected) segments, got \(draft.segments.count).")
    }
    let indices = draft.segments.map(\.index).sorted()
    if indices != Array(0..<expected) {
      reasons.append(
        "Indices must be 0 to \(expected - 1), each exactly once; got \(indices.map(String.init).joined(separator: ", "))."
      )
    }
    if !reasons.isEmpty { throw Rejection(reasons: reasons) }
    var texts = [String](repeating: "", count: expected)
    for segment in draft.segments {
      let original = chunk.segments[segment.index]
      let cleaned = segment.text.trimmingCharacters(in: .whitespacesAndNewlines)
      let originalWords = Self.wordCount(original.text)
      let cleanedWords = Self.wordCount(cleaned)
      if originalWords > 0 && cleanedWords == 0 {
        reasons.append("Segment \(segment.index) came back empty.")
        continue
      }
      if originalWords > 0 {
        let ratio = Double(cleanedWords) / Double(originalWords)
        if abs(cleanedWords - originalWords) > 1 && !wordRatio.contains(ratio) {
          reasons.append(
            "Segment \(segment.index) changed from \(originalWords) to \(cleanedWords) words; keep the wording, only fix spelling, casing and punctuation."
          )
        }
      }
      texts[segment.index] = cleaned
    }
    if !reasons.isEmpty { throw Rejection(reasons: reasons) }
    return texts
  }

  static func wordCount(_ text: String) -> Int {
    text.split(whereSeparator: \.isWhitespace).count
  }
}
