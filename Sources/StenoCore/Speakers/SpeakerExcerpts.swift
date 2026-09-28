import Foundation

/// What a speaker said in their sample clip, for the picker row that replaces
/// the cluster confidence: the speaker's segments overlapping
/// `Speaker.sampleClipRange` joined with a space, else their longest segment.
public enum SpeakerExcerpts {
  /// Characters kept before the text is cut and "…" appended.
  public static let maxLength = 160

  /// The excerpt for `speaker`, or "" when no segment belongs to it.
  ///
  /// Uses the speaker's segments (by `speakerID`) that overlap the clip range,
  /// sorted by start. When the range is nil or nothing overlaps, the longest
  /// segment by duration is used instead. A leading "…" marks text that
  /// starts before the clip; text longer than `maxLength` is cut there and
  /// ends with "…". Whitespace is trimmed.
  public static func text(for speaker: Speaker, in segments: [TranscriptSegment]) -> String {
    let own = segments.filter { $0.speakerID == speaker.id }
    guard !own.isEmpty else { return "" }

    var used: [TranscriptSegment] = []
    var cutAtStart = false
    if let range = speaker.sampleClipRange {
      used = own.filter { $0.start < range.upperBound && $0.end > range.lowerBound }
        .sorted { $0.start < $1.start }
      if let first = used.first, first.start < range.lowerBound { cutAtStart = true }
    }
    if used.isEmpty {
      guard let longest = own.max(by: { $0.end - $0.start < $1.end - $1.start }) else {
        return ""
      }
      used = [longest]
      cutAtStart = false
    }

    var text =
      used.map { $0.text.trimmingCharacters(in: .whitespacesAndNewlines) }
      .filter { !$0.isEmpty }
      .joined(separator: " ")
    if text.count > maxLength {
      text = String(text.prefix(maxLength)).trimmingCharacters(in: .whitespacesAndNewlines) + "…"
    }
    if cutAtStart, !text.isEmpty { text = "…" + text }
    return text
  }
}
