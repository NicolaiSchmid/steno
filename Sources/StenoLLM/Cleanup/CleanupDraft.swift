import Foundation
import StenoCore

/// The model's answer to one cleanup chunk: the same segments by index.
public struct CleanupDraft: Codable, Sendable, Equatable {
  public struct Segment: Codable, Sendable, Equatable {
    public var index: Int
    public var text: String
  }

  public var segments: [Segment]

  /// Word counts may drift by at most this ratio, or by one word so that
  /// "Git Hub" can become "GitHub".
  static let wordRatio = 0.7...1.3

  /// Why the draft does not fit its chunk, in prompt-ready sentences; empty
  /// when it does. Same count, indices `0..<n` each once, no emptied
  /// segment, word count within `wordRatio` of the input.
  public func problems(against chunk: TranscriptChunk) -> [String] {
    let expected = chunk.segments.count
    var problems: [String] = []
    if segments.count != expected {
      problems.append("Expected \(expected) segments, got \(segments.count).")
    }
    let indices = segments.map(\.index).sorted()
    if indices != Array(0..<expected) {
      problems.append(
        "Indices must be 0 to \(expected - 1), each exactly once; got \(indices.map(String.init).joined(separator: ", "))."
      )
    }
    guard problems.isEmpty else { return problems }
    for segment in segments {
      let originalWords = Self.wordCount(chunk.segments[segment.index].text)
      let cleanedWords = Self.wordCount(segment.text)
      guard originalWords > 0 else { continue }
      if cleanedWords == 0 {
        problems.append("Segment \(segment.index) came back empty.")
      } else if abs(cleanedWords - originalWords) > 1,
        !Self.wordRatio.contains(Double(cleanedWords) / Double(originalWords))
      {
        problems.append(
          "Segment \(segment.index) changed from \(originalWords) to \(cleanedWords) words; keep the wording, only fix spelling, casing and punctuation."
        )
      }
    }
    return problems
  }

  /// The draft with the framing of a prompt line stripped from the front
  /// of every text: an optional `[n]` index, then a speaker label from
  /// `labels` and its colon. The model reads `[3] Me: words` and some
  /// models answer `Me: words`; the label is one word, so the word-count
  /// check alone lets it through into the transcript.
  public func strippingSpeakerLabels(_ labels: SpeakerLabels) -> CleanupDraft {
    var stripped = self
    for index in stripped.segments.indices {
      stripped.segments[index].text = Self.strippingLabel(
        from: stripped.segments[index].text, labels: labels)
    }
    return stripped
  }

  /// `text` without a leading `[n]` index and `Label:`; `text` itself when
  /// neither is there, so a text that happens to start with a colon-bearing
  /// word ("Meeting: agenda") is left alone unless the word is a label.
  static func strippingLabel(from text: String, labels: SpeakerLabels) -> String {
    var rest = text[...].drop(while: \.isWhitespace)
    var stripped = false
    if rest.first == "[", let close = rest.firstIndex(of: "]"),
      Int(rest[rest.index(after: rest.startIndex)..<close]) != nil
    {
      rest = rest[rest.index(after: close)...].drop(while: \.isWhitespace)
      stripped = true
    }
    if let colon = rest.firstIndex(of: ":"), labels.isLabel(String(rest[..<colon])) {
      rest = rest[rest.index(after: colon)...].drop(while: \.isWhitespace)
      stripped = true
    }
    return stripped ? String(rest) : text
  }

  /// The cleaned texts in segment order, trimmed; meaningful once
  /// `problems(against:)` is empty.
  public var orderedTexts: [String] {
    segments.sorted { $0.index < $1.index }.map {
      $0.text.trimmingCharacters(in: .whitespacesAndNewlines)
    }
  }

  static func wordCount(_ text: String) -> Int {
    text.split(whereSeparator: \.isWhitespace).count
  }
}
