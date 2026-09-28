import SwiftUI

/// Initials on a `secondary` circle for a named speaker, a `person` glyph
/// for an unnamed one. Achromatic on purpose: hue is not how this app tells
/// people apart.
struct Avatar: View {
  let name: String?
  var size: CGFloat = 28

  var body: some View {
    ZStack {
      Circle().fill(Color.stenoSecondary)
      if let name, let initials = Self.initials(of: name) {
        Text(initials)
          .font(.system(size: size * 0.42, weight: .semibold))
          .foregroundStyle(Color.stenoStrong)
      } else {
        Image(systemName: "person")
          .font(.system(size: size * 0.45))
          .foregroundStyle(Color.stenoFaint)
      }
    }
    .frame(width: size, height: size)
    .accessibilityHidden(true)
  }

  /// The first letters of the first two words, uppercased ("Jan Herold" is
  /// "JH", "Nicolai" is "N"); nil for a blank name.
  nonisolated static func initials(of name: String) -> String? {
    let words = name.split(whereSeparator: { $0.isWhitespace }).prefix(2)
    let letters = words.compactMap { word in word.first.map { String($0).uppercased() } }
    return letters.isEmpty ? nil : letters.joined()
  }
}
