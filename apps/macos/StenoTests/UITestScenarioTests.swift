import XCTest

/// The launch-argument parser the UI smoke tests rely on: known flags parse,
/// a misspelt `-steno-*` flag becomes the launch error the window shows.
final class UITestScenarioTests: XCTestCase {
  func testTheKnownFlagsParse() {
    let none = UITestScenario(arguments: ["/Applications/Steno.app/Contents/MacOS/Steno"])
    XCTAssertFalse(none.isUITesting)
    XCTAssertFalse(none.showPrompt)
    XCTAssertFalse(none.holdTranscribe)
    XCTAssertEqual(none.seed, .sample)
    XCTAssertFalse(none.startsRecording)
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

    let hold = UITestScenario(arguments: [
      "Steno", "-steno-ui-testing", "-steno-ui-testing-hold-transcribe",
    ])
    XCTAssertTrue(hold.holdTranscribe, "the processing card's flag is known")
    XCTAssertEqual(hold.unknownFlags, [])
    XCTAssertNil(hold.launchError)
  }

  /// The seed and recording flags of the redesign's smoke tests: rich seed,
  /// empty store (which wins over a seed) and a recording started at launch.
  func testTheSeedAndRecordingFlagsParse() {
    let rich = UITestScenario(arguments: [
      "Steno", "-steno-ui-testing", "-steno-rich-seed", "-steno-start-recording",
    ])
    XCTAssertTrue(rich.isUITesting)
    XCTAssertEqual(rich.seed, .rich)
    XCTAssertTrue(rich.startsRecording)
    XCTAssertEqual(rich.unknownFlags, [])
    XCTAssertNil(rich.launchError)

    let empty = UITestScenario(arguments: [
      "Steno", "-steno-ui-testing", "-steno-empty", "-steno-rich-seed",
    ])
    XCTAssertNil(empty.seed, "an empty store wins over a seed")
    XCTAssertFalse(empty.startsRecording)
    XCTAssertEqual(empty.unknownFlags, [])

    let typo = UITestScenario(arguments: [
      "Steno", "-steno-ui-testing", "-steno-rich-sed", "-NSDocumentRevisionsDebugMode", "YES",
    ])
    XCTAssertEqual(typo.unknownFlags, ["-steno-rich-sed"])
    XCTAssertEqual(typo.seed, .sample, "a misspelt seed flag does not change the seed")
    XCTAssertEqual(typo.launchError, "Unknown UI-test flags: -steno-rich-sed")
  }

  /// The flag `AppEnvironment.preview()` reads is the one the parser knows.
  @MainActor func testTheHoldTranscribeFlagIsTheEnvironmentsArgument() {
    XCTAssertEqual(UITestScenario.holdTranscribeFlag, AppEnvironment.holdTranscribeArgument)
  }

  func testAnUnknownStenoFlagIsReported() {
    let scenario = UITestScenario(arguments: ["Steno", "-steno-ui-testing", "-steno-show-promt"])
    XCTAssertTrue(scenario.isUITesting)
    XCTAssertFalse(scenario.showPrompt)
    XCTAssertEqual(scenario.unknownFlags, ["-steno-show-promt"])
  }

  /// `AppBootstrap.load()` shows `launchError` instead of the app, so the
  /// smoke test fails on the reason and not on a later timeout.
  func testUnknownFlagsBecomeTheLaunchError() {
    XCTAssertNil(
      UITestScenario(arguments: ["Steno", "-steno-ui-testing", "-steno-show-prompt"]).launchError)
    XCTAssertEqual(
      UITestScenario(arguments: ["Steno", "-steno-ui-testing", "-steno-show-promt", "-steno-x"])
        .launchError,
      "Unknown UI-test flags: -steno-show-promt, -steno-x")
    XCTAssertNil(
      UITestScenario(arguments: ["Steno", "-steno-show-promt"]).launchError,
      "outside UI testing a stray flag is not ours to report")
  }
}
