import StenoCore
import SwiftUI

/// The summary as `SummaryMarkdown.render` produces it: `## heading`
/// sections with `- **lead**: text` bullets, plus decisions. Inline markdown
/// (bold names) renders through `AttributedString`; block structure is laid
/// out here because SwiftUI's `Text` renders inline styles only.
struct SummaryTab: View {
  let model: MeetingDetailViewModel

  var body: some View {
    ScrollView {
      VStack(alignment: .leading, spacing: Theme.Space.lg) {
        if model.summaryMarkdown.isEmpty {
          placeholder
        } else {
          ForEach(Array(MarkdownBlocks.parse(model.summaryMarkdown).enumerated()), id: \.offset) {
            _, block in
            MarkdownBlockView(block: block)
          }
          if let decisions = model.export?.decisions, !decisions.isEmpty {
            VStack(alignment: .leading, spacing: Theme.Space.sm) {
              Text("Decisions")
                .font(.steno(Theme.TextSize.base, weight: .semibold))
                .foregroundStyle(Color.stenoStrong)
              ForEach(decisions) { decision in
                MarkdownBlockView(block: .bullet(decision.text))
              }
            }
          }
        }
      }
      .frame(maxWidth: 720, alignment: .leading)
      .padding(Theme.Space.lg)
      .textSelection(.enabled)
    }
  }

  private var placeholder: some View {
    VStack(alignment: .leading, spacing: Theme.Space.sm) {
      Text(model.meeting?.state == .ready ? "No summary" : "Summary appears after processing")
        .font(.steno(Theme.TextSize.sm, weight: .medium))
        .foregroundStyle(Color.stenoMutedForeground)
      if let meeting = model.meeting, meeting.state == .queued || meeting.state == .processing {
        ProgressView().controlSize(.small)
      }
    }
  }
}

/// The subset of Markdown `SummaryMarkdown` emits, as blocks.
enum MarkdownBlocks {
  enum Block: Equatable {
    case heading(String)
    case bullet(String)
    case paragraph(String)
  }

  static func parse(_ markdown: String) -> [Block] {
    var blocks: [Block] = []
    for rawLine in markdown.split(separator: "\n", omittingEmptySubsequences: true) {
      let line = String(rawLine)
      if line.hasPrefix("## ") {
        blocks.append(.heading(String(line.dropFirst(3))))
      } else if line.hasPrefix("# ") {
        blocks.append(.heading(String(line.dropFirst(2))))
      } else if line.hasPrefix("- ") {
        blocks.append(.bullet(String(line.dropFirst(2))))
      } else if !line.trimmingCharacters(in: .whitespaces).isEmpty {
        blocks.append(.paragraph(line))
      }
    }
    return blocks
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
    case .paragraph(let text):
      Text(MarkdownBlocks.inline(text))
        .font(.steno(Theme.TextSize.sm))
        .foregroundStyle(Color.stenoForeground)
        .fixedSize(horizontal: false, vertical: true)
    }
  }
}
