import StenoCore
import SwiftUI

/// The summary as `SummaryMarkdown.sections(for:)` renders it: one heading
/// per section with its bullets (inline Markdown, speaker names already
/// substituted), then the decisions. The tab never parses Markdown back;
/// inline styling (bold names) goes through `AttributedString`.
struct SummaryTab: View {
  let model: MeetingDetailViewModel

  var body: some View {
    ScrollView {
      VStack(alignment: .leading, spacing: Theme.Space.lg) {
        let sections = model.summarySections
        if sections.isEmpty {
          VStack(alignment: .leading, spacing: Theme.Space.sm) {
            PendingText(
              meeting: model.meeting, none: "No summary",
              pending: "Summary appears after processing")
            if let meeting = model.meeting, meeting.state == .queued || meeting.state == .processing
            {
              ProgressView().controlSize(.small)
            }
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
