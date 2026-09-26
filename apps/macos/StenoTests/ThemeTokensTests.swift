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
