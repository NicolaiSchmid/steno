import Foundation
import StenoCore

/// WebVTT per the W3C spec: `hh:mm:ss.ttt` timestamps, one cue per segment
/// in start order, `<v Name>text` voice spans, `NOTE` block with title and
/// date. Payload text escapes `& < >` and drops line breaks and `-->`;
/// voice annotations drop `& >` and line breaks.
struct WebVTTRenderer {
  let export: MeetingExport

  static let minimumCue: TimeInterval = 0.001

  func render() -> String {
    let names = Names(export: export, options: .plain)
    var blocks = ["WEBVTT - Steno \(ArtifactRenderer.stenoID(export))"]
    blocks.append(
      "NOTE\n\(Self.noteText(export.meeting.title))\n\(DateText.utc(export.meeting.startedAt))")
    for segment in ArtifactRenderer.orderedSegments(export) {
      let start = max(0, segment.start)
      let end = max(segment.end, start + Self.minimumCue)
      blocks.append(
        "\(Timecode.vtt(start)) --> \(Timecode.vtt(end))\n<v \(Self.annotation(names.speaker(segment.speakerID)))>\(Self.payload(segment.text))"
      )
    }
    return blocks.joined(separator: "\n\n") + "\n"
  }

  static func payload(_ text: String) -> String {
    MarkdownText.singleLine(text)
      .replacingOccurrences(of: "-->", with: "")
      .replacingOccurrences(of: "&", with: "&amp;")
      .replacingOccurrences(of: "<", with: "&lt;")
      .replacingOccurrences(of: ">", with: "&gt;")
  }

  static func annotation(_ name: String) -> String {
    MarkdownText.singleLine(name)
      .replacingOccurrences(of: "&", with: "")
      .replacingOccurrences(of: ">", with: "")
  }

  static func noteText(_ text: String) -> String {
    let single = MarkdownText.singleLine(text).replacingOccurrences(of: "-->", with: "")
    return single.isEmpty ? "Untitled" : single
  }
}
