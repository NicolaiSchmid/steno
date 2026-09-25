import Foundation
import StenoCore

/// The four tabs as plain text lines, composed from the same pure pieces the
/// tab views lay out: `MarkdownBlocks` and the inline renderer (Summary),
/// `TranscriptTurns`, `MeetingExport.displayName(forSpeaker:)` and
/// `timestampText` (Transcript), `MeetingExport.assigneeName(for:)` and the
/// priority chips (Tasks), the meeting's scratchpad (Scratchpad), and
/// `PendingText.text` for a tab without content. The views add styling only;
/// the snapshot test pins these lines for the fixture meeting.
enum TabText {
  static func lines(
    _ tab: MeetingDetailViewModel.Tab, export: MeetingExport,
    locale: Locale = .current, timeZone: TimeZone = .current
  ) -> [String] {
    switch tab {
    case .summary: summary(export)
    case .transcript: transcript(export)
    case .tasks: tasks(export, locale: locale, timeZone: timeZone)
    case .scratchpad: [export.meeting.scratchpad]
    }
  }

  private static func summary(_ export: MeetingExport) -> [String] {
    let markdown = SummaryMarkdown.render(export)
    guard !markdown.isEmpty else {
      return [
        PendingText.text(
          meeting: export.meeting, none: "No summary", pending: "Summary appears after processing")
      ]
    }
    var lines = MarkdownBlocks.parse(markdown).map(line(for:))
    if !export.decisions.isEmpty {
      lines.append("Decisions")
      lines.append(contentsOf: export.decisions.map { line(for: .bullet($0.text)) })
    }
    return lines
  }

  private static func line(for block: MarkdownBlocks.Block) -> String {
    switch block {
    case .heading(let text): text
    case .bullet(let text): "• " + String(MarkdownBlocks.inline(text).characters)
    case .paragraph(let text): String(MarkdownBlocks.inline(text).characters)
    }
  }

  private static func transcript(_ export: MeetingExport) -> [String] {
    let turns = TranscriptTurns.group(export.segments)
    guard !turns.isEmpty else {
      return [
        PendingText.text(
          meeting: export.meeting, none: "No transcript",
          pending: "Transcript appears after processing")
      ]
    }
    return turns.flatMap { turn in
      let name = turn.speakerID.map(export.displayName(forSpeaker:)) ?? "Unknown"
      return ["\(name) \(turn.start.timestampText) \(turn.lane.label)", turn.text]
    }
  }

  private static func tasks(_ export: MeetingExport, locale: Locale, timeZone: TimeZone)
    -> [String]
  {
    guard !export.tasks.isEmpty else {
      return [
        PendingText.text(
          meeting: export.meeting, none: "No tasks", pending: "Tasks appear after processing")
      ]
    }
    return export.tasks.flatMap { task in
      var meta: [String] = []
      if let assignee = export.assigneeName(for: task), !assignee.isEmpty {
        meta.append(assignee)
      }
      switch task.priority {
      case .high: meta.append("High")
      case .normal: break
      case .low: meta.append("Low")
      }
      if let due = task.dueDate {
        meta.append(
          due.formatted(
            Date.FormatStyle(date: .abbreviated, time: .omitted, locale: locale, timeZone: timeZone)
          ))
      }
      return ["\(task.done ? "[x]" : "[ ]") \(task.text)", meta.joined(separator: " · ")]
    }
  }
}

extension MeetingExport {
  /// The assignee as the Tasks tab shows it: the linked person's current
  /// name, else the name the LLM extracted.
  func assigneeName(for task: MeetingTask) -> String? {
    if let personID = task.assigneePersonID, let person = person(id: personID) {
      return person.displayName
    }
    return task.assigneeName
  }
}
