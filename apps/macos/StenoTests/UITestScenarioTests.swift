import XCTest

/// The launch-argument parser the UI smoke tests rely on: known flags parse,
/// a misspelt `-steno-*` flag is reported instead of ignored.
final class UITestScenarioTests: XCTestCase {
  func testKnownFlagsParse() {
    let none = UITestScenario(arguments: ["/Applications/Steno.app/Contents/MacOS/Steno"])
    XCTAssertFalse(none.isUITesting)
    XCTAssertFalse(none.showPrompt)
    XCTAssertEqual(none.unknownFlags, [])

    let testing = UITestScenario(arguments: [
      "Steno", "-steno-ui-testing", "-NSSomeAppKitFlag", "YES",
    ])
    XCTAssertTrue(testing.isUITesting)
    XCTAssertFalse(testing.showPrompt)
    XCTAssertEqual(testing.unknownFlags, [], "flags outside the prefix are not ours")

    let prompt = UITestScenario(arguments: ["Steno", "-steno-ui-testing", "-steno-show-prompt"])
    XCTAssertTrue(prompt.isUITesting)
    XCTAssertTrue(prompt.showPrompt)
    XCTAssertEqual(prompt.unknownFlags, [])
  }

  func testAnUnknownStenoFlagIsReported() {
    let scenario = UITestScenario(arguments: ["Steno", "-steno-ui-testing", "-steno-show-promt"])
    XCTAssertTrue(scenario.isUITesting)
    XCTAssertFalse(scenario.showPrompt)
    XCTAssertEqual(scenario.unknownFlags, ["-steno-show-promt"])
  }
}
