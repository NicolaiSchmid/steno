import StenoCore
import SwiftUI

/// The few composed controls the app reuses: an achromatic primary button,
/// a veil secondary button, a status chip and a hairline card. Pressed
/// states use the motion tokens.

struct StenoPrimaryButtonStyle: ButtonStyle {
  func makeBody(configuration: Configuration) -> some View {
    configuration.label
      .font(.steno(Theme.TextSize.xs, weight: .semibold))
      .foregroundStyle(Color.stenoPrimaryForeground)
      .padding(.horizontal, Theme.Space.md)
      .padding(.vertical, Theme.Space.xs + 2)
      .background(
        RoundedRectangle(cornerRadius: Theme.Space.radiusSmall, style: .continuous)
          .fill(Color.stenoPrimary))
      .scaleEffect(configuration.isPressed ? Motion.pressScale : 1)
      .opacity(configuration.isPressed ? Motion.pressOpacity : 1)
      .animation(Motion.functional, value: configuration.isPressed)
  }
}

struct StenoSecondaryButtonStyle: ButtonStyle {
  func makeBody(configuration: Configuration) -> some View {
    configuration.label
      .font(.steno(Theme.TextSize.xs, weight: .medium))
      .foregroundStyle(Color.stenoForeground)
      .padding(.horizontal, Theme.Space.md)
      .padding(.vertical, Theme.Space.xs + 2)
      .background(
        RoundedRectangle(cornerRadius: Theme.Space.radiusSmall, style: .continuous)
          .fill(Color.stenoSecondary))
      .overlay(
        RoundedRectangle(cornerRadius: Theme.Space.radiusSmall, style: .continuous)
          .strokeBorder(Color.stenoBorder, lineWidth: Theme.Space.hairline))
      .scaleEffect(configuration.isPressed ? Motion.pressScale : 1)
      .opacity(configuration.isPressed ? Motion.pressOpacity : 1)
      .animation(Motion.functional, value: configuration.isPressed)
  }
}

/// A small alpha chip (never filled) with a semantic colour.
struct StatusChip: View {
  var text: String
  var color: Color

  var body: some View {
    Text(text)
      .font(.steno(Theme.TextSize.xxxs, weight: .medium))
      .foregroundStyle(color)
      .padding(.horizontal, Theme.Space.sm)
      .padding(.vertical, 2)
      .background(
        Capsule().fill(color.opacity(0.12)))
  }
}

/// A veil surface with a hairline border.
struct Card<Content: View>: View {
  @ViewBuilder var content: () -> Content

  var body: some View {
    content()
      .padding(Theme.Space.md)
      .background(
        RoundedRectangle(cornerRadius: Theme.Space.radius, style: .continuous)
          .fill(Color.stenoCard))
      .overlay(
        RoundedRectangle(cornerRadius: Theme.Space.radius, style: .continuous)
          .strokeBorder(Color.stenoBorder, lineWidth: Theme.Space.hairline))
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

/// An inline message row for errors and warnings.
struct MessageRow: View {
  enum Kind {
    case error
    case warning
    case info
  }

  var kind: Kind
  var text: String

  private var color: Color {
    switch kind {
    case .error: Color.stenoDestructive
    case .warning: Color.stenoWarning
    case .info: Color.stenoInfo
    }
  }

  var body: some View {
    HStack(alignment: .top, spacing: Theme.Space.sm) {
      Circle().fill(color).frame(width: 6, height: 6).padding(.top, 5)
      Text(text)
        .font(.steno(Theme.TextSize.xs))
        .foregroundStyle(Color.stenoForeground)
        .textSelection(.enabled)
    }
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

/// What a display tab shows until the pipeline has produced its content:
/// `none` once the meeting is ready, `pending` before.
struct PendingText: View {
  let meeting: Meeting?
  let none: String
  let pending: String

  var body: some View {
    Text(Self.text(meeting: meeting, none: none, pending: pending))
      .font(.steno(Theme.TextSize.sm, weight: .medium))
      .foregroundStyle(Color.stenoMutedForeground)
  }

  /// `nonisolated`: `View` conformance makes the struct main-actor
  /// isolated, and `TabText` calls this from plain code.
  nonisolated static func text(meeting: Meeting?, none: String, pending: String) -> String {
    meeting?.state == .ready ? none : pending
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
  /// The 720 pt reading column the Summary, Transcript and Tasks tabs share.
  func readingColumn() -> some View {
    frame(maxWidth: 720, alignment: .leading)
      .padding(Theme.Space.lg)
      .textSelection(.enabled)
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
