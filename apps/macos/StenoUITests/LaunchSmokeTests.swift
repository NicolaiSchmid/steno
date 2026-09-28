import XCTest

/// The UI smoke tests: the app launches in its UI-testing mode (preview
/// environment: in-memory database seeded with StenoCore's sample meeting,
/// fakes, synthetic audio, no permission prompts), the main window appears,
/// the fixture meeting is listed and each of the four tabs is selectable and
/// shows its content; and the sidebar control starts and stops a recording.
/// The app cannot join the SwiftPM end-to-end test target, so these tests
/// and the manual checklist are its end-to-end proof. Identifiers are
/// pinned, never copy: titles and chip texts belong to other plans.
@MainActor
final class LaunchSmokeTests: XCTestCase {
  func testMainWindowOpens() throws {
    let app = XCUIApplication()
    app.launchArguments = ["-steno-ui-testing"]
    app.launch()

    XCTAssertEqual(app.state, .runningForeground)
    let window = app.windows.firstMatch
    XCTAssertTrue(window.waitForExistence(timeout: 10), "no window appeared")

    let meeting = app.staticTexts["Produktstrategie 90/10"].firstMatch
    XCTAssertTrue(meeting.waitForExistence(timeout: 10), "the fixture meeting is not listed")
    if meeting.isHittable { meeting.click() }

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

  /// The preview environment has no LLM endpoint and no vault, so the setup
  /// banner shows over the detail pane with both fixes. "Set up summaries"
  /// opens Settings on the LLM tab (`settings-llm` is in the hierarchy only
  /// while that tab is selected, which also proves the request was applied
  /// and cleared by the scene); "Not now" hides the banner for the launch.
  func testSetupBannerLinksToSettingsAndHides() throws {
    let app = XCUIApplication()
    app.launchArguments = ["-steno-ui-testing"]
    app.launch()

    let window = app.windows.firstMatch
    XCTAssertTrue(window.waitForExistence(timeout: 10), "no window appeared")
    let setup = app.buttons["setup-summaries"].firstMatch
    XCTAssertTrue(setup.waitForExistence(timeout: 10), "the banner's summaries button is missing")
    XCTAssertTrue(
      app.buttons["choose-vault"].firstMatch.exists, "the banner's vault button is missing")
    let notNow = app.buttons["banner-not-now"].firstMatch
    XCTAssertTrue(notNow.exists, "the banner's Not now is missing")

    setup.click()
    let llmTab = app.descendants(matching: .any)["settings-llm"].firstMatch
    XCTAssertTrue(llmTab.waitForExistence(timeout: 10), "Settings did not open on the LLM tab")
    let screenshot = XCTAttachment(screenshot: XCUIScreen.main.screenshot())
    screenshot.name = "setup-banner-and-llm-settings"
    screenshot.lifetime = .keepAlways
    add(screenshot)

    notNow.click()
    XCTAssertTrue(
      waitUntil(timeout: 5) { !app.buttons["setup-summaries"].firstMatch.exists },
      "Not now did not hide the banner")
    XCTAssertFalse(app.buttons["banner-not-now"].firstMatch.exists)
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
}
