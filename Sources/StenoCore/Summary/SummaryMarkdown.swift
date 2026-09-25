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

  static func substitute(_ text: String, names: [(label: String, name: String)], bold: Bool)
    -> String
  {
    var result = text
    for (label, name) in names {
      let replacement = bold ? "**\(name)**" : name
      result = result.replacingOccurrences(of: label, with: replacement)
    }
    return result
  }
}
