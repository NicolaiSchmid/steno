import AppKit
import SwiftUI
import XCTest

/// Every `--color-*` token in `mobile/global.css` has a Swift counterpart in
/// `Theme.tokens`, and nothing in `Theme.tokens` is unknown to the CSS. The
/// Mac-only tokens, control boxes and press rules are pinned to the plan's
/// tables in `.plans/2026-09-28-macos-visual-redesign.md`.
final class ThemeTokensTests: XCTestCase {
  func testEveryCSSColorTokenHasASwiftCounterpart() throws {
    let cssNames = try cssColorNames()
    let swiftNames = Set(Theme.tokens.map(\.cssName))
    XCTAssertEqual(cssNames.subtracting(swiftNames), [], "CSS tokens without a Swift token")
    XCTAssertEqual(swiftNames.subtracting(cssNames), [], "Swift tokens without a CSS token")
    XCTAssertEqual(Theme.tokens.count, swiftNames.count, "duplicate token names")
  }

  func testTokensResolveToDynamicColors() {
    for token in Theme.tokens {
      let color = token.nsColor
      XCTAssertNotNil(color.usingColorSpace(.sRGB), token.cssName)
    }
  }

  /// The Mac-only surfaces resolve to their light and dark values under the
  /// matching appearance and never take a name the CSS owns.
  @MainActor func testMacTokensResolveInBothAppearancesAndAvoidCSSNames() throws {
    XCTAssertEqual(Set(Theme.macTokens.map(\.cssName)), ["sidebar", "raised"])
    let cssNames = try cssColorNames()
    for token in Theme.macTokens {
      XCTAssertFalse(cssNames.contains(token.cssName), "\(token.cssName) collides with the CSS")
      try assertResolves(token, under: .aqua, to: token.light)
      try assertResolves(token, under: .darkAqua, to: token.dark)
    }
  }

  /// `raised` is opaque white on the light canvas and the 3.1 % white veil
  /// on the dark one.
  func testRaisedMatchesThePlan() {
    assertEqual(Theme.raised.light, (red: 1, green: 1, blue: 1, alpha: 1), "raised light")
    assertEqual(Theme.raised.dark, (red: 1, green: 1, blue: 1, alpha: 0.031), "raised dark")
  }

  /// `sidebar` is `background` with the `card` veil composited on it, not a
  /// hand-picked grey: `#f2f2f2` in light (242.25 / 255 before rounding),
  /// `#080808` in dark (7.9 / 255).
  func testSidebarIsBackgroundUnderTheCardVeil() {
    assertEqual(
      Theme.sidebar.light, Theme.composite(Theme.card.light, over: Theme.background.light),
      "sidebar light is the composite")
    assertEqual(
      Theme.sidebar.dark, Theme.composite(Theme.card.dark, over: Theme.background.dark),
      "sidebar dark is the composite")

    let expected: [(name: String, actual: Theme.RGBA, grey: Double)] = [
      ("light", Theme.sidebar.light, 242.25 / 255), ("dark", Theme.sidebar.dark, 7.9 / 255),
    ]
    for pair in expected {
      assertEqual(
        pair.actual, (red: pair.grey, green: pair.grey, blue: pair.grey, alpha: 1), pair.name)
    }
  }

  /// Every spacing step except `xxs` and `hairline` sits on the 4 pt grid,
  /// and the radii descend one step per nest: 16 > 12 > 8 > 6 > 4.
  func testSpaceSitsOnTheGridAndRadiiDescend() {
    let steps: [(name: String, value: CGFloat)] = [
      ("xs", Theme.Space.xs), ("sm", Theme.Space.sm), ("md", Theme.Space.md),
      ("lg", Theme.Space.lg), ("xl", Theme.Space.xl), ("xxl", Theme.Space.xxl),
      ("xxxl", Theme.Space.xxxl),
    ]
    for step in steps {
      XCTAssertEqual(
        step.value.truncatingRemainder(dividingBy: 4), 0, "Space.\(step.name) is off the 4 pt grid")
    }
    XCTAssertEqual(Theme.Space.xxs, 2)
    XCTAssertEqual(Theme.Space.hairline, 1)
    XCTAssertEqual(Theme.Radius.allCases.map(\.rawValue), [16, 12, 8, 6, 4])
  }

  /// The plan's control heights (CTA 40, buttons 32, inputs 28, nav rows 32,
  /// icon buttons 28, segmented cells 24 in a 28 container) and insets.
  func testControlBoxesMatchThePlan() {
    XCTAssertEqual(Theme.Control.ctaHeight, 40)
    XCTAssertEqual(Theme.Control.buttonHeight, 32)
    XCTAssertEqual(Theme.Control.inputHeight, 28)
    XCTAssertEqual(Theme.Control.navRowHeight, 32)
    XCTAssertEqual(Theme.Control.iconButtonSize, 28)
    XCTAssertEqual(Theme.Control.segmentHeight, 24)
    XCTAssertEqual(Theme.Control.segmentContainerHeight, 28)
    XCTAssertEqual(Theme.Control.buttonInset, 14)
    XCTAssertEqual(Theme.Control.chipInset, 6)
    XCTAssertEqual(Theme.Control.rowInset, 10)
    XCTAssertEqual(Theme.Control.menuRowInset, 6)
  }

  func testMotionTokensMirrorMobile() {
    XCTAssertEqual(Motion.durationFunctional, 0.150)
    XCTAssertEqual(Motion.durationExit, 0.120)
    XCTAssertEqual(Motion.durationEntrance, 0.250)
    XCTAssertEqual(Motion.durationPressIn, 0.100)
    XCTAssertEqual(Motion.springDamping, 28)
    XCTAssertEqual(Motion.springStiffness, 320)
    XCTAssertEqual(Motion.pressScale, 0.97)
    XCTAssertEqual(Motion.pressOpacity, 0.85)
    XCTAssertEqual(Motion.hitSlop, 10)
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
  private func cssColorNames() throws -> Set<String> {
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
