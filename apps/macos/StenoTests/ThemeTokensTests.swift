import AppKit
import SwiftUI
import XCTest

/// The native surfaces' tokens: every entry in `Theme.tokens` is a `--color-*`
/// name in `mobile/global.css` (the ladder they mirror) and the one Mac-only
/// surface resolves in both appearances. Spacing, radii, the
/// remaining control boxes and the motion rules are pinned to the values the
/// menu bar popover, the bubble and the detection prompt were drawn with.
final class ThemeTokensTests: XCTestCase {
  func testEverySwiftTokenIsAMobileCSSToken() throws {
    let cssNames = try mobileColorNames()
    let swiftNames = Set(Theme.tokens.map(\.cssName))
    XCTAssertEqual(swiftNames.subtracting(cssNames), [], "Swift tokens without a CSS token")
    XCTAssertEqual(Theme.tokens.count, swiftNames.count, "duplicate token names")
  }

  func testTokensResolveToDynamicColors() {
    for token in Theme.tokens + Theme.macTokens {
      let color = token.nsColor
      XCTAssertNotNil(color.usingColorSpace(.sRGB), token.cssName)
    }
  }

  /// `raised` is the one Mac-only surface: opaque white on the light canvas,
  /// the 3.1 % white veil on the dark one, resolved under the matching
  /// appearance, and never a name the CSS owns.
  @MainActor func testRaisedResolvesInBothAppearancesAndAvoidsCSSNames() throws {
    XCTAssertEqual(Theme.macTokens.map(\.cssName), ["raised"])
    let cssNames = try mobileColorNames()
    for token in Theme.macTokens {
      XCTAssertFalse(cssNames.contains(token.cssName), "\(token.cssName) collides with the CSS")
      try assertResolves(token, under: .aqua, to: token.light)
      try assertResolves(token, under: .darkAqua, to: token.dark)
    }
    assertEqual(Theme.raised.light, (red: 1, green: 1, blue: 1, alpha: 1), "raised light")
    assertEqual(Theme.raised.dark, (red: 1, green: 1, blue: 1, alpha: 0.031), "raised dark")
  }

  /// Every spacing step except `xxs` and `hairline` sits on the 4 pt grid,
  /// and the radii descend one step per nest: 16 > 12 > 8 > 6.
  func testSpaceSitsOnTheGridAndRadiiDescend() {
    let steps: [(name: String, value: CGFloat)] = [
      ("xs", Theme.Space.xs), ("sm", Theme.Space.sm), ("md", Theme.Space.md),
      ("lg", Theme.Space.lg), ("xl", Theme.Space.xl),
    ]
    for step in steps {
      XCTAssertEqual(
        step.value.truncatingRemainder(dividingBy: 4), 0, "Space.\(step.name) is off the 4 pt grid")
    }
    XCTAssertEqual(Theme.Space.xxs, 2)
    XCTAssertEqual(Theme.Space.hairline, 1)
    XCTAssertEqual(Theme.Radius.allCases.map(\.rawValue), [16, 12, 8, 6])
  }

  /// The control boxes the native surfaces still draw: 32 pt buttons with a
  /// 14 pt inset, chips at 8, message rows at 10, menu rows at 6, and the
  /// chip's glyph one point under its 12 pt text.
  func testControlBoxesMatchThePlan() {
    XCTAssertEqual(Theme.Control.buttonHeight, 32)
    XCTAssertEqual(Theme.Control.buttonInset, 14)
    XCTAssertEqual(Theme.Control.chipInset, 8)
    XCTAssertEqual(Theme.Control.rowInset, 10)
    XCTAssertEqual(Theme.Control.menuRowInset, 6)
    XCTAssertEqual(Theme.Control.chipGlyphSize, 11)
    XCTAssertEqual(Theme.Control.chipGlyphSize, Theme.TextSize.xxs.size - 1)
  }

  /// The main window's minimum is the UI smoke test's review size; the page
  /// lays its columns out inside it.
  @MainActor func testMainWindowMinimumIsTheReviewSize() {
    XCTAssertEqual(MainWindow.minimumSize, CGSize(width: 960, height: 600))
  }

  func testMotionTokensMirrorMobileAndTheWebUI() {
    XCTAssertEqual(Motion.durationFunctional, 0.150)
    XCTAssertEqual(Motion.durationExit, 0.120)
    XCTAssertEqual(Motion.durationEntrance, 0.250)
  }

