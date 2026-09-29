import StenoCore
import SwiftUI

/// The summary as `SummaryMarkdown.sections(for:)` renders it: one heading
/// per section with its bullets (inline Markdown, speaker names already
/// substituted), then the decisions. The tab never parses Markdown back;
/// inline styling (bold names) goes through `AttributedString`. A ready
/// meeting the pipeline summarised without an endpoint shows the
/// `SummaryStatus` row instead, with its fix or re-run; any other tab
/// without content shows the states table's row (`PendingText`), unless
/// the progress model has an entry, when the `ProcessingCard` stands in.
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
      if sections.isEmpty, progress == nil {
        PendingText(tab: .summary, model: model)
      } else {
        ScrollView {
          VStack(alignment: .leading, spacing: 0) {
            ProcessingCardSlot(progress: progress, meeting: model.meeting)
            ForEach(Array(sections.enumerated()), id: \.element.id) { index, section in
              SummarySectionView(
                heading: section.heading, bullets: section.bullets,
                isFirst: index == 0 && progress == nil)
            }
            if let decisions = model.export?.decisions, !decisions.isEmpty {
              SummarySectionView(
                heading: "Decisions", bullets: decisions.map(\.text),
                isFirst: sections.isEmpty && progress == nil)
            }
          }
          .readingColumn()
        }
      }
    }
  }
}

/// One section of the summary: the 16 pt semibold heading with 24 pt above
/// it (none for the first), then the bullets 8 pt apart.
struct SummarySectionView: View {
  let heading: String
  let bullets: [String]
  let isFirst: Bool

  var body: some View {
    VStack(alignment: .leading, spacing: Theme.Space.sm) {
      MarkdownBlockView(block: .heading(heading))
      ForEach(Array(bullets.enumerated()), id: \.offset) { _, bullet in
        MarkdownBlockView(block: .bullet(bullet))
      }
    }
    .padding(.top, isFirst ? 0 : Theme.Space.xl)
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

/// A heading at 16 semibold `strong`, or a bullet: a 4 pt `faint` dot
/// hung 8 pt from the top of 14/19 prose, so it sits on the first line's
/// x-height instead of a glyph at the baseline.
struct MarkdownBlockView: View {
  let block: MarkdownBlocks.Block

  /// The bullet dot and where it hangs; the plan's 4 pt at 8 pt.
  static let bulletSize: CGFloat = 4
  static let bulletOffset: CGFloat = 8

  var body: some View {
    switch block {
    case .heading(let text):
      Text(text)
        .font(.steno(Theme.TextSize.base, weight: .semibold))
        .foregroundStyle(Color.stenoStrong)
    case .bullet(let text):
      HStack(alignment: .top, spacing: Theme.Space.sm) {
        Circle()
          .fill(Color.stenoFaint)
          .frame(width: Self.bulletSize, height: Self.bulletSize)
          .padding(.top, Self.bulletOffset)
          .accessibilityHidden(true)
        Text(MarkdownBlocks.inline(text))
          .font(.steno(Theme.TextSize.sm))
          .proseLeading()
          .foregroundStyle(Color.stenoForeground)
          .fixedSize(horizontal: false, vertical: true)
      }
    }
  }
}
