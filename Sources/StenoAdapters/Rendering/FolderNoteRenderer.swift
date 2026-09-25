import Foundation
import StenoCore

/// The meeting's folder note: the file Obsidian opens when the folder is
/// clicked.
struct FolderNoteRenderer {
  let export: MeetingExport
  let options: RenderOptions
  let folderSlug: String

  func render() -> String {
    let meeting = export.meeting
    var parts = [frontmatter().encoded()]
    parts.append("# \(MarkdownText.singleLine(meeting.title))\n")
    parts.append(infoLine() + "\n")
    parts.append("## Summary\n")
    let summary = Self.demoted(SummaryMarkdown.render(export))
    parts.append(summary.isEmpty ? "No summary.\n" : summary)
    if !export.decisions.isEmpty {
      parts.append("## Decisions\n")
      parts.append(
        export.decisions.map { "- \(MarkdownText.singleLine($0.text))" }.joined(separator: "\n")
          + "\n")
    }
    let scratchpad = meeting.scratchpad.trimmingCharacters(in: .whitespacesAndNewlines)
    if !scratchpad.isEmpty {
      parts.append("## Scratchpad\n")
      parts.append(scratchpad + "\n")
    }
    return parts.joined(separator: "\n")
  }

  func frontmatter() -> Frontmatter {
    let meeting = export.meeting
    var frontmatter = Frontmatter(timeZone: options.timeZone)
    frontmatter.append("title", .string(meeting.title))
    frontmatter.append("date", .dateTime(meeting.startedAt))
    frontmatter.append("duration", .int(Self.wholeMinutes(meeting.duration)))
    frontmatter.append(
      "participants", .list(Names(export: export, options: options).participants()))
    frontmatter.append("tags", .tags(["meeting"] + meeting.tags))
    frontmatter.append("source", .string(Self.sourceKey(meeting.source)))
    frontmatter.append("template", .string(meeting.templateID))
    if let language = meeting.language {
      frontmatter.append("language", .string(language.rawValue))
    }
    frontmatter.append("steno_id", .string(ArtifactRenderer.stenoID(export)))
    return frontmatter
  }

  /// `2026-09-24 14:00–15:30 · 1 h 30 min · Mac call · [[slug - Transcript|Transcript]] · [[slug - Tasks|Tasks]]`.
  func infoLine() -> String {
    let meeting = export.meeting
    let start = meeting.startedAt
    let end = start.addingTimeInterval(max(0, meeting.duration))
    let transcript = ObsidianLayout.transcriptNote(slug: folderSlug)
    let tasks = ObsidianLayout.tasksNote(slug: folderSlug)
    let links =
      switch options.linkStyle {
      case .wikilink:
        [
          MarkdownText.wikilink(String(transcript.dropLast(3)), alias: "Transcript"),
          MarkdownText.wikilink(String(tasks.dropLast(3)), alias: "Tasks"),
        ]
      case .none:
        [
          MarkdownText.markdownLink("Transcript", file: transcript),
          MarkdownText.markdownLink("Tasks", file: tasks),
        ]
      }
    let when =
      "\(DateText.day(start, in: options.timeZone)) \(DateText.clock(start, in: options.timeZone))–\(DateText.clock(end, in: options.timeZone))"
    return ([when, Self.durationText(meeting.duration), Self.sourceLabel(meeting.source)] + links)
      .joined(separator: " · ")
  }

  /// Core renders sections as `## heading`; under `## Summary` they become
  /// `### heading`, the offset `SummaryMarkdown` leaves to adapters.
  static func demoted(_ summary: String) -> String {
    summary.split(separator: "\n", omittingEmptySubsequences: false)
      .map { $0.hasPrefix("#") ? "#" + $0 : String($0) }
      .joined(separator: "\n")
  }

  /// Whole minutes, rounded up.
  static func wholeMinutes(_ duration: TimeInterval) -> Int {
    guard duration.isFinite, duration > 0 else { return 0 }
    return Int((duration / 60).rounded(.up))
  }

  /// `1 h 30 min`, `2 h`, `45 min`.
  static func durationText(_ duration: TimeInterval) -> String {
    let minutes = wholeMinutes(duration)
    let hours = minutes / 60
    let rest = minutes % 60
    switch (hours, rest) {
    case (0, _): return "\(rest) min"
    case (_, 0): return "\(hours) h"
    default: return "\(hours) h \(rest) min"
    }
  }

  static func sourceKey(_ source: MeetingSource) -> String {
    switch source {
    case .macCall: "mac-call"
    case .macInPerson: "mac-in-person"
    case .phone: "phone"
    }
  }

  static func sourceLabel(_ source: MeetingSource) -> String {
    switch source {
    case .macCall: "Mac call"
    case .macInPerson: "In person"
    case .phone: "Phone"
    }
  }
}
