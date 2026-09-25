import StenoCore
import SwiftUI

/// The extracted tasks, read only: text, assignee, priority, due date.
struct TasksTab: View {
  let model: MeetingDetailViewModel

  var body: some View {
    ScrollView {
      VStack(alignment: .leading, spacing: Theme.Space.sm) {
        let tasks = model.export?.tasks ?? []
        if tasks.isEmpty {
          PendingText(
            meeting: model.meeting, none: "No tasks", pending: "Tasks appear after processing")
        }
        ForEach(tasks) { task in
          TaskRow(task: task, assignee: model.export?.assigneeName(for: task))
        }
      }
      .readingColumn()
    }
  }
}

struct TaskRow: View {
  let task: MeetingTask
  let assignee: String?

  var body: some View {
    HStack(alignment: .firstTextBaseline, spacing: Theme.Space.sm) {
      Image(systemName: task.done ? "checkmark.square" : "square")
        .foregroundStyle(task.done ? Color.stenoLive : Color.stenoFaint)
      VStack(alignment: .leading, spacing: Theme.Space.xs) {
        Text(task.text)
          .font(.steno(Theme.TextSize.sm))
          .foregroundStyle(task.done ? Color.stenoMutedForeground : Color.stenoForeground)
          .strikethrough(task.done)
          .fixedSize(horizontal: false, vertical: true)
        HStack(spacing: Theme.Space.sm) {
          if let assignee, !assignee.isEmpty {
            StatusChip(text: assignee, color: Color.stenoMutedForeground)
          }
          priorityChip
          if let due = task.dueDate {
            Text(due, format: .dateTime.year().month(.abbreviated).day())
              .font(.steno(Theme.TextSize.xxs))
              .foregroundStyle(Color.stenoFaint)
          }
        }
      }
    }
    .padding(.vertical, Theme.Space.xs)
  }

  @ViewBuilder
  private var priorityChip: some View {
    switch task.priority {
    case .high: StatusChip(text: "High", color: Color.stenoWarning)
    case .normal: EmptyView()
    case .low: StatusChip(text: "Low", color: Color.stenoFaint)
    }
  }
}
