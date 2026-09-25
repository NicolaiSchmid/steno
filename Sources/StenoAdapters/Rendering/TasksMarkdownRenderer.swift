import Foundation
import StenoCore

/// The tasks note in Obsidian Tasks syntax. The plugin reads its fields from
/// the end of the line, so the order is fixed: description, assignee link,
/// tag, priority, due date. Emoji are bare code points: U+FE0F and NBSP
/// break recognition.
struct TasksMarkdownRenderer {
  let export: MeetingExport
  let options: RenderOptions

  static let highPriority = "\u{23EB}"  // ⏫
  static let lowPriority = "\u{1F53D}"  // 🔽
  static let dueMarker = "\u{1F4C5}"  // 📅
  static let closingLine = "Edit tasks in Steno; this file is rewritten on re-export."

  func render() -> String {
    var parts = ArtifactRenderer.noteHead(export, kind: "Tasks", options: options)
    if export.tasks.isEmpty {
      parts.append("No tasks.\n")
    } else {
      parts.append(export.tasks.map(line).joined(separator: "\n") + "\n")
    }
    parts.append(Self.closingLine + "\n")
    return parts.joined(separator: "\n")
  }

  /// `- [ ] Angebot an ACME schicken [[Anna Müller]] #task ⏫ 📅 2026-10-01`.
  func line(_ task: MeetingTask) -> String {
    var fields = ["- [\(task.done ? "x" : " ")] \(MarkdownText.singleLine(task.text))"]
    if let assignee = Names(export: export, options: options).assignee(task) {
      fields.append(MarkdownText.singleLine(assignee))
    }
    if let tag = options.taskTag.flatMap(MarkdownText.tag) {
      fields.append("#\(tag)")
    }
    switch task.priority {
    case .high: fields.append(Self.highPriority)
    case .low: fields.append(Self.lowPriority)
    case .normal: break
    }
    if let dueDate = task.dueDate {
      fields.append("\(Self.dueMarker) \(DateText.day(dueDate, in: options.timeZone))")
    }
    return fields.joined(separator: " ")
  }
}
