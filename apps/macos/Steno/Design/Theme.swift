import AppKit
import SwiftUI

/// Design tokens for the SwiftUI surfaces that remain native: the menu bar
/// popover, the floating recording bubble and the detection prompt (plan
/// `2026-09-29-macos-webview-ui.md`, non-goals). They mirror the names and
/// values of `mobile/global.css`: dark is the base, light is the re-based
/// ladder; alpha-veil surfaces (never opaque greys), hairline borders and
/// semantic status colours. Every colour is a dynamic `NSColor` that follows
/// the window's appearance. `ThemeTokensTests` checks that every entry in
/// `tokens` is a `--color-*` name in the CSS; the three windows are web pages
/// now and take their colours from `apps/macos/web/src/theme.css` instead
/// (`WebCanvas` carries the one value the host shares with them).
enum Theme {
  typealias RGBA = (red: Double, green: Double, blue: Double, alpha: Double)

  struct Token: Sendable {
    let cssName: String
    let dark: RGBA
    let light: RGBA

    var nsColor: NSColor {
      let dark = self.dark
      let light = self.light
      return NSColor(name: NSColor.Name("steno.\(cssName)")) { appearance in
        let isDark = appearance.bestMatch(from: [.darkAqua, .aqua]) == .darkAqua
        let value = isDark ? dark : light
        return NSColor(
          srgbRed: value.red, green: value.green, blue: value.blue, alpha: value.alpha)
      }
    }

    var color: Color { Color(nsColor: nsColor) }
  }

  /// `0xRRGGBB` as an opaque sRGB value.
  static func hex(_ value: UInt32, alpha: Double = 1) -> RGBA {
    (
      red: Double((value >> 16) & 0xFF) / 255, green: Double((value >> 8) & 0xFF) / 255,
      blue: Double(value & 0xFF) / 255, alpha: alpha
    )
  }

  private static func white(_ alpha: Double) -> RGBA { (red: 1, green: 1, blue: 1, alpha: alpha) }
  private static func black(_ alpha: Double) -> RGBA { (red: 0, green: 0, blue: 0, alpha: alpha) }

  // Canvas and text ladder.
  static let background = Token(cssName: "background", dark: hex(0x000000), light: hex(0xFAFAFA))
  static let strong = Token(cssName: "strong", dark: hex(0xFFFFFF), light: hex(0x0A0A0A))
  static let foreground = Token(cssName: "foreground", dark: hex(0xD4D4D4), light: hex(0x404040))
  static let mutedForeground = Token(
    cssName: "muted-foreground", dark: hex(0xA3A3A3), light: hex(0x525252))
  static let faint = Token(cssName: "faint", dark: hex(0x737373), light: hex(0x696969))
  static let ghost = Token(cssName: "ghost", dark: hex(0x525252), light: hex(0xA3A3A3))

  // Alpha-veil surfaces.
  static let card = Token(cssName: "card", dark: white(0.031), light: black(0.031))
  static let secondary = Token(cssName: "secondary", dark: white(0.059), light: black(0.059))
  static let popover = Token(cssName: "popover", dark: hex(0x101010), light: hex(0xFFFFFF))

  // Hairlines.
  static let border = Token(cssName: "border", dark: white(0.102), light: black(0.122))

  // The achromatic CTA gradient.
  static let accentFrom = Token(cssName: "accent-from", dark: hex(0xFFFFFF), light: hex(0x171717))
  static let accentTo = Token(cssName: "accent-to", dark: hex(0xE4E4E7), light: hex(0x000000))
  static let onAccent = Token(cssName: "on-accent", dark: hex(0x000000), light: hex(0xFFFFFF))

  // Semantic status.
  static let live = Token(cssName: "live", dark: hex(0x4C8C57), light: hex(0x3C8149))
  static let warning = Token(cssName: "warning", dark: hex(0xD4A72C), light: hex(0x7A5800))
  static let info = Token(cssName: "info", dark: hex(0x6B8CBE), light: hex(0x3D64A0))
  static let destructive = Token(cssName: "destructive", dark: hex(0xD05252), light: hex(0xB23A3A))
  static let destructiveStrong = Token(
    cssName: "destructive-strong", dark: hex(0xDC6B6B), light: hex(0x9A2F2F))

