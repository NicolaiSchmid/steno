import StenoCore
import SwiftUI

/// The extracted tasks, read only: text, assignee, priority, due date. A
/// ready meeting the pipeline summarised without an endpoint shows the
/// `SummaryStatus` row instead, with the same action as the Summary tab.
struct TasksTab: View {
  let model: MeetingDetailViewModel
  let controller: AppController
  /// Where the pipeline is with this meeting while it is queued or
  /// processing, from `controller.progress.entry(for:)`; nil otherwise. The
  /// card it drives replaces the pending copy and the spinner.
  let progress: ProcessingProgressModel.Entry?

  var body: some View {
    let tasks = model.export?.tasks ?? []
    if tasks.isEmpty, let row = model.summaryStatus.skippedRow(for: .tasks) {
      SkippedSummaryState(row: row, tab: .tasks, model: model, controller: controller)
    } else {
      ScrollView {
        VStack(alignment: .leading, spacing: Theme.Space.sm) {
          ProcessingCardSlot(progress: progress, meeting: model.meeting)
          if tasks.isEmpty, progress == nil {
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
            StatusChip(text: assignee, style: .neutral)
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
    case .low: StatusChip(text: "Low", style: .neutral)
    }
  }
}