  /// The Mac press recipe (2 % scale, 5 % dim, half opacity disabled) and
  /// the swap rule: `functional`, and nothing under Reduce Motion.
  func testMacPressAndSwapRules() {
    XCTAssertEqual(Motion.controlPressScale, 0.98)
    XCTAssertEqual(Motion.controlPressOpacity, 0.95)
    XCTAssertEqual(Motion.disabledOpacity, 0.5)
    XCTAssertNil(Motion.swap(reduceMotion: true))
    XCTAssertEqual(Motion.swap(reduceMotion: false), Motion.functional)
  }

  /// The two 1 Hz tokens and their Reduce Motion rules: the countdown
  /// hairline steps without animation, the pulse holds at opacity 1.
  func testCountdownAndPulseHoldUnderReduceMotion() {
    XCTAssertEqual(Motion.durationCountdown, 1)
    XCTAssertEqual(Motion.durationPulse, 1)
    XCTAssertEqual(Motion.pulseOpacity, 0.5)
    XCTAssertNil(Motion.countdown(reduceMotion: true))
    XCTAssertEqual(Motion.countdown(reduceMotion: false), Motion.countdown)
    XCTAssertNil(Motion.pulse(reduceMotion: true))
    XCTAssertEqual(Motion.pulse(reduceMotion: false), Motion.pulse)
  }

  // MARK: - Helpers

  /// The `--color-*` names declared in `mobile/global.css`.
  private func mobileColorNames() throws -> Set<String> {
    let css = try String(
      contentsOf: TestSupport.repositoryRoot.appendingPathComponent("mobile/global.css"),
      encoding: .utf8)
    let pattern = try NSRegularExpression(pattern: "--color-([a-z0-9-]+):")
    let range = NSRange(css.startIndex..., in: css)
    let names = Set(
      pattern.matches(in: css, range: range).compactMap { match -> String? in
        guard let nameRange = Range(match.range(at: 1), in: css) else { return nil }
        return String(css[nameRange])
      })
    XCTAssertFalse(names.isEmpty, "no --color-* tokens found in global.css")
    return names
  }

  /// The six-digit hex value of `variable` inside the `selector { ... }`
  /// block of `css`, as an opaque colour; nil when the block or the
  /// variable is missing or not a plain hex value.
  private func cssColor(_ variable: String, inBlock selector: String, of css: String) throws
    -> Theme.RGBA?
  {
    let block = try NSRegularExpression(
      pattern: NSRegularExpression.escapedPattern(for: selector) + "\\s*\\{([^}]*)\\}")
    let range = NSRange(css.startIndex..., in: css)
    guard let match = block.firstMatch(in: css, range: range),
      let bodyRange = Range(match.range(at: 1), in: css)
    else { return nil }
    let body = String(css[bodyRange])
    let declaration = try NSRegularExpression(
      pattern: NSRegularExpression.escapedPattern(for: variable) + ":\\s*#([0-9a-fA-F]{6})\\s*;")
    guard
      let value = declaration.firstMatch(in: body, range: NSRange(body.startIndex..., in: body)),
      let hexRange = Range(value.range(at: 1), in: body),
      let number = UInt32(body[hexRange], radix: 16)
    else { return nil }
    return Theme.hex(number)
  }

  private func assertEqual(
    _ actual: Theme.RGBA, _ expected: Theme.RGBA, _ label: String,
    file: StaticString = #filePath, line: UInt = #line
  ) {
    XCTAssertEqual(actual.red, expected.red, accuracy: 0.0005, label, file: file, line: line)
    XCTAssertEqual(actual.green, expected.green, accuracy: 0.0005, label, file: file, line: line)
    XCTAssertEqual(actual.blue, expected.blue, accuracy: 0.0005, label, file: file, line: line)
    XCTAssertEqual(actual.alpha, expected.alpha, accuracy: 0.0005, label, file: file, line: line)
  }

  @MainActor private func assertResolves(
    _ token: Theme.Token, under name: NSAppearance.Name, to expected: Theme.RGBA,
    file: StaticString = #filePath, line: UInt = #line
  ) throws {
    let appearance = try XCTUnwrap(NSAppearance(named: name), file: file, line: line)
    var resolved: NSColor?
    appearance.performAsCurrentDrawingAppearance {
      resolved = token.nsColor.usingColorSpace(.sRGB)
    }
    let label = "\(token.cssName) under \(name.rawValue)"
    let color = try XCTUnwrap(resolved, label, file: file, line: line)
    XCTAssertEqual(color.redComponent, expected.red, accuracy: 0.002, label, file: file, line: line)
    XCTAssertEqual(
      color.greenComponent, expected.green, accuracy: 0.002, label, file: file, line: line)
    XCTAssertEqual(
      color.blueComponent, expected.blue, accuracy: 0.002, label, file: file, line: line)
    XCTAssertEqual(
      color.alphaComponent, expected.alpha, accuracy: 0.002, label, file: file, line: line)
  }
}
