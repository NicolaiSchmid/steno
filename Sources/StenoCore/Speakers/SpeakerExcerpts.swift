import Foundation

/// What a speaker says in or around their sample clip: the speaker's
/// segments overlapping `Speaker.sampleClipRange` joined with a space, else
/// their longest segment. Rust: `excerpt` in `steno-host`'s `speakers.rs`,
/// the same algorithm.
public enum SpeakerExcerpts {
  /// Characters (Unicode scalars, as Rust counts them) kept before the text
  /// is cut and "…" appended.
  public static let maxLength = 160

  /// Slack at the clip's edges for a segment to count as inside it, in
  /// seconds. Rust: `CLIP_EDGE`.
  static let clipEdge: TimeInterval = 0.25

  /// What a speaker's row shows of what they said.
  public struct Excerpt: Sendable, Equatable {
    public var text: String
    /// Whether `text` is exactly what the sample clip plays: every segment
    /// used lies inside the clip. Segments carry no word timings, so a
    /// segment only partly inside the clip makes the text context around
    /// it, as does the longest segment of a voice without a clip.
    public var inClip: Bool
  }

  /// The excerpt's text for `speaker`, or "" when no segment belongs to it.
  public static func text(for speaker: Speaker, in segments: [TranscriptSegment]) -> String {
    excerpt(for: speaker, in: segments).text
  }

  /// The excerpt for `speaker`; empty text when no segment belongs to it.
  ///
  /// Uses the speaker's segments (by `speakerID`) that overlap the clip range,
  /// sorted by start, a quote when all of them lie inside it (`inClip`).
  /// When the range is nil or nothing overlaps, the longest segment by
  /// duration is used instead (the last of equally long ones), as context. A
  /// leading "…" marks text that starts before the clip; text longer than
  /// `maxLength` is cut there and ends with "…". Whitespace is trimmed.
  public static func excerpt(for speaker: Speaker, in segments: [TranscriptSegment]) -> Excerpt {
    let own = segments.filter { $0.speakerID == speaker.id }
    guard !own.isEmpty else { return Excerpt(text: "", inClip: false) }

    var used: [TranscriptSegment] = []
    var cutAtStart = false
    var inClip = false
    if let range = speaker.sampleClipRange {
      used = own.filter { $0.start < range.upperBound && $0.end > range.lowerBound }
        .sorted { $0.start < $1.start }
      if let first = used.first, first.start < range.lowerBound { cutAtStart = true }
      inClip = used.allSatisfy {
        $0.start >= range.lowerBound - clipEdge && $0.end <= range.upperBound + clipEdge
      }
    }
    if used.isEmpty {
      // `reduce` keeps the last of equally long segments, as Rust's `max_by`.
      let longest = own.dropFirst().reduce(own[0]) {
        $1.end - $1.start >= $0.end - $0.start ? $1 : $0
      }
      used = [longest]
      cutAtStart = false
      inClip = false
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
    return Excerpt(text: text, inClip: inClip)
  }

  /// What a combined voice says in its own clip: the text of the segments
  /// that overlap its clip range, cut as `excerpt(for:in:)` cuts it, and only
  /// when they all lie inside the clip, so it is a quote of what plays. The
  /// segments are matched by time, not by speaker, since they now read as
  /// the voice it joined. nil without a range, an overlapping segment, or a
  /// segment reaching outside the clip. Rust: `combined_excerpt` in
  /// `steno-host`.
  public static func clipText(for speaker: Speaker, in segments: [TranscriptSegment]) -> String? {
    guard let range = speaker.sampleClipRange else { return nil }
    let overlapping = segments.filter { $0.start < range.upperBound && $0.end > range.lowerBound }
      .map { segment in
        var segment = segment
        segment.speakerID = speaker.id
        return segment
      }
    let excerpt = excerpt(for: speaker, in: overlapping)
    return excerpt.inClip && !excerpt.text.isEmpty ? excerpt.text : nil
  }
}
