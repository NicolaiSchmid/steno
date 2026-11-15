import Foundation

/// What a speaker says in their sample clip, so the quote is what plays: the
/// speaker's segments overlapping `Speaker.sampleClipRange` joined with a
/// space, else their longest segment. Rust: `excerpt` in `steno-host`'s
/// `speakers.rs`, the same algorithm.
public enum SpeakerExcerpts {
  /// Characters (Unicode scalars, as Rust counts them) kept before the text
  /// is cut and "…" appended.
  public static let maxLength = 160

  /// The excerpt for `speaker`, or "" when no segment belongs to it.
  ///
  /// Uses the speaker's segments (by `speakerID`) that overlap the clip range,
  /// sorted by start. When the range is nil or nothing overlaps, the longest
  /// segment by duration is used instead (the last of equally long ones). A leading "…" marks text that
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
      // `reduce` keeps the last of equally long segments, as Rust's `max_by`.
      let longest = own.dropFirst().reduce(own[0]) {
        $1.end - $1.start >= $0.end - $0.start ? $1 : $0
      }
      used = [longest]
      cutAtStart = false
    }

    var text =
      used.map { $0.text.trimmingCharacters(in: .whitespacesAndNewlines) }
      .filter { !$0.isEmpty }
      .joined(separator: " ")
    if text.unicodeScalars.count > maxLength {
      text =
        String(String.UnicodeScalarView(text.unicodeScalars.prefix(maxLength)))
        .trimmingCharacters(in: .whitespacesAndNewlines) + "…"
    }
    if cutAtStart, !text.isEmpty { text = "…" + text }
    return text
  }
}
