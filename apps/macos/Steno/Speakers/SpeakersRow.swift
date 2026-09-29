import StenoCore
import SwiftUI

/// The header's Speakers row: a 13 pt `muted` label and one secondary
/// button with the first avatars, the confirmed names, "+n" for the rest
/// and "n to confirm" while speakers are unnamed. Clicking it toggles the
/// `SpeakersPopover`.
struct SpeakersRow: View {
  let model: SpeakersViewModel
  @Binding var isPresented: Bool

  static let shownAvatars = 3
  /// The stacked avatars inside the button, 5 pt under its 32 pt height so
  /// the ring around each has room.
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
            Text("·")
              .font(.steno(Theme.TextSize.xs))
              .foregroundStyle(Color.stenoGhost)
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
      }
      .buttonStyle(StenoSecondaryButtonStyle())
      .accessibilityIdentifier("speakers-row")
      .accessibilityLabel(summary)
      .help("Who spoke; click to name speakers")
    }
  }
}
