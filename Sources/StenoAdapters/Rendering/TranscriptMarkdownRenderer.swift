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
    var parts = ArtifactRenderer.noteHead(export, kind: "Transcript", options: options)
    let turns = Self.turns(ArtifactRenderer.orderedSegments(export))
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
    for segment in segments {
      let text = MarkdownText.singleLine(segment.text)
      guard !text.isEmpty else { continue }
      if let last = turns.indices.last, turns[last].speakerID == segment.speakerID {
        if segment.start - previousEnd >= paragraphGap {
          turns[last].paragraphs.append(text)
        } else {
          turns[last].paragraphs[turns[last].paragraphs.count - 1] += " " + text
        }
      } else {
        turns.append(Turn(speakerID: segment.speakerID, start: segment.start, paragraphs: [text]))
      }
      previousEnd = max(previousEnd, segment.end)
    }
    return turns
  }
}
