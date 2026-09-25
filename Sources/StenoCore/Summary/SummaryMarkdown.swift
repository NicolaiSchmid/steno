import Foundation

/// Renders a `SummaryDocument` to Markdown at display and export time:
/// `## heading` per section, `- **lead**: text` per bullet, and every
/// speaker cluster label ("Speaker 2") replaced by the speaker's current
/// name in bold. Renaming a speaker is therefore a re-render, never an LLM
/// re-run. Adapters that need a heading offset shift the `##` themselves.
public enum SummaryMarkdown {
  public static func render(_ export: MeetingExport) -> String {
    guard let summary = export.meeting.summary else { return "" }
    let names = speakerNames(export)
    var blocks: [String] = []
    for section in summary.sections where !section.bullets.isEmpty {
      var lines = ["## \(section.heading)", ""]
      for bullet in section.bullets {
        let lead = substitute(bullet.lead, names: names, bold: false)
        let text = substitute(bullet.text, names: names, bold: true)
        lines.append(lead.isEmpty ? "- \(text)" : "- **\(lead)**: \(text)")
      }
      blocks.append(lines.joined(separator: "\n"))
    }
    return blocks.isEmpty ? "" : blocks.joined(separator: "\n\n") + "\n"
  }

  /// Cluster label to current display name, for speakers that resolved to a
  /// person; unresolved labels stay as they are. Longest labels first so
  /// "Speaker 10" is never matched by "Speaker 1".
  static func speakerNames(_ export: MeetingExport) -> [(label: String, name: String)] {
    export.speakers
      .compactMap { speaker -> (String, String)? in
        guard speaker.personID != nil else { return nil }
        let name = export.displayName(forSpeaker: speaker.id)
        guard !name.isEmpty, name != speaker.clusterLabel else { return nil }
        return (speaker.clusterLabel, name)
      }
      .sorted { lhs, rhs in
        if lhs.0.count != rhs.0.count { return lhs.0.count > rhs.0.count }
        return lhs.0 < rhs.0
      }
  }

  /// Replaces every label that stands as a whole word (no letter or digit
  /// touching it on either side), in one pass over `text`, so "Me" never
  /// matches inside "Meeting" and a name written in never gets matched again.
  static func substitute(_ text: String, names: [(label: String, name: String)], bold: Bool)
    -> String
  {
    guard !names.isEmpty else { return text }
    var result = ""
    var index = text.startIndex
    while index < text.endIndex {
      if isWordBoundary(text, before: index),
        let (label, name) = names.first(where: { candidate in
          text[index...].hasPrefix(candidate.label)
            && isWordBoundary(text, after: text.index(index, offsetBy: candidate.label.count))
        })
      {
        result += bold ? "**\(name)**" : name
        index = text.index(index, offsetBy: label.count)
      } else {
        result.append(text[index])
        index = text.index(after: index)
      }
    }
    return result
  }

  private static func isWordBoundary(_ text: String, before index: String.Index) -> Bool {
    index == text.startIndex || !isWordCharacter(text[text.index(before: index)])
  }

  private static func isWordBoundary(_ text: String, after index: String.Index) -> Bool {
    index == text.endIndex || !isWordCharacter(text[index])
  }

  private static func isWordCharacter(_ character: Character) -> Bool {
    character.isLetter || character.isNumber
  }
}
