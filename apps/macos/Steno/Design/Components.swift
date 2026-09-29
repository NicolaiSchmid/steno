import StenoCore
import SwiftUI

/// The composed controls the app reuses, built from the tokens: the three
/// button styles, the status chip, the raised card, the status dot and the
/// message row. Boxes, fills and type follow the redesign plan's components
/// table; every state swap runs over `Motion.functional` and holds still
/// under Reduce Motion. `Design/Controls.swift` holds the nav row, the
/// icon button, the fields and the segmented tabs; `Design/EmptyState.swift`
/// the empty state.

extension LinearGradient {
  /// The achromatic CTA fill: `accent-from` over `accent-to`, top to bottom.
  static var stenoAccent: LinearGradient {
    LinearGradient(
      colors: [Color.stenoAccentFrom, Color.stenoAccentTo], startPoint: .top, endPoint: .bottom)
  }
}

/// Press and disabled feedback shared by the two button styles: a 2 % scale
/// and a 5 % dim while pressed, half opacity while disabled.
private struct PressFeedback: ViewModifier {
  let isPressed: Bool
  @Environment(\.isEnabled) private var isEnabled
  @Environment(\.accessibilityReduceMotion) private var reduceMotion

  func body(content: Content) -> some View {
    content
      .scaleEffect(isPressed ? Motion.controlPressScale : 1)
      .opacity(isPressed ? Motion.controlPressOpacity : 1)
      .opacity(isEnabled ? 1 : Motion.disabledOpacity)
      .animation(Motion.swap(reduceMotion: reduceMotion), value: isPressed)
  }
}

/// The primary action: 32 pt tall, radius 8, the accent gradient with
/// `on-accent` text, no border.
struct StenoPrimaryButtonStyle: ButtonStyle {
  func makeBody(configuration: Configuration) -> some View {
    configuration.label
      .font(.steno(Theme.TextSize.sm, weight: .medium))
      .foregroundStyle(Color.stenoOnAccent)
      .padding(.horizontal, Theme.Control.buttonInset)
      .frame(height: Theme.Control.buttonHeight)
      .background(Theme.Radius.md.shape.fill(LinearGradient.stenoAccent))
      .contentShape(Theme.Radius.md.shape)
      .modifier(PressFeedback(isPressed: configuration.isPressed))
  }
}

/// The secondary action: the same box on a `raised` surface with a hairline,
/// the `card` veil on hover while enabled. `height` defaults to the button
/// height; the floating bubble passes its 28 pt control height.
struct StenoSecondaryButtonStyle: ButtonStyle {
  var height: CGFloat = Theme.Control.buttonHeight

  func makeBody(configuration: Configuration) -> some View {
    Surface(configuration: configuration, height: height)
  }

  private struct Surface: View {
    let configuration: Configuration
    let height: CGFloat
    @State private var hovering = false
    @Environment(\.isEnabled) private var isEnabled
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    var body: some View {
      configuration.label
        .font(.steno(Theme.TextSize.sm))
        .foregroundStyle(Color.stenoStrong)
        .padding(.horizontal, Theme.Control.buttonInset)
        .frame(height: height)
        .background(
          ZStack {
            Theme.Radius.md.shape.fill(Color.stenoRaised)
            Theme.Radius.md.shape.fill(hovering && isEnabled ? Color.stenoCard : Color.clear)
          }
        )
        .overlay(Theme.Radius.md.shape.hairline())
        .contentShape(Theme.Radius.md.shape)
        .onHover { hovering = $0 }
        .animation(Motion.swap(reduceMotion: reduceMotion), value: hovering)
        .modifier(PressFeedback(isPressed: configuration.isPressed))
    }
  }
}

/// The ghost action: a bare 13 pt label in `faint`, `strong` on hover, no
/// box, the button height as the hit height. "Skip" on the onboarding rows,
/// "Not now" on the setup banner and the detail footer's text actions.
/// `tint` is the resting colour; the menu bar footer passes `muted`.
struct StenoGhostButtonStyle: ButtonStyle {
  var tint: Color = Color.stenoFaint

  func makeBody(configuration: Configuration) -> some View {
    Ghost(configuration: configuration, tint: tint)
  }

  private struct Ghost: View {
    let configuration: Configuration
    let tint: Color
    @State private var hovering = false
    @Environment(\.isEnabled) private var isEnabled
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    var body: some View {
      configuration.label
        .font(.steno(Theme.TextSize.xs))
        .foregroundStyle(hovering && isEnabled ? Color.stenoStrong : tint)
        .frame(height: Theme.Control.buttonHeight)
        .contentShape(Rectangle())
        .onHover { hovering = $0 }
        .animation(Motion.swap(reduceMotion: reduceMotion), value: hovering)
        .modifier(PressFeedback(isPressed: configuration.isPressed))
    }
  }
}

