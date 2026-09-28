import XCTest

/// The UI smoke tests: the app launches in its UI-testing mode (preview
/// environment: in-memory database seeded with StenoCore's sample meeting,
/// fakes, synthetic audio, no permission prompts), the main window appears,
/// the fixture meeting is listed and each of the four tabs is selectable and
/// shows its content; the sidebar control starts and stops a recording; and
/// with the transcribe hold the processing card follows the run and makes
/// way for the summary. The app cannot join the SwiftPM end-to-end test
/// target, so these tests and the manual checklist are its end-to-end
/// proof. Identifiers are pinned, never copy: titles and chip texts belong
/// to other plans.
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

    // Speakers: the header row names the confirmed speaker and counts the
    // unnamed one; its popover lists both, and the unnamed speaker's picker
    // opens pre-filled with the suggested name. Ids are SampleData's
    // `uuid(20)` and `uuid(21)`.
    let speakersRow = app.buttons["speakers-row"].firstMatch
    XCTAssertTrue(speakersRow.waitForExistence(timeout: 10), "no speakers row")
    XCTAssertTrue(speakersRow.label.contains("Nicolai"), speakersRow.label)
    XCTAssertTrue(speakersRow.label.contains("1 to confirm"), speakersRow.label)
    speakersRow.click()
    XCTAssertTrue(app.popovers.firstMatch.waitForExistence(timeout: 10), "no speakers popover")
    let speakerOne = "00000000-0000-0000-0000-000000000014"
    let speakerTwo = "00000000-0000-0000-0000-000000000015"
    let firstPicker = app.buttons["speaker-picker-\(speakerOne)"].firstMatch
    XCTAssertTrue(firstPicker.waitForExistence(timeout: 5), "Speaker 1 row missing")
    XCTAssertTrue(firstPicker.label.contains("Nicolai"), firstPicker.label)
    let secondPicker = app.buttons["speaker-picker-\(speakerTwo)"].firstMatch
    XCTAssertTrue(secondPicker.waitForExistence(timeout: 5), "Speaker 2 row missing")
    secondPicker.click()
    let field = app.textFields["speaker-field-\(speakerTwo)"].firstMatch
    XCTAssertTrue(field.waitForExistence(timeout: 5), "the picker field did not open")
    XCTAssertEqual(field.value as? String, "Jérôme", "pre-filled with the suggested name")
    app.typeKey(.escape, modifierFlags: [])
    if app.popovers.firstMatch.exists { app.typeKey(.escape, modifierFlags: []) }

    // Transcript: the turn header's name is the same picker, in its own
    // popover. The screenshot is the review evidence for the speaker
    // surfaces.
    app.buttons["tab-transcript"].firstMatch.click()
    let turnPicker = app.buttons["speaker-picker-\(speakerOne)"].firstMatch
    XCTAssertTrue(turnPicker.waitForExistence(timeout: 10), "no picker on the turn header")
    XCTAssertTrue(turnPicker.label.contains("Nicolai"), turnPicker.label)
    turnPicker.click()
    let turnField = app.textFields["speaker-field-\(speakerOne)"].firstMatch
    XCTAssertTrue(turnField.waitForExistence(timeout: 5), "the transcript picker did not open")
    let screenshot = XCTAttachment(screenshot: XCUIScreen.main.screenshot())
    screenshot.name = "transcript-speaker-picker"
    screenshot.lifetime = .keepAlways
    add(screenshot)
    app.typeKey(.escape, modifierFlags: [])
  }

  /// Record call in the sidebar, the control turns into Stop and the live
  /// row joins the fixture meeting; Stop returns the control and keeps the
  /// row. The synthetic backend delivers audio at once, so this stays well
  /// under 30 seconds.
  func testSidebarStartsAndStopsARecording() throws {
    let app = XCUIApplication()
    app.launchArguments = ["-steno-ui-testing"]
    app.launch()

    let window = app.windows.firstMatch
    XCTAssertTrue(window.waitForExistence(timeout: 10), "no window appeared")

    let record = app.buttons["sidebar-record"].firstMatch
    XCTAssertTrue(record.waitForExistence(timeout: 10), "the sidebar record control is missing")
    XCTAssertFalse(app.buttons["sidebar-stop"].exists, "nothing in the window records on its own")
    XCTAssertTrue(
      waitUntil(timeout: 10) { meetingRowCount(in: app) == 1 },
      "expected the fixture meeting alone before the click, got \(meetingRowCount(in: app))")
    record.click()

    let stop = app.buttons["sidebar-stop"].firstMatch
    XCTAssertTrue(stop.waitForExistence(timeout: 10), "the control did not turn into Stop")
    XCTAssertTrue(
      waitUntil(timeout: 20) { meetingRowCount(in: app) == 2 },
      "expected the fixture meeting plus the live row, got \(meetingRowCount(in: app))")

    stop.click()
    XCTAssertTrue(record.waitForExistence(timeout: 10), "the control did not return to Record call")
    XCTAssertEqual(meetingRowCount(in: app), 2, "the stopped recording keeps its row")
  }

  /// Rows carrying a `meeting-<uuid>` identifier, and only those: the
  /// redesign's `meeting-list` container must not count. The list's cells
  /// are counted with one query when they carry the row identifier; SwiftUI
  /// otherwise stamps a row's identifier on each of the row's text elements
  /// as well, so the fallback counts distinct identifiers, not elements.
  private func meetingRowCount(in app: XCUIApplication) -> Int {
    let row = NSPredicate(format: "identifier MATCHES %@", "meeting-[0-9A-F-]{36}")
    let cells = app.descendants(matching: .cell).matching(row).count
    if cells > 0 { return cells }
    let elements = app.descendants(matching: .any).matching(row)
    return Set(elements.allElementsBoundByIndex.map(\.identifier)).count
  }

  /// Polls `condition` on the main run loop until it holds or `timeout` passes.
  private func waitUntil(timeout: TimeInterval, _ condition: () -> Bool) -> Bool {
    let deadline = Date().addingTimeInterval(timeout)
    while !condition() {
      guard Date() < deadline else { return false }
      RunLoop.current.run(until: Date().addingTimeInterval(0.5))
    }
    return true
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
    /// A SwiftUI `Text` exposes its string as the element's value on macOS;
    /// `label` is empty.
    func stageTitle() -> String { (stage.value as? String) ?? stage.label }
    let bar = app.descendants(matching: .any)["processing-bar"].firstMatch
    // A failure here names what the card showed instead, so a run that
    // never left "Waiting to process" reads differently from one whose
    // elements were not exposed.
    let deadline = Date().addingTimeInterval(20)
    while stageTitle() != "Transcribing…", Date() < deadline {
      RunLoop.current.run(until: Date().addingTimeInterval(0.5))
    }
    XCTAssertEqual(
      stageTitle(), "Transcribing…",
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
    XCTAssertEqual(stageTitle(), "Transcribing…", "the bar was read outside the transcribe stage")
    XCTAssertTrue(app.staticTexts["processing-remaining"].firstMatch.exists)
    // The presenter moves the bar from the event's fraction towards the
    // next event once a second; by the second sample it is off zero.
    let moved = expectation(
      for: NSPredicate { _, _ in ((try? percent()) ?? 0) > 0 }, evaluatedWith: NSNull())
    wait(for: [moved], timeout: 10)
    let second = try percent()
    XCTAssertGreaterThan(second, 0, "the bar has not moved by the second sample")
    XCTAssertTrue((0..<100).contains(second))
    XCTAssertEqual(stageTitle(), "Transcribing…")

    // Two lanes at sixty seconds each from the run's start at launch, then
    // the fakes finish in well under a second; the card leaves with the
    // meeting's `.ready` state.
    XCTAssertTrue(card.waitForNonExistence(timeout: 150), "the card outlived the run")
    XCTAssertTrue(
      app.staticTexts["Executive Summary"].firstMatch.waitForExistence(timeout: 10),
      "the summary did not replace the card")
  }
}
