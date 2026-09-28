import SwiftUI

/// The empty state of a pane or a list: a 48 pt icon well on the `card`
/// veil, a 14 pt medium title, a 13 pt `muted` body wrapping at 280 pt, and
/// an optional secondary action 16 pt below. Without a symbol it is the
/// quieter "no content" variant the ready tabs use. Copy and ids are the
/// caller's; the plan's states table lists them.
struct EmptyState: View {
  struct Action {
    let title: String
    let id: String
    let run: () -> Void

    init(title: String, id: String, run: @escaping () -> Void) {
      self.title = title
      self.id = id
      self.run = run
    }
  }

  let symbol: String?
  let title: String
  let message: String
  let action: Action?
  let id: String

  init(
    symbol: String? = nil, title: String, body message: String, action: Action? = nil, id: String
  ) {
    self.symbol = symbol
    self.title = title
    self.message = message
    self.action = action
    self.id = id
  }

  var body: some View {
    VStack(spacing: Theme.Space.md) {
      if let symbol {
        Image(systemName: symbol)
          .font(.system(size: 20))
          .foregroundStyle(Color.stenoFaint)
          .frame(width: Theme.Control.emptyWellSize, height: Theme.Control.emptyWellSize)
          .background(Theme.Radius.lg.shape.fill(Color.stenoCard))
          .overlay(
            Theme.Radius.lg.shape.strokeBorder(Color.stenoBorder, lineWidth: Theme.Space.hairline)
          )
          .accessibilityHidden(true)
      }
      Text(title)
        .font(.steno(Theme.TextSize.sm, weight: .medium))
        .foregroundStyle(Color.stenoStrong)
      Text(message)
        .font(.steno(Theme.TextSize.xs))
        .foregroundStyle(Color.stenoMutedForeground)
        .multilineTextAlignment(.center)
        .frame(maxWidth: Theme.Control.emptyBodyWidth)
        .fixedSize(horizontal: false, vertical: true)
      if let action {
        Button(action.title, action: action.run)
          .buttonStyle(StenoSecondaryButtonStyle())
          .accessibilityIdentifier(action.id)
          .padding(.top, Theme.Space.xs)
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
