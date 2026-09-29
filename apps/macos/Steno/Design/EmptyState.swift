import SwiftUI

/// The empty state of a pane or a list: a 48 pt icon well on the `card`
/// veil, a 14 pt medium title, a 13 pt `muted` body wrapping at 280 pt, an
/// optional small spinner under the body (the processing row), an optional
/// secondary action 16 pt below and an optional `faint` footnote 12 pt under
/// the action. Without a symbol it is the quieter "no content" variant the
/// ready tabs use. Copy and ids are the caller's; the plan's states table
/// lists them. The title carries `<id>-title`, because a container id is
/// not queryable from XCUITest on macOS and the title is what a smoke test
/// reads to know which row is showing.
struct EmptyState: View {
  struct Action {
    let title: String
    let id: String
    /// Disables the button alone; the title and body stay readable.
    let isEnabled: Bool
    /// The button's help, for the reason a disabled action cannot run
    /// ("Set up an LLM endpoint in Settings > Summaries first").
    let help: String?
    let run: () -> Void

    init(
      title: String, id: String, isEnabled: Bool = true, help: String? = nil,
      run: @escaping () -> Void
    ) {
      self.title = title
      self.id = id
      self.isEnabled = isEnabled
      self.help = help
      self.run = run
    }
  }

  let symbol: String?
  let title: String
  let message: String
  /// A small indeterminate spinner under the body while work is under way.
  let showsSpinner: Bool
  let action: Action?
  /// One `faint` line under the action, for a caveat the action needs
  /// ("Summary only; the transcript stays as recorded.").
  let footnote: String?
  let id: String
  /// The width the body and the footnote wrap at; a layout width, not a
  /// control box. Fixed, not a maximum: with `fixedSize(vertical:)` a
  /// `maxWidth` answers the minimum-size probe (width 0) with one character
  /// per line, and in a split view's detail column that minimum becomes the
  /// window's, which then grows past the screen (the hosted runner's 1024 x
  /// 768 display is the budget; `EmptyStateLayoutTests` pins the minimum).
  static let bodyWidth: CGFloat = 280

  init(
    symbol: String? = nil, title: String, body message: String, showsSpinner: Bool = false,
    action: Action? = nil, footnote: String? = nil, id: String
  ) {
    self.symbol = symbol
    self.title = title
    self.message = message
    self.showsSpinner = showsSpinner
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
        .accessibilityIdentifier("\(id)-title")
      Text(message)
        .font(.steno(Theme.TextSize.xs))
        .foregroundStyle(Color.stenoMutedForeground)
        .multilineTextAlignment(.center)
        .frame(width: Self.bodyWidth)
        .fixedSize(horizontal: false, vertical: true)
      if showsSpinner {
        ProgressView().controlSize(.small)
      }
      if let action {
        Button(action.title, action: action.run)
          .buttonStyle(StenoSecondaryButtonStyle())
          .disabled(!action.isEnabled)
          .help(action.help ?? "")
          .accessibilityIdentifier(action.id)
          .padding(.top, Theme.Space.xs)
      }
      if let footnote {
        Text(footnote)
          .font(.steno(Theme.TextSize.xxs))
          .foregroundStyle(Color.stenoFaint)
          .multilineTextAlignment(.center)
          .frame(width: Self.bodyWidth)
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
          symbol: "waveform.badge.magnifyingglass", title: "Transcribing",
          body: "Audio stays on this Mac. This usually takes a minute or two.",
          showsSpinner: true, id: "empty-processing")
        EmptyState(
          symbol: "exclamationmark.triangle", title: "Processing failed",
          body: "The LLM endpoint did not answer.",
          action: .init(title: "Try again", id: "empty-retry") {}, id: "empty-detail")
        EmptyState(
          title: "No summary", body: "The template produced no sections.", id: "empty-summary")
      }
    }
    .frame(width: 720, height: 720)
  }
#endif
