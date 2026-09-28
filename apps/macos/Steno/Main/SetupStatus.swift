import Foundation
import StenoCore

/// Why the Summary and Tasks tabs have nothing to show, selected by
/// `MeetingDetailViewModel.summaryStatus`. A `.ready` meeting whose
/// `summary` is nil was processed without an LLM endpoint (the pipeline's
/// invariant); whether one exists now comes from `Settings`, so the row can
/// offer the fix or the re-run. Copy per the onboarding plan's "Meeting
/// detail" section and the redesign plan's states table.
enum SummaryStatus: Equatable, Sendable {
  /// Recording, queued, processing or failed without a summary: today's
  /// pending text.
  case pending
  /// The summary exists; the tabs render it (or "No summary" when the
  /// template produced no sections).
  case present
  /// Ready without a summary and no endpoint configured now.
  case skippedUnconfigured
  /// Ready without a summary; an endpoint exists now, so "Run summary" works.
  case skippedRunnable

  init(meeting: Meeting?, llmConfigured: Bool) {
    guard let meeting else {
      self = .pending
      return
    }
    if meeting.summary != nil {
      self = .present
    } else if meeting.state == .ready {
      self = llmConfigured ? .skippedRunnable : .skippedUnconfigured
    } else {
      self = .pending
    }
  }

  /// The empty-tab row for a skipped summary; nil for `pending` and
  /// `present`, which keep `PendingText`.
  func skippedRow(for tab: MeetingDetailViewModel.Tab) -> SkippedRow? {
    switch (self, tab) {
    case (.skippedUnconfigured, .summary):
      SkippedRow(title: "No summary", body: SetupCopy.summarySkipped, action: .setUpSummaries)
    case (.skippedRunnable, .summary):
      SkippedRow(
        title: "No summary yet", body: SetupCopy.summaryRunnable, action: .runSummary,
        footnote: SetupCopy.summaryRunnableFootnote)
    case (.skippedUnconfigured, .tasks):
      SkippedRow(title: "No tasks", body: SetupCopy.tasksSkipped, action: .setUpSummaries)
    case (.skippedRunnable, .tasks):
      SkippedRow(title: "No tasks", body: SetupCopy.tasksSkipped, action: .runSummary)
    case (.pending, _), (.present, _), (_, .transcript), (_, .scratchpad):
      nil
    }
  }

  /// What a tab shows for a skipped summary: the `EmptyState`'s title and
  /// body, its one action and, for the runnable Summary tab, the footnote.
  struct SkippedRow: Equatable, Sendable {
    enum Action: Equatable, Sendable {
      /// Opens Settings > LLM.
      case setUpSummaries
      /// Calls `rerunSummary()`.
      case runSummary

      var title: String {
        switch self {
        case .setUpSummaries: SetupCopy.setUpSummaries
        case .runSummary: SetupCopy.runSummary
        }
      }
    }

    var title: String
    var body: String
    var action: Action
    var footnote: String?

    /// The row as text lines, what `TabText` reports for the tab.
    var lines: [String] {
      [title, body] + (footnote.map { [$0] } ?? [])
    }
  }
}

/// What the detail footer shows, selected by
/// `MeetingDetailViewModel.exportStatus`. One vocabulary: "export", never
/// "delivered".
enum ExportStatus: Equatable, Sendable {
  /// No delivery rows and no vault: "Not exported: no Obsidian vault is
  /// configured." with "Choose a vault".
  case noVault
  /// No delivery rows, a vault exists: "Not exported yet." with "Export now".
  case notExported
  /// One badge per delivery row.
  case exported([Delivery])

  init(deliveries: [Delivery], vaultConfigured: Bool) {
    if !deliveries.isEmpty {
      self = .exported(deliveries)
    } else {
      self = vaultConfigured ? .notExported : .noVault
    }
  }
}

/// The onboarding plan's user-facing strings for the detail pane, in one
/// place so the tabs, the footer and `TabText` cannot drift.
enum SetupCopy {
  static let summarySkipped =
    "Summary skipped: no LLM endpoint is configured. The transcript is complete."
  static let summaryRunnable = "This meeting was processed before an LLM endpoint was configured."
  static let summaryRunnableFootnote = "Summary only; the transcript stays as recorded."
  static let tasksSkipped = "No tasks: the summary was skipped."
  static let setUpSummaries = "Set up summaries"
  static let runSummary = "Run summary"
  static let chooseVault = "Choose a vault"
  static let exportNow = "Export now"
  static let notExportedNoVault = "Not exported: no Obsidian vault is configured."
  static let notExportedYet = "Not exported yet."
  static let rerunHelp = "Set up an LLM endpoint in Settings > LLM first"
  static let reexportHelp = "Choose an Obsidian vault in Settings > Obsidian first"
}
