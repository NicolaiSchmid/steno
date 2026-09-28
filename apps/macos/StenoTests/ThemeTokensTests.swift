import AppKit
import XCTest

/// Every `--color-*` token in `mobile/global.css` has a Swift counterpart in
/// `Theme.tokens`, and nothing in `Theme.tokens` is unknown to the CSS.
final class ThemeTokensTests: XCTestCase {
  func testEveryCSSColorTokenHasASwiftCounterpart() throws {
    let css = try String(
      contentsOf: TestSupport.repositoryRoot.appendingPathComponent("mobile/global.css"),
      encoding: .utf8)
    let pattern = try NSRegularExpression(pattern: "--color-([a-z0-9-]+):")
    let range = NSRange(css.startIndex..., in: css)
    let cssNames = Set(
      pattern.matches(in: css, range: range).compactMap { match -> String? in
        guard let nameRange = Range(match.range(at: 1), in: css) else { return nil }
        return String(css[nameRange])
      })
    XCTAssertFalse(cssNames.isEmpty, "no --color-* tokens found in global.css")

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
    let cssNames = Set(Theme.tokens.map(\.cssName))
    for token in Theme.macTokens {
      XCTAssertFalse(cssNames.contains(token.cssName), "\(token.cssName) collides with the CSS")
      try assertResolves(token, under: .aqua, to: token.light)
      try assertResolves(token, under: .darkAqua, to: token.dark)
    }
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

  /// `sidebar` is `background` with the `card` veil composited on it, not a
  /// hand-picked grey: `#f2f2f2` in light (242.25 / 255 before rounding),
  /// `#080808` in dark (7.9 / 255).
  func testSidebarIsBackgroundUnderTheCardVeil() {
    let expected: [(name: String, actual: Theme.RGBA, grey: Double)] = [
      ("light", Theme.sidebar.light, 242.25 / 255), ("dark", Theme.sidebar.dark, 7.9 / 255),
    ]
    for pair in expected {
      XCTAssertEqual(pair.actual.red, pair.grey, accuracy: 0.0005, pair.name)
      XCTAssertEqual(pair.actual.green, pair.grey, accuracy: 0.0005, pair.name)
      XCTAssertEqual(pair.actual.blue, pair.grey, accuracy: 0.0005, pair.name)
      XCTAssertEqual(pair.actual.alpha, 1, pair.name)
    }
  }

  /// Every spacing step except `xxs` and `hairline` sits on the 4 pt grid,
  /// and the radii descend one step per nest.
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

    let radii = Theme.Radius.allCases.map(\.rawValue)
    XCTAssertEqual(radii, [16, 12, 8, 6, 4])
    for (outer, inner) in zip(radii, radii.dropFirst()) {
      XCTAssertGreaterThan(outer, inner, "radii must strictly descend")
    }
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
}