  // The one Mac-only surface: `raised` is the white card on the light
  // canvas; on black it is the `card` veil, the dark idiom the ladder was
  // authored on.
  static let raised = Token(cssName: "raised", dark: white(0.031), light: hex(0xFFFFFF))

  /// Every token by its CSS name (`--color-<name>`), for the tokens test.
  static let tokens: [Token] = [
    background, strong, foreground, mutedForeground, faint, ghost,
    card, secondary, popover,
    border,
    accentFrom, accentTo, onAccent,
    live, warning, info, destructive, destructiveStrong,
  ]

  /// The Mac-only surfaces; not in the CSS and not in `tokens`.
  static let macTokens: [Token] = [raised]

  /// Type scale from `global.css` (`--text-*`), in points with line heights.
  enum TextSize {
    static let xxxs: (size: CGFloat, lineHeight: CGFloat) = (11, 14)
    static let xxs: (size: CGFloat, lineHeight: CGFloat) = (12, 16)
    static let xs: (size: CGFloat, lineHeight: CGFloat) = (13, 17)
    static let sm: (size: CGFloat, lineHeight: CGFloat) = (14, 19)
  }

  /// Spacing used across the native surfaces, so call sites carry no
  /// numbers. Every step except `xxs` and `hairline` sits on the 4 pt grid.
  enum Space {
    static let xxs: CGFloat = 2
    static let xs: CGFloat = 4
    static let sm: CGFloat = 8
    static let md: CGFloat = 12
    static let lg: CGFloat = 16
    static let xl: CGFloat = 24
    static let hairline: CGFloat = 1
  }

  /// Radii descend one step per nest: cards `lg`, controls `md`, chips and
  /// the bubble's wells `sm`; `xl` is for panels the system does not round
  /// for us.
  enum Radius: CGFloat, CaseIterable {
    case xl = 16
    case lg = 12
    case md = 8
    case sm = 6

    var shape: RoundedRectangle { RoundedRectangle(cornerRadius: rawValue, style: .continuous) }
  }

  /// Control boxes: the button height and the insets the recipes fix
  /// off-grid.
  enum Control {
    static let buttonHeight: CGFloat = 32
    /// Horizontal padding inside a button.
    static let buttonInset: CGFloat = 14
    /// Horizontal padding inside a chip.
    static let chipInset: CGFloat = 8
    /// Horizontal padding inside a message row.
    static let rowInset: CGFloat = 10
    /// Vertical padding inside the menu bar popover's queue and recent rows
    /// (8 pt horizontally, from `Space`).
    static let menuRowInset: CGFloat = 6
    /// The glyph inside a chip, one point under its 12 pt text.
    static let chipGlyphSize: CGFloat = 11
  }
}

extension Color {
  static var stenoBackground: Color { Theme.background.color }
  static var stenoStrong: Color { Theme.strong.color }
  static var stenoForeground: Color { Theme.foreground.color }
  static var stenoMutedForeground: Color { Theme.mutedForeground.color }
  static var stenoFaint: Color { Theme.faint.color }
  static var stenoGhost: Color { Theme.ghost.color }
  static var stenoCard: Color { Theme.card.color }
  static var stenoRaised: Color { Theme.raised.color }
  static var stenoSecondary: Color { Theme.secondary.color }
  static var stenoPopover: Color { Theme.popover.color }
  static var stenoBorder: Color { Theme.border.color }
  static var stenoAccentFrom: Color { Theme.accentFrom.color }
  static var stenoAccentTo: Color { Theme.accentTo.color }
  static var stenoOnAccent: Color { Theme.onAccent.color }
  static var stenoLive: Color { Theme.live.color }
  static var stenoWarning: Color { Theme.warning.color }
  static var stenoInfo: Color { Theme.info.color }
  static var stenoDestructive: Color { Theme.destructive.color }
  static var stenoDestructiveStrong: Color { Theme.destructiveStrong.color }
}

extension Font {
  static func steno(_ size: (size: CGFloat, lineHeight: CGFloat), weight: Font.Weight = .regular)
    -> Font
  {
    .system(size: size.size, weight: weight)
  }
}