/// The one 6 pt status dot: the Stop control, the message rows and the
/// list entry share it.
struct StatusDot: View {
  var color: Color

  var body: some View {
    Circle()
      .fill(color)
      .frame(width: 6, height: 6)
  }
}

/// A small chip, radius 6. Semantic state gets the colour at 12 % with the
/// text in the colour; neutral metadata (tags, assignees, "Optional") gets a
/// hairline and `muted` text, so only state reads as state.
struct StatusChip: View {
  enum Style: Equatable {
    case semantic(Color)
    case neutral
  }

  let text: String
  let style: Style
  let systemImage: String?

  init(text: String, style: Style, systemImage: String? = nil) {
    self.text = text
    self.style = style
    self.systemImage = systemImage
  }

  init(text: String, color: Color, systemImage: String? = nil) {
    self.init(text: text, style: .semantic(color), systemImage: systemImage)
  }

  private var foreground: Color {
    switch style {
    case .semantic(let color): color
    case .neutral: Color.stenoMutedForeground
    }
  }

  private var fill: Color {
    switch style {
    case .semantic(let color): color.opacity(0.12)
    case .neutral: Color.clear
    }
  }

  private var isNeutral: Bool {
    if case .neutral = style { return true }
    return false
  }

  var body: some View {
    HStack(spacing: Theme.Space.xs) {
      if let systemImage {
        Image(systemName: systemImage)
          .font(.system(size: Theme.Control.chipGlyphSize, weight: .medium))
      }
      Text(text)
    }
    .font(.steno(Theme.TextSize.xxxs, weight: .medium))
    .foregroundStyle(foreground)
    .padding(.horizontal, Theme.Control.chipInset)
    .padding(.vertical, Theme.Space.hairline)
    .background(Theme.Radius.sm.shape.fill(fill))
    .overlay(Theme.Radius.sm.shape.hairline(isNeutral ? Color.stenoBorder : Color.clear))
  }
}

/// A raised surface with a hairline border, radius 12, no shadow.
struct Card<Content: View>: View {
  var padding: CGFloat
  @ViewBuilder var content: () -> Content

  init(padding: CGFloat = Theme.Space.lg, @ViewBuilder content: @escaping () -> Content) {
    self.padding = padding
    self.content = content
  }

  var body: some View {
    content()
      .padding(padding)
      .background(Theme.Radius.lg.shape.fill(Color.stenoRaised))
      .overlay(Theme.Radius.lg.shape.hairline())
  }
}

struct SectionLabel: View {
  var text: String

  var body: some View {
    Text(text.uppercased())
      .font(.steno(Theme.TextSize.xxxs, weight: .semibold))
      .foregroundStyle(Color.stenoFaint)
      .tracking(0.6)
  }
}

/// An inline message row for errors, warnings and notes: the status dot,
/// 13 pt body text, the colour at 8 % behind, radius 8.
struct MessageRow: View {
  enum Kind {
    case error
    case warning
    case info
    case success
  }

  var kind: Kind
  var text: String

  private var color: Color {
    switch kind {
    case .error: Color.stenoDestructive
    case .warning: Color.stenoWarning
    case .info: Color.stenoInfo
    case .success: Color.stenoLive
    }
  }

  var body: some View {
    HStack(alignment: .top, spacing: Theme.Space.sm) {
      StatusDot(color: color).padding(.top, 5)
      Text(text)
        .font(.steno(Theme.TextSize.xs))
        .foregroundStyle(Color.stenoForeground)
        .textSelection(.enabled)
    }
    .padding(.vertical, Theme.Space.sm)
    .padding(.horizontal, Theme.Control.rowInset)
    .frame(maxWidth: .infinity, alignment: .leading)
    .background(Theme.Radius.md.shape.fill(color.opacity(0.08)))
  }
}

extension StatusChip {
  /// How a `MeetingState` reads in the list, the header and the queue.
  init(_ state: MeetingState) {
    switch state {
    case .recording: self.init(text: "Recording", color: Color.stenoLiveBright)
    case .queued: self.init(text: "Queued", color: Color.stenoInfo)
    case .processing: self.init(text: "Processing", color: Color.stenoInfo)
    case .ready: self.init(text: "Ready", color: Color.stenoLive)
    case .failed: self.init(text: "Failed", color: Color.stenoDestructive)
    }
  }
}

extension Binding where Value: Sendable {
  /// A binding whose reads come from the model and whose writes run an
  /// async main-actor view-model action, for controls that mirror a
  /// `private(set)` property and save on change.
  static func action(
    _ get: @escaping () -> Value, _ set: @escaping @Sendable @MainActor (Value) async -> Void
  ) -> Binding<Value> {
    Binding(get: get, set: { value in Task { @MainActor in await set(value) } })
  }
}

