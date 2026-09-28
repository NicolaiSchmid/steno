import SwiftUI

/// The empty state of a pane or a list: a 48 pt icon well on the `card`
/// veil, a 14 pt medium title, a 13 pt `muted` body wrapping at 280 pt, an
/// optional secondary action 16 pt below and an optional `faint` footnote
/// 12 pt under the action. Without a symbol it is the quieter "no content"
/// variant the ready tabs use. Copy and ids are the caller's; the plan's
/// states table lists them.
struct EmptyState: View {
  struct Action {
    let title: String
    let id: String
    /// Disables the button alone; the title and body stay readable.
    let isEnabled: Bool
    let run: () -> Void

    init(title: String, id: String, isEnabled: Bool = true, run: @escaping () -> Void) {
      self.title = title
      self.id = id
      self.isEnabled = isEnabled
      self.run = run
    }
  }

  let symbol: String?
  let title: String
  let message: String
  let action: Action?
  /// One `faint` line under the action, for a caveat the action needs
  /// ("Summary only; the transcript stays as recorded.").
  let footnote: String?
  let id: String
  /// The width the body wraps at; a layout width, not a control box.
  private static let bodyWidth: CGFloat = 280

  init(
    symbol: String? = nil, title: String, body message: String, action: Action? = nil,
    footnote: String? = nil, id: String
  ) {
    self.symbol = symbol
    self.title = title
    self.message = message
    self.action = action
    self.footnote = footnote
    self.id = id
  }

  var body: some View {
    VStack(spacing: Theme.Space.md) {
      if let symbol {
        Image(systemName: symbol)
          .font(.system(size: Theme.Control.emptySymbolSize))
          .foregroundStyle(Color.stenoFaint)
          .frame(width: Theme.Control.emptyWellSize, height: Theme.Control.emptyWellSize)
          .background(Theme.Radius.lg.shape.fill(Color.stenoCard))
          .overlay(Theme.Radius.lg.shape.hairline())
          .accessibilityHidden(true)
      }
      Text(title)
        .font(.steno(Theme.TextSize.sm, weight: .medium))
        .foregroundStyle(Color.stenoStrong)
      Text(message)
        .font(.steno(Theme.TextSize.xs))
        .foregroundStyle(Color.stenoMutedForeground)
        .multilineTextAlignment(.center)
        .frame(maxWidth: Self.bodyWidth)
        .fixedSize(horizontal: false, vertical: true)
      if let action {
        Button(action.title, action: action.run)
          .buttonStyle(StenoSecondaryButtonStyle())
          .disabled(!action.isEnabled)
          .accessibilityIdentifier(action.id)
          .padding(.top, Theme.Space.xs)
      }
      if let footnote {
        Text(footnote)
          .font(.steno(Theme.TextSize.xxs))
          .foregroundStyle(Color.stenoFaint)
          .multilineTextAlignment(.center)
          .frame(maxWidth: Self.bodyWidth)
          .fixedSize(horizontal: false, vertical: true)
      }
    }
    .frame(maxWidth: .infinity, maxHeight: .infinity)
    .accessibilityElement(children: .contain)
    .accessibilityIdentifier(id)
  }
}

#if DEBUG
  #Preview("Empty states") {
    PreviewPair {
      VStack(spacing: Theme.Space.xl) {
        EmptyState(
          symbol: "waveform", title: "No meetings yet",
          body: "Record a call or an in-person meeting and it appears here.",
          id: "empty-meetings")
        EmptyState(
          symbol: "exclamationmark.triangle", title: "Processing failed",
          body: "The LLM endpoint did not answer.",
          action: .init(title: "Try again", id: "empty-retry") {}, id: "empty-detail")
        EmptyState(
          title: "No summary", body: "The template produced no sections.", id: "empty-summary")
      }
    }
    .frame(width: 720, height: 560)
  }
#endif
