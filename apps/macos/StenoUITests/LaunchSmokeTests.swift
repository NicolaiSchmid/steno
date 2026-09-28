import XCTest

/// The UI smoke tests: the app launches in its UI-testing mode (preview
/// environment: in-memory database seeded with StenoCore's sample meeting,
/// fakes, no permission prompts), the main window appears, the fixture
/// meeting is listed and each of the four tabs is selectable and shows its
/// content; and with the transcribe hold the processing card follows the
/// run and makes way for the summary. The app cannot join the SwiftPM
/// end-to-end test target, so these tests and the manual checklist are its
/// end-to-end proof.
@MainActor
final class LaunchSmokeTests: XCTestCase {
  /// Launches with `arguments`, waits for the window and selects the
  /// fixture meeting.
  private func launchAndSelectTheFixtureMeeting(_ arguments: [String]) -> XCUIApplication {
    let app = XCUIApplication()
    app.launchArguments = arguments
    app.launch()

    XCTAssertEqual(app.state, .runningForeground)
    let window = app.windows.firstMatch
    XCTAssertTrue(window.waitForExistence(timeout: 10), "no window appeared")

    let meeting = app.staticTexts["Produktstrategie 90/10"].firstMatch
    XCTAssertTrue(meeting.waitForExistence(timeout: 10), "the fixture meeting is not listed")
    if meeting.isHittable { meeting.click() }
    return app
  }

  func testMainWindowOpens() throws {
    let app = launchAndSelectTheFixtureMeeting(["-steno-ui-testing"])

    // Text from StenoCore's SampleData, one per tab; the scratchpad is the
    // one editor.
    let expectations: [(tab: String, check: () -> Bool)] = [
      ("summary", { app.staticTexts["Executive Summary"].firstMatch.waitForExistence(timeout: 5) }),
      (
        "transcript",
        {
          app.staticTexts["Wir setzen neunzig Prozent auf den Kern."].firstMatch
            .waitForExistence(timeout: 5)
        }
      ),
      ("tasks", { app.staticTexts["Budgetzahlen prüfen"].firstMatch.waitForExistence(timeout: 5) }),
      ("scratchpad", { app.textViews.firstMatch.waitForExistence(timeout: 5) }),
    ]
    for expectation in expectations {
      let button = app.buttons["tab-\(expectation.tab)"].firstMatch
      XCTAssertTrue(button.waitForExistence(timeout: 10), "tab \(expectation.tab) missing")
      button.click()
      XCTAssertTrue(expectation.check(), "tab \(expectation.tab) content missing")
    }
  }

  /// `-steno-ui-testing-hold-transcribe` queues the seeded meeting at launch
  /// and holds the fake engine sixty seconds per lane: the card appears on
  /// the Summary tab, its stage reads "Transcribing…", its bar carries a
  /// percentage sampled while the stage is transcribe (the presenter never
  /// crosses the next event's fraction, so a value read under that title is
  /// within the transcribe share) that has moved off zero by the second
  /// 1 Hz sample, and once the run is through the card is gone and the
  /// summary the fake summarizer wrote shows the template's first heading.
  /// The run starts at launch, before the test has a window, so every wait
  /// below is sized from the test's own readiness: the card must still be
  /// in transcribe after the launch waits, and the card-gone wait covers
  /// both lanes' hold from the moment the card was found.
  func testProcessingCardFollowsAHeldRun() throws {
    let app = launchAndSelectTheFixtureMeeting([
      "-steno-ui-testing", "-steno-ui-testing-hold-transcribe",
    ])

    let card = app.descendants(matching: .any)["processing-card"].firstMatch
    XCTAssertTrue(card.waitForExistence(timeout: 20), "the processing card did not appear")

    let stage = app.staticTexts["processing-stage"].firstMatch
    let bar = app.descendants(matching: .any)["processing-bar"].firstMatch
    // A failure here names what the card showed instead, so a run that
    // never left "Waiting to process" reads differently from one whose
    // elements were not exposed.
    let deadline = Date().addingTimeInterval(20)
    while stage.label != "Transcribing…", Date() < deadline {
      RunLoop.current.run(until: Date().addingTimeInterval(0.5))
    }
    XCTAssertEqual(
      stage.label, "Transcribing…",
      """
      stage exists \(stage.exists), bar exists \(bar.exists), bar label \(bar.label), \
      bar value \(String(describing: bar.value)); card: \
      \(card.debugDescription.prefix(2500))
      """)
    XCTAssertTrue(bar.exists, "the bar is missing")
    /// The percent from the bar's label, "Processing progress, 12 percent".
    func percent() throws -> Int {
      let words = bar.label.split(whereSeparator: { $0 == " " || $0 == "," })
      let digits = words.first { Int($0) != nil }
      return try XCTUnwrap(digits.flatMap { Int($0) }, "not a percentage: \(bar.label)")
    }
    let first = try percent()
    XCTAssertTrue((0..<100).contains(first), "\(first) percent is not a fraction under way")
    XCTAssertEqual(stage.label, "Transcribing…", "the bar was read outside the transcribe stage")
    XCTAssertTrue(app.staticTexts["processing-remaining"].firstMatch.exists)
    // The presenter moves the bar from the event's fraction towards the
    // next event once a second; by the second sample it is off zero.
    let moved = expectation(
      for: NSPredicate { _, _ in ((try? percent()) ?? 0) > 0 }, evaluatedWith: NSNull())
    wait(for: [moved], timeout: 10)
    let second = try percent()
    XCTAssertGreaterThan(second, 0, "the bar has not moved by the second sample")
    XCTAssertTrue((0..<100).contains(second))
    XCTAssertEqual(stage.label, "Transcribing…")

    // Two lanes at sixty seconds each from the run's start at launch, then
    // the fakes finish in well under a second; the card leaves with the
    // meeting's `.ready` state.
    XCTAssertTrue(card.waitForNonExistence(timeout: 150), "the card outlived the run")
    XCTAssertTrue(
      app.staticTexts["Executive Summary"].firstMatch.waitForExistence(timeout: 10),
      "the summary did not replace the card")
  }
}
