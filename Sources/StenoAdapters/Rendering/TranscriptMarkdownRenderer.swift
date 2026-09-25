import Foundation
import StenoCore

/// The transcript as reading Markdown: one turn per run of consecutive
/// segments by one speaker, a paragraph break inside a turn at gaps of
/// `paragraphGap` seconds or more.
struct TranscriptMarkdownRenderer {
  let export: MeetingExport
  let options: RenderOptions

  static let paragraphGap: TimeInterval = 3

  struct Turn {
    var speakerID: UUID?
    var start: TimeInterval
    var paragraphs: [String]
  }

  func render() -> String {
    let names = Names(export: export, options: options)
    var frontmatter = Frontmatter(timeZone: options.timeZone)
    frontmatter.append("title", .string("\(export.meeting.title) — Transcript"))
    frontmatter.append("type", .string("transcript"))
    frontmatter.append("steno_id", .string(ArtifactRenderer.stenoID(export)))
    var parts = [frontmatter.encoded()]
    parts.append("# \(MarkdownText.singleLine(export.meeting.title)) — Transcript\n")
    let turns = Self.turns(export.segments)
    if turns.isEmpty {
      parts.append("No transcript.\n")
    }
    for turn in turns {
      parts.append("## \(names.linkedSpeaker(turn.speakerID)) — \(Timecode.clock(turn.start))\n")
      parts.append(
        turn.paragraphs.map(MarkdownText.escapeParagraphStart).joined(separator: "\n\n") + "\n")
    }
    return parts.joined(separator: "\n")
  }

  /// Segments in start order grouped into turns; empty texts are skipped.
  static func turns(_ segments: [TranscriptSegment]) -> [Turn] {
    var turns: [Turn] = []
    var previousEnd: TimeInterval = 0
    for segment in segments.sorted(by: {
      ($0.start, $0.id.uuidString) < ($1.start, $1.id.uuidString)
    }) {
      let text = MarkdownText.singleLine(segment.text)
      guard !text.isEmpty else { continue }
      if var last = turns.last, last.speakerID == segment.speakerID {
        if segment.start - previousEnd >= paragraphGap {
          last.paragraphs.append(text)
        } else {
          last.paragraphs[last.paragraphs.count - 1] += " " + text
        }
        turns[turns.count - 1] = last
      } else {
        turns.append(Turn(speakerID: segment.speakerID, start: segment.start, paragraphs: [text]))
      }
      previousEnd = max(previousEnd, segment.end)
    }
    return turns
  }
}
