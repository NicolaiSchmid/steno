import StenoCore
import SwiftUI

/// The summary as `SummaryMarkdown.sections(for:)` renders it: one heading
/// per section with its bullets (inline Markdown, speaker names already
/// substituted), then the decisions. The tab never parses Markdown back;
/// inline styling (bold names) goes through `AttributedString`. A ready
/// meeting the pipeline summarised without an endpoint shows the
/// `SummaryStatus` row instead, with its fix or re-run.
struct SummaryTab: View {
  let model: MeetingDetailViewModel
  let controller: AppController
  /// Where the pipeline is with this meeting while it is queued or
  /// processing, from `controller.progress.entry(for:)`; nil otherwise. The
  /// card it drives replaces the pending copy and the spinner.
  let progress: ProcessingProgressModel.Entry?

  var body: some View {
    let sections = model.summarySections
    if sections.isEmpty, let row = model.summaryStatus.skippedRow(for: .summary) {
      SkippedSummaryState(row: row, tab: .summary, model: model, controller: controller)
    } else {
      ScrollView {
        VStack(alignment: .leading, spacing: Theme.Space.lg) {
          ProcessingCardSlot(progress: progress, meeting: model.meeting)
          if sections.isEmpty {
            if progress == nil {
              PendingText(
                meeting: model.meeting, none: "No summary",
                pending: "Summary appears after processing")
            }
          } else {
            ForEach(sections, id: \.id) { section in
              VStack(alignment: .leading, spacing: Theme.Space.sm) {
                MarkdownBlockView(block: .heading(section.heading))
                ForEach(Array(section.bullets.enumerated()), id: \.offset) { _, bullet in
                  MarkdownBlockView(block: .bullet(bullet))
                }
              }
            }
            if let decisions = model.export?.decisions, !decisions.isEmpty {
              VStack(alignment: .leading, spacing: Theme.Space.sm) {
                MarkdownBlockView(block: .heading("Decisions"))
                ForEach(decisions) { decision in
                  MarkdownBlockView(block: .bullet(decision.text))
                }
              }
            }
          }
        }
        .readingColumn()
      }
    }
  }
}

/// The Summary and Tasks tabs' row for a ready meeting without a summary:
/// an `EmptyState` whose action opens Settings > Summaries while no endpoint
/// is configured, or runs the summary once one is. Ids
/// `<tab>-setup-summaries` and `<tab>-run-summary`; the copy is `SetupCopy`'s.
struct SkippedSummaryState: View {
  let row: SummaryStatus.SkippedRow
  let tab: MeetingDetailViewModel.Tab
  let model: MeetingDetailViewModel
  let controller: AppController
  @Environment(\.openSettings) private var openSettings

  var body: some View {
    EmptyState(
      title: row.title, body: row.body, action: action, footnote: row.footnote,
      id: "empty-\(tab.rawValue)")
  }

  private var action: EmptyState.Action {
    switch row.action {
    case .setUpSummaries:
      EmptyState.Action(title: row.action.title, id: "\(tab.rawValue)-setup-summaries") {
        controller.openSettings(.summaries, with: openSettings)
      }
    case .runSummary:
      EmptyState.Action(
        title: row.action.title, id: "\(tab.rawValue)-run-summary",
        isEnabled: model.canRerunSummary && !model.isBusy
      ) {
        Task { await model.rerunSummary() }
      }
    }
  }
}

/// The two block shapes the summary is made of, and the inline renderer.
enum MarkdownBlocks {
  enum Block: Equatable {
    case heading(String)
    case bullet(String)
  }

  static func inline(_ text: String) -> AttributedString {
    (try? AttributedString(
      markdown: text,
      options: AttributedString.MarkdownParsingOptions(
        interpretedSyntax: .inlineOnlyPreservingWhitespace)))
      ?? AttributedString(text)
  }
}

struct MarkdownBlockView: View {
  let block: MarkdownBlocks.Block

  var body: some View {
    switch block {
    case .heading(let text):
      Text(text)
        .font(.steno(Theme.TextSize.base, weight: .semibold))
        .foregroundStyle(Color.stenoStrong)
        .padding(.top, Theme.Space.xs)
    case .bullet(let text):
      HStack(alignment: .firstTextBaseline, spacing: Theme.Space.sm) {
        Text("•").foregroundStyle(Color.stenoFaint)
        Text(MarkdownBlocks.inline(text))
          .font(.steno(Theme.TextSize.sm))
          .foregroundStyle(Color.stenoForeground)
          .fixedSize(horizontal: false, vertical: true)
      }
    }
  }
}
