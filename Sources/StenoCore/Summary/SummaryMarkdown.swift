import Foundation

/// One summary section as rendered: the heading and its bullets as inline
/// Markdown (`**lead**: text`) with every speaker cluster label already
/// replaced by the speaker's current name. What `SummaryMarkdown.render`
/// joins, and what a UI iterates instead of re-parsing the Markdown.
public struct RenderedSection: Sendable, Equatable, Hashable {
  /// The `SummarySection.id` (a template section id).
  public var id: String
  public var heading: String
  /// Inline Markdown per bullet, without the list marker.
  public var bullets: [String]

  public init(id: String, heading: String, bullets: [String]) {
    self.id = id
    self.heading = heading
    self.bullets = bullets
  }

  /// The bullets as a Markdown list, one `- ` line each, no trailing newline.
  public var body: String {
    bullets.map { "- \($0)" }.joined(separator: "\n")
  }

  /// `## heading`, a blank line, then `body`.
  public var markdown: String {
    "## \(heading)\n\n\(body)"
  }
}

/// Renders a `SummaryDocument` to Markdown at display and export time:
/// `## heading` per section, `- **lead**: text` per bullet, and every
/// speaker cluster label ("Speaker 2") replaced by the speaker's current
/// name in bold. Renaming a speaker is therefore a re-render, never an LLM
/// re-run. `sections(for:)` is the structured form `render` joins; a UI that
/// shows sections renders those and never parses the Markdown back.
/// Adapters that need a heading offset shift the `##` themselves.
public enum SummaryMarkdown {
  /// Every section with at least one bullet, in document order, names
  /// substituted. Empty when the meeting has no summary yet.
  public static func sections(for export: MeetingExport) -> [RenderedSection] {
    guard let summary = export.meeting.summary else { return [] }
    let names = speakerNames(export)
    return summary.sections.compactMap { section in
      guard !section.bullets.isEmpty else { return nil }
      let bullets = section.bullets.map { bullet in
        let lead = substitute(bullet.lead, names: names, bold: false)
        let text = substitute(bullet.text, names: names, bold: true)
        return lead.isEmpty ? text : "**\(lead)**: \(text)"
      }
      return RenderedSection(id: section.id, heading: section.heading, bullets: bullets)
    }
  }

  /// `sections(for:)` joined by blank lines, with one trailing newline; the
  /// empty string when there is nothing to render.
  public static func render(_ export: MeetingExport) -> String {
    let sections = sections(for: export)
    return sections.isEmpty
      ? "" : sections.map(\.markdown).joined(separator: "\n\n") + "\n"
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
