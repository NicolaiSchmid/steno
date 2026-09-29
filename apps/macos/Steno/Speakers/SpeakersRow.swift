import StenoCore
import SwiftUI

/// The header's Speakers row: a 13 pt `muted` label and one pill button
/// with the first avatars, the confirmed names, "+n" for the rest and "n to
/// confirm" while speakers are unnamed. The pill is the secondary button it
/// behaves like: the button height, `raised` with a hairline, the `card`
/// veil on hover, never a grey block. Clicking it toggles the
/// `SpeakersPopover`.
struct SpeakersRow: View {
  let model: SpeakersViewModel
  @Binding var isPresented: Bool
  @State private var hovering = false
  @Environment(\.accessibilityReduceMotion) private var reduceMotion

  static let shownAvatars = 3
  /// The stacked avatars inside the pill; 5 pt under the button height's
  /// text box so the ring has room.
  static let avatarSize: CGFloat = 22

  private var named: [String] {
    model.rows.filter(\.isConfirmed).compactMap { $0.person?.displayName }
  }
  private var shownNames: [String] { Array(named.prefix(Self.shownAvatars)) }
  private var overflow: Int { named.count - shownNames.count }
  private var pendingText: String? {
    switch model.unconfirmedCount {
    case 0: nil
    case 1: "1 to confirm"
    case let count: "\(count) to confirm"
    }
  }

  /// "Nicolai, Jan · 1 to confirm": what VoiceOver and the UI test read.
  var summary: String {
    var parts: [String] = []
    if !shownNames.isEmpty {
      parts.append(shownNames.joined(separator: ", ") + (overflow > 0 ? " +\(overflow)" : ""))
    }
    if let pendingText { parts.append(pendingText) }
    return parts.isEmpty ? "No speakers" : parts.joined(separator: " · ")
  }

  var body: some View {
    HStack(spacing: Theme.Space.md) {
      Text("Speakers")
        .font(.steno(Theme.TextSize.xs))
        .foregroundStyle(Color.stenoMutedForeground)
      Button {
        isPresented.toggle()
      } label: {
        HStack(spacing: Theme.Space.sm) {
          HStack(spacing: -6) {
            ForEach(model.rows.prefix(Self.shownAvatars)) { row in
              Avatar(name: row.isConfirmed ? row.person?.displayName : nil, size: Self.avatarSize)
                .overlay(Circle().strokeBorder(Color.stenoBackground, lineWidth: 1.5))
            }
          }
          if !shownNames.isEmpty {
            Text("·").foregroundStyle(Color.stenoGhost)
            Text(shownNames.joined(separator: ", "))
              .font(.steno(Theme.TextSize.xs, weight: .medium))
              .foregroundStyle(Color.stenoStrong)
              .lineLimit(1)
          }
          if overflow > 0 {
            Text("+\(overflow)")
              .font(.steno(Theme.TextSize.xs))
              .foregroundStyle(Color.stenoFaint)
          }
          if let pendingText {
            Text(pendingText)
              .font(.steno(Theme.TextSize.xxs))
              .foregroundStyle(Color.stenoFaint)
          }
          Image(systemName: "chevron.down")
            .font(.system(size: 10, weight: .semibold))
            .foregroundStyle(Color.stenoFaint)
        }
        .padding(.horizontal, Theme.Space.md)
        .frame(height: Theme.Control.buttonHeight)
        .background(
          ZStack {
            Theme.Radius.md.shape.fill(Color.stenoRaised)
            Theme.Radius.md.shape.fill(hovering ? Color.stenoCard : Color.clear)
          }
        )
        .overlay(Theme.Radius.md.shape.hairline())
        .contentShape(Theme.Radius.md.shape)
      }
      .buttonStyle(.plain)
      .onHover { hovering = $0 }
      .animation(Motion.swap(reduceMotion: reduceMotion), value: hovering)
      .accessibilityIdentifier("speakers-row")
      .accessibilityLabel(summary)
      .help("Who spoke; click to name speakers")
    }
  }
}
