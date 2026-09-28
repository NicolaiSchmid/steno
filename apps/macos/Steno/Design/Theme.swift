import AppKit
import SwiftUI

/// Design tokens mirroring `mobile/global.css`: dark is the base, light is
/// the re-based ladder; five luminance tiers, alpha-veil surfaces (never
/// opaque greys), hairline borders, one achromatic primary and semantic
/// status colours. Every colour is a dynamic `NSColor` that follows the
/// window's appearance. `ThemeTokensTests` checks that every `--color-*`
/// name in the CSS has an entry in `tokens`. `macTokens` adds the two
/// surfaces only the Mac needs (`sidebar`, `raised`), which the CSS does not
/// mirror and which must never collide with a CSS name.
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

  private static func hex(_ value: UInt32, alpha: Double = 1) -> RGBA {
    (
      red: Double((value >> 16) & 0xFF) / 255, green: Double((value >> 8) & 0xFF) / 255,
      blue: Double(value & 0xFF) / 255, alpha: alpha
    )
  }

  private static func white(_ alpha: Double) -> RGBA { (red: 1, green: 1, blue: 1, alpha: alpha) }
  private static func black(_ alpha: Double) -> RGBA { (red: 0, green: 0, blue: 0, alpha: alpha) }

  /// Source-over compositing of an alpha veil on an opaque base, so a
  /// derived surface stays derivable instead of being a hand-picked grey.
  static func composite(_ veil: RGBA, over base: RGBA) -> RGBA {
    let a = veil.alpha
    return (
      red: veil.red * a + base.red * (1 - a), green: veil.green * a + base.green * (1 - a),
      blue: veil.blue * a + base.blue * (1 - a), alpha: 1
    )
  }

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
  static let cardForeground = Token(
    cssName: "card-foreground", dark: hex(0xD4D4D4), light: hex(0x404040))
  static let muted = Token(cssName: "muted", dark: white(0.031), light: black(0.031))
  static let secondary = Token(cssName: "secondary", dark: white(0.059), light: black(0.059))
  static let secondaryForeground = Token(
    cssName: "secondary-foreground", dark: hex(0xFFFFFF), light: hex(0x0A0A0A))
  static let accent = Token(cssName: "accent", dark: white(0.059), light: black(0.059))
  static let accentForeground = Token(
    cssName: "accent-foreground", dark: hex(0xFFFFFF), light: hex(0x0A0A0A))
  static let popover = Token(cssName: "popover", dark: hex(0x101010), light: hex(0xFFFFFF))
  static let popoverForeground = Token(
    cssName: "popover-foreground", dark: hex(0xD4D4D4), light: hex(0x404040))

  // Hairlines.
  static let border = Token(cssName: "border", dark: white(0.102), light: black(0.122))
  static let input = Token(cssName: "input", dark: white(0.102), light: black(0.122))
  static let ring = Token(cssName: "ring", dark: white(0.251), light: black(0.239))

  // Primary action and the achromatic CTA gradient.
  static let primary = Token(cssName: "primary", dark: hex(0xFFFFFF), light: hex(0x171717))
  static let primaryForeground = Token(
    cssName: "primary-foreground", dark: hex(0x000000), light: hex(0xFFFFFF))
  static let accentFrom = Token(cssName: "accent-from", dark: hex(0xFFFFFF), light: hex(0x171717))
  static let accentTo = Token(cssName: "accent-to", dark: hex(0xE4E4E7), light: hex(0x000000))
  static let onAccent = Token(cssName: "on-accent", dark: hex(0x000000), light: hex(0xFFFFFF))

  // Semantic status.
  static let live = Token(cssName: "live", dark: hex(0x4C8C57), light: hex(0x3C8149))
  static let liveBright = Token(cssName: "live-bright", dark: hex(0x62B06F), light: hex(0x2F7040))
  static let warning = Token(cssName: "warning", dark: hex(0xD4A72C), light: hex(0x7A5800))
  static let info = Token(cssName: "info", dark: hex(0x6B8CBE), light: hex(0x3D64A0))
  static let destructive = Token(cssName: "destructive", dark: hex(0xD05252), light: hex(0xB23A3A))
  static let destructiveStrong = Token(
    cssName: "destructive-strong", dark: hex(0xDC6B6B), light: hex(0x9A2F2F))
  static let destructiveForeground = Token(
    cssName: "destructive-foreground", dark: hex(0xFFFFFF), light: hex(0xFFFFFF))
  static let codeBackground = Token(cssName: "code-bg", dark: hex(0x101010), light: hex(0xF4F4F3))

  // Mac-only surfaces. `sidebar` is the nav column fill: `background` with
  // the `card` veil composited on it (`#f2f2f2` light, `#080808` dark).
  // `raised` is the white card on the light canvas; on black it is the
  // `card` veil, the dark idiom the ladder was authored on.
  static let sidebar = Token(
    cssName: "sidebar", dark: composite(card.dark, over: background.dark),
    light: composite(card.light, over: background.light))
  static let raised = Token(cssName: "raised", dark: white(0.031), light: hex(0xFFFFFF))

  /// Every token by its CSS name (`--color-<name>`), for the tokens test.
  static let tokens: [Token] = [
    background, strong, foreground, mutedForeground, faint, ghost,
    card, cardForeground, muted, secondary, secondaryForeground, accent, accentForeground,
    popover, popoverForeground,
    border, input, ring,
    primary, primaryForeground, accentFrom, accentTo, onAccent,
    live, liveBright, warning, info, destructive, destructiveStrong, destructiveForeground,
    codeBackground,
  ]

  /// The Mac-only surfaces; not in the CSS and not in `tokens`.
  static let macTokens: [Token] = [sidebar, raised]

  /// Type scale from `global.css` (`--text-*`), in points with line heights.
  enum TextSize {
    static let xxxs: (size: CGFloat, lineHeight: CGFloat) = (11, 14)
    static let xxs: (size: CGFloat, lineHeight: CGFloat) = (12, 16)
    static let xs: (size: CGFloat, lineHeight: CGFloat) = (13, 17)
    static let sm: (size: CGFloat, lineHeight: CGFloat) = (14, 19)
    static let base: (size: CGFloat, lineHeight: CGFloat) = (16, 23)
    static let lg: (size: CGFloat, lineHeight: CGFloat) = (18, 23)
    static let xl: (size: CGFloat, lineHeight: CGFloat) = (21, 28)
    static let xxl: (size: CGFloat, lineHeight: CGFloat) = (26, 32)
    static let xxxl: (size: CGFloat, lineHeight: CGFloat) = (30, 36)
  }

  /// Spacing used across the app, so call sites carry no numbers. Every
  /// step except `xxs` and `hairline` sits on the 4 pt grid.
  enum Space {
    static let xxs: CGFloat = 2
    static let xs: CGFloat = 4
    static let sm: CGFloat = 8
    static let md: CGFloat = 12
    static let lg: CGFloat = 16
    static let xl: CGFloat = 24
    static let xxl: CGFloat = 32
    static let xxxl: CGFloat = 48
    static let hairline: CGFloat = 1
  }

  /// Radii descend one step per nest: cards and the CTA `lg`, controls and
  /// inputs `md`, chips and segmented cells `sm`, tiny wells `xs`; `xl` is
  /// reserved for panels the system does not round for us.
  enum Radius: CGFloat, CaseIterable {
    case xl = 16
    case lg = 12
    case md = 8
    case sm = 6
    case xs = 4

    var shape: RoundedRectangle { RoundedRectangle(cornerRadius: rawValue, style: .continuous) }
  }

  /// Control boxes: heights and the insets the recipes fix off-grid.
  enum Control {
    /// The Start recording CTA at the top of the nav column.
    static let ctaHeight: CGFloat = 40
    static let buttonHeight: CGFloat = 32
    static let inputHeight: CGFloat = 28
    static let navRowHeight: CGFloat = 32
    static let iconButtonSize: CGFloat = 28
    static let segmentHeight: CGFloat = 24
    static let segmentContainerHeight: CGFloat = 28
    /// Horizontal padding inside a button.
    static let buttonInset: CGFloat = 14
    /// Horizontal padding inside a chip.
    static let chipInset: CGFloat = 6
    /// Horizontal padding inside nav rows, inputs and the search field.
    static let rowInset: CGFloat = 10
    /// The glyph column in a nav row.
    static let navGlyphWidth: CGFloat = 20
    /// The glyph inside a chip, one point under its 11 pt text.
    static let chipGlyphSize: CGFloat = 10
    /// The empty state's icon well and the glyph inside it.
    static let emptyWellSize: CGFloat = 48
    static let emptySymbolSize: CGFloat = 20
    /// The Start recording CTA: the glyph well before the label and the
    /// trailing chevron segment that opens the in-person menu.
    static let ctaWellSize: CGFloat = 28
    static let ctaMenuWidth: CGFloat = 28
    /// The selection rail on a list entry.
    static let railWidth: CGFloat = 2
    /// The compact level meter inside the Stop control: one 4 pt bar per
    /// lane, 40 pt wide.
    static let meterWidth: CGFloat = 40
    static let meterHeight: CGFloat = 4
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
  static var stenoSidebar: Color { Theme.sidebar.color }
  static var stenoRaised: Color { Theme.raised.color }
  static var stenoSecondary: Color { Theme.secondary.color }
  static var stenoAccent: Color { Theme.accent.color }
  static var stenoPopover: Color { Theme.popover.color }
  static var stenoBorder: Color { Theme.border.color }
  static var stenoInput: Color { Theme.input.color }
  static var stenoRing: Color { Theme.ring.color }
  static var stenoPrimary: Color { Theme.primary.color }
  static var stenoPrimaryForeground: Color { Theme.primaryForeground.color }
  static var stenoAccentFrom: Color { Theme.accentFrom.color }
  static var stenoAccentTo: Color { Theme.accentTo.color }
  static var stenoOnAccent: Color { Theme.onAccent.color }
  static var stenoLive: Color { Theme.live.color }
  static var stenoLiveBright: Color { Theme.liveBright.color }
  static var stenoWarning: Color { Theme.warning.color }
  static var stenoInfo: Color { Theme.info.color }
  static var stenoDestructive: Color { Theme.destructive.color }
  static var stenoCodeBackground: Color { Theme.codeBackground.color }
}

extension Font {
  static func steno(_ size: (size: CGFloat, lineHeight: CGFloat), weight: Font.Weight = .regular)
    -> Font
  {
    .system(size: size.size, weight: weight)
  }
}
