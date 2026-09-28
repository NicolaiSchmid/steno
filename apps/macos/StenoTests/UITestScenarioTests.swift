import XCTest

@MainActor
final class UITestScenarioTests: XCTestCase {
  func testParsesTheFlagsAndReportsUnknownOnes() {
    let none = UITestScenario(arguments: ["/Applications/Steno.app/Contents/MacOS/Steno"])
    XCTAssertFalse(none.isUITesting)
    XCTAssertEqual(none.seed, .sample)
    XCTAssertFalse(none.startsRecording)
    XCTAssertEqual(none.unknownFlags, [])

    let rich = UITestScenario(arguments: [
      "Steno", "-steno-ui-testing", "-steno-rich-seed", "-steno-start-recording",
    ])
    XCTAssertTrue(rich.isUITesting)
    XCTAssertEqual(rich.seed, .rich)
    XCTAssertTrue(rich.startsRecording)
    XCTAssertEqual(rich.unknownFlags, [])

    let empty = UITestScenario(arguments: [
      "Steno", "-steno-ui-testing", "-steno-empty", "-steno-rich-seed",
    ])
    XCTAssertNil(empty.seed, "an empty store wins over a seed")

    let hold = UITestScenario(arguments: [
      "Steno", "-steno-ui-testing", AppEnvironment.holdTranscribeArgument,
    ])
    XCTAssertEqual(hold.unknownFlags, [], "the transcribe hold is a known flag")

    let typo = UITestScenario(arguments: [
      "Steno", "-steno-ui-testing", "-steno-rich-sed", "-NSDocumentRevisionsDebugMode", "YES",
    ])
    XCTAssertEqual(typo.unknownFlags, ["-steno-rich-sed"])
    XCTAssertEqual(typo.seed, .sample)
  }
}
