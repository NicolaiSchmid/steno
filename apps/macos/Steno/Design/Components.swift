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

extension MeetingStateLabel {
  static func chip(for state: MeetingStateLabel) -> StatusChip {
    StatusChip(text: state.text, color: state.color)
  }
}

/// How a `MeetingState` reads in the list and the queue.
struct MeetingStateLabel {
  var text: String
  var color: Color
}

extension Duration {
  /// `mm:ss` or `h:mm:ss` for elapsed recording time.
  var clockText: String {
    let total = Int(components.seconds)
    return TimeInterval(total).clockText
  }
}

extension TimeInterval {
  var clockText: String {
    let total = max(0, Int(self.rounded(.down)))
    let hours = total / 3600
    let minutes = (total % 3600) / 60
    let seconds = total % 60
    if hours > 0 { return String(format: "%d:%02d:%02d", hours, minutes, seconds) }
    return String(format: "%02d:%02d", minutes, seconds)
  }

  /// `HH:MM:SS` for transcript timestamps.
  var timestampText: String {
    let total = max(0, Int(self.rounded(.down)))
    return String(format: "%02d:%02d:%02d", total / 3600, (total % 3600) / 60, total % 60)
  }
}