extension View {
  /// The 720 pt reading column the four tabs share: 32 pt sides, 24 pt
  /// above, 32 pt below, pinned to the leading edge (a `ScrollView` would
  /// otherwise centre a column narrower than the pane).
  func readingColumn() -> some View {
    frame(maxWidth: 720, alignment: .leading)
      .padding(.horizontal, Theme.Space.xxl)
      .padding(.top, Theme.Space.xl)
      .padding(.bottom, Theme.Space.xxl)
      .frame(maxWidth: .infinity, alignment: .leading)
      .textSelection(.enabled)
  }

  /// The ladder's leading for a text size applied through `lineSpacing`,
  /// the one place `lineHeight - size` is spelled out.
  func stenoLeading(_ size: (size: CGFloat, lineHeight: CGFloat)) -> some View {
    lineSpacing(size.lineHeight - size.size)
  }

  /// Body prose at 14/19: `TextSize.sm` with its leading.
  func proseLeading() -> some View {
    stenoLeading(Theme.TextSize.sm)
  }

  /// `shadow-sm`, the one shadow in the system: on the active segmented cell.
  func stenoShadowSmall() -> some View {
    shadow(color: .black.opacity(0.1), radius: 1.5, y: 1)
      .shadow(color: .black.opacity(0.1), radius: 1, y: 1)
  }
}

extension InsettableShape {
  /// The 1 pt inner stroke every bordered surface wears, `border` unless a
  /// state (focus `ring`) says otherwise.
  func hairline(_ color: Color = Color.stenoBorder) -> some View {
    strokeBorder(color, lineWidth: Theme.Space.hairline)
  }
}

extension TimeInterval {
  private var wholeSeconds: Duration { .seconds(Int(max(0, rounded(.down)))) }

  /// `mm:ss` or `h:mm:ss` for elapsed recording time.
  var clockText: String {
    wholeSeconds.formatted(
      .time(
        pattern: self >= 3600
          ? .hourMinuteSecond(padHourToLength: 1) : .minuteSecond(padMinuteToLength: 2)))
  }

  /// `HH:MM:SS` for transcript timestamps.
  var timestampText: String {
    wholeSeconds.formatted(.time(pattern: .hourMinuteSecond(padHourToLength: 2)))
  }
}

#if DEBUG
  /// Light beside dark on their own canvases, for the component previews.
  struct PreviewPair<Content: View>: View {
    @ViewBuilder var content: () -> Content

    var body: some View {
      HStack(spacing: 0) {
        pane(.light)
        pane(.dark)
      }
    }

    private func pane(_ scheme: ColorScheme) -> some View {
      content()
        .padding(Theme.Space.xl)
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .background(Color.stenoBackground)
        .environment(\.colorScheme, scheme)
    }
  }

  #Preview("Buttons") {
    PreviewPair {
      VStack(alignment: .leading, spacing: Theme.Space.md) {
        HStack(spacing: Theme.Space.sm) {
          Button("Record call") {}.buttonStyle(StenoPrimaryButtonStyle())
          Button("Later") {}.buttonStyle(StenoSecondaryButtonStyle())
        }
        HStack(spacing: Theme.Space.sm) {
          Button("Record call") {}.buttonStyle(StenoPrimaryButtonStyle()).disabled(true)
          Button("Later") {}.buttonStyle(StenoSecondaryButtonStyle()).disabled(true)
        }
        Button {
        } label: {
          StopLabel(since: .now.addingTimeInterval(-754))
        }
        .buttonStyle(StenoSecondaryButtonStyle())
      }
    }
    .frame(width: 560, height: 220)
  }

  #Preview("Chips") {
    PreviewPair {
      HStack(spacing: Theme.Space.sm) {
        let states: [MeetingState] = [
          .recording, .queued, .processing, .ready, .failed(reason: "Timed out"),
        ]
        ForEach(states, id: \.self) { StatusChip($0) }
        StatusChip(text: "Optional", style: .neutral)
        StatusChip(text: "Exported 10:02", style: .neutral, systemImage: "folder")
      }
    }
    .frame(width: 760, height: 120)
  }

  #Preview("Card and message rows") {
    PreviewPair {
      VStack(alignment: .leading, spacing: Theme.Space.md) {
        Card {
          VStack(alignment: .leading, spacing: Theme.Space.sm) {
            SectionLabel(text: "Meetings")
            Text("Produktstrategie 90/10")
              .font(.steno(Theme.TextSize.sm, weight: .medium))
              .foregroundStyle(Color.stenoStrong)
            Text("Decided to ship the redesign as two PRs.")
              .font(.steno(Theme.TextSize.xs))
              .foregroundStyle(Color.stenoMutedForeground)
          }
        }
        MessageRow(kind: .info, text: "Processing starts when the current meeting finishes.")
        MessageRow(kind: .warning, text: "Microphone access is denied.")
        MessageRow(kind: .error, text: "The LLM endpoint did not answer.")
      }
    }
    .frame(width: 720, height: 360)
  }
#endif
