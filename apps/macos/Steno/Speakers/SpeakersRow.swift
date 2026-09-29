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
  /// The stacked avatars inside the 32 pt button: 22 pt, overlapping by 6,
  /// each with a 1.5 pt `background` ring so they read as separate circles
  /// where they cross.
  static let avatarSize: CGFloat = 22
  static let avatarOverlap: CGFloat = 6
  static let avatarRingWidth: CGFloat = 1.5

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
          HStack(spacing: -Self.avatarOverlap) {
            ForEach(model.rows.prefix(Self.shownAvatars)) { row in
              Avatar(name: row.isConfirmed ? row.person?.displayName : nil, size: Self.avatarSize)
                .overlay(
                  Circle().strokeBorder(Color.stenoBackground, lineWidth: Self.avatarRingWidth))
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
            Text("+\(overflow)").foregroundStyle(Color.stenoFaint)
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
        // 13 pt inside the style's 14 pt box: the row is a summary, not a label.
        .font(.steno(Theme.TextSize.xs))
      }
      .buttonStyle(StenoSecondaryButtonStyle())
      .accessibilityIdentifier("speakers-row")
      .accessibilityLabel(summary)
      .help("Who spoke; click to name speakers")
    }
  }
}
