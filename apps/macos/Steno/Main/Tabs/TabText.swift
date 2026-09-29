import Foundation
import StenoCore

/// The four tabs as plain text lines, composed from the same pure pieces the
/// tab views lay out: `MarkdownBlocks` and the inline renderer (Summary),
/// `TranscriptTurns`, `MeetingExport.displayName(forSpeaker:)` and
/// `timestampText` (Transcript), `MeetingExport.assigneeName(for:)` and the
/// priority chips (Tasks), the meeting's scratchpad (Scratchpad), the
/// `ProcessingCard`'s title row through `ProcessingPresentation.lines` while
/// `progress` says the meeting is queued or processing, the `SummaryStatus`
/// skipped row (title, body, footnote) for a ready meeting the pipeline
/// summarised without an endpoint (`llmConfigured` is the setup state it
/// keys off), and the `TabPlaceholder` row (title, body) for a tab without
/// content otherwise. The views add styling only; the snapshot test pins
/// these lines for the fixture meeting.
enum TabText {
  static func lines(
    _ tab: MeetingDetailViewModel.Tab, export: MeetingExport,
    progress: ProcessingProgressModel.Entry? = nil, elapsed: Duration = .zero,
    llmConfigured: Bool = false, locale: Locale = .current, timeZone: TimeZone = .current
  ) -> [String] {
    switch tab {
    case .summary:
      summary(export, progress: progress, elapsed: elapsed, llmConfigured: llmConfigured)
    case .transcript: transcript(export, progress: progress, elapsed: elapsed)
    case .tasks:
      tasks(
        export, progress: progress, elapsed: elapsed, llmConfigured: llmConfigured, locale: locale,
        timeZone: timeZone)
    case .scratchpad: card(progress, elapsed: elapsed) + [export.meeting.scratchpad]
    }
  }

  /// The skipped row's lines for a ready meeting without a summary; nil
  /// otherwise.
  private static func skippedLines(
    _ tab: MeetingDetailViewModel.Tab, export: MeetingExport, llmConfigured: Bool
  ) -> [String]? {
    SummaryStatus(meeting: export.meeting, llmConfigured: llmConfigured).skippedRow(for: tab)?
      .lines
  }

  /// The card's lines while the meeting is queued or processing, else none;
  /// every tab shows the card above whatever content it has.
  private static func card(_ progress: ProcessingProgressModel.Entry?, elapsed: Duration)
    -> [String]
  {
    progress.map { ProcessingPresentation.lines(entry: $0, elapsed: elapsed) } ?? []
  }

  /// A tab without content: the card's lines while the meeting is queued or
  /// processing, else the states table's row for the meeting's state.
  private static func pending(
    _ tab: MeetingDetailViewModel.Tab, _ export: MeetingExport,
    progress: ProcessingProgressModel.Entry?, elapsed: Duration
  ) -> [String] {
    if let progress { return ProcessingPresentation.lines(entry: progress, elapsed: elapsed) }
    return TabPlaceholder(tab: tab, state: export.meeting.state).lines
  }

  private static func summary(
    _ export: MeetingExport, progress: ProcessingProgressModel.Entry?, elapsed: Duration,
    llmConfigured: Bool
  ) -> [String] {
    let sections = SummaryMarkdown.sections(for: export)
    guard !sections.isEmpty else {
      if let skipped = skippedLines(.summary, export: export, llmConfigured: llmConfigured) {
        return skipped
      }
      return pending(.summary, export, progress: progress, elapsed: elapsed)
    }
    var lines = card(progress, elapsed: elapsed)
    lines += sections.flatMap { section in
      [line(for: .heading(section.heading))] + section.bullets.map { line(for: .bullet($0)) }
    }
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
    }
  }

  private static func transcript(
    _ export: MeetingExport, progress: ProcessingProgressModel.Entry?, elapsed: Duration
  ) -> [String] {
    let turns = TranscriptTurns.group(export.segments)
    guard !turns.isEmpty else {
      return pending(.transcript, export, progress: progress, elapsed: elapsed)
    }
    let rows: [String] = turns.flatMap { turn in
      let name = turn.speakerID.map(export.displayName(forSpeaker:)) ?? "Unknown"
      return ["\(name) \(turn.start.timestampText) \(turn.lane.label)", turn.text]
    }
    return card(progress, elapsed: elapsed) + rows
  }

  private static func tasks(
    _ export: MeetingExport, progress: ProcessingProgressModel.Entry?, elapsed: Duration,
    llmConfigured: Bool, locale: Locale, timeZone: TimeZone
  ) -> [String] {
    guard !export.tasks.isEmpty else {
      if let skipped = skippedLines(.tasks, export: export, llmConfigured: llmConfigured) {
        return skipped
      }
      return pending(.tasks, export, progress: progress, elapsed: elapsed)
    }
    let rows: [String] = export.tasks.flatMap { task in
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
    return card(progress, elapsed: elapsed) + rows
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
