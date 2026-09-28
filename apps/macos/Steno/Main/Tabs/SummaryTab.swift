import StenoCore
import SwiftUI

/// The summary as `SummaryMarkdown.sections(for:)` renders it: one heading
/// per section with its bullets (inline Markdown, speaker names already
/// substituted), then the decisions. The tab never parses Markdown back;
/// inline styling (bold names) goes through `AttributedString`.
struct SummaryTab: View {
  let model: MeetingDetailViewModel
  /// Where the pipeline is with this meeting while it is queued or
  /// processing, from `controller.progress.entry(for:)`; nil otherwise. The
  /// card it drives replaces the pending copy and the spinner.
  let progress: ProcessingProgressModel.Entry?

  var body: some View {
    ScrollView {
      VStack(alignment: .leading, spacing: Theme.Space.lg) {
        if let progress, let meeting = model.meeting {
          ProcessingCard(entry: progress, meeting: meeting)
        }
        let sections = model.summarySections
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
