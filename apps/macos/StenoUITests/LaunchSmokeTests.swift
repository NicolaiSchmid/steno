import XCTest

/// The UI smoke tests: the app launches in its UI-testing mode (preview
/// environment: in-memory database seeded with StenoCore's sample meeting,
/// fakes, synthetic audio, no permission prompts), the main window appears,
/// the fixture meeting is listed and each of the four tabs is selectable and
/// shows its content; the sidebar control starts and stops a recording; the
/// floating panel shows the detection prompt and the recording bubble; an
/// empty store shows both empty states; and with the transcribe hold the
/// processing card follows the run and makes way for the summary. Every
/// launch runs in UTC so the card headers and derived titles do not move
/// with the runner. The app cannot join the SwiftPM end-to-end test
/// target, so these tests and the manual checklist are its end-to-end
/// proof. Identifiers are pinned, never copy: titles and chip texts belong
/// to other plans.
@MainActor
final class LaunchSmokeTests: XCTestCase {
  /// SampleData's `meetingID`, `uuid(1)`.
  private static let fixtureID = "00000000-0000-0000-0000-000000000001"

  /// The fixture meeting's list entry, a button carrying `meeting-<uuid>`.
  private func fixtureEntry(in app: XCUIApplication) -> XCUIElement {
    app.buttons["meeting-\(Self.fixtureID)"].firstMatch
  }

  /// The app under test, launched in UTC with `arguments`.
  private func launch(_ arguments: [String]) -> XCUIApplication {
    let app = XCUIApplication()
    app.launchArguments = arguments
    app.launchEnvironment["TZ"] = "UTC"
    app.launch()
    return app
  }

  /// Launches with `arguments`, waits for the window and selects the
  /// fixture meeting by clicking its entry, which also gives the list
  /// keyboard focus. The entry carries the fixture's calendar title; the
  /// detail heading is not consulted, so the list is what is proven.
  private func launchAndSelectTheFixtureMeeting(_ arguments: [String]) -> XCUIApplication {
    let app = launch(arguments)

    XCTAssertEqual(app.state, .runningForeground)
    let window = app.windows.firstMatch
    XCTAssertTrue(window.waitForExistence(timeout: 10), "no window appeared")

    let entry = fixtureEntry(in: app)
    XCTAssertTrue(entry.waitForExistence(timeout: 10), "the fixture meeting is not listed")
    XCTAssertTrue(
      entry.label.contains("Produktstrategie 90/10"),
      "the entry does not carry the fixture title: \(entry.label)")
    XCTAssertTrue(entry.isHittable, "the fixture entry cannot be clicked")
    entry.click()
    return app
  }

  /// The nav column and the list column of the redesign: the record control
  /// sits above the filter rows, Failed hides the ready fixture and All
  /// shows it again, search is a field in the column that ⌘F focuses and
  /// "Clear filters" empties, and the toolbar carries neither the old delete
  /// button nor a sidebar toggle.
  func testNavigationColumnFiltersTheListAndTheToolbarIsEmpty() throws {
    let app = launchAndSelectTheFixtureMeeting(["-steno-ui-testing"])

    let record = app.buttons["sidebar-record"].firstMatch
    let failed = app.buttons["nav-failed"].firstMatch
    XCTAssertTrue(record.waitForExistence(timeout: 10), "the record control is missing")
    XCTAssertTrue(failed.waitForExistence(timeout: 10), "the Failed row is missing")
    XCTAssertLessThanOrEqual(
      record.frame.maxY, failed.frame.minY, "the record control sits above the nav rows")
    let all = app.buttons["nav-all"].firstMatch
    XCTAssertTrue(all.isSelected, "All is the selected filter at launch")

    failed.click()
    XCTAssertTrue(
      fixtureEntry(in: app).waitForNonExistence(timeout: 5), "Failed still lists the ready fixture")
    // The no-match empty state is read through its button and its title:
    // the `EmptyState` container's own id is not exposed as an element on
    // macOS, its title's `<id>-title` is.
    XCTAssertTrue(
      app.buttons["clear-filters"].firstMatch.waitForExistence(timeout: 5),
      "no empty state for the empty filter")
    XCTAssertTrue(
      app.staticTexts["empty-meetings-title"].firstMatch.exists, "the empty state has no title")
    XCTAssertTrue(failed.isSelected)
    all.click()
    XCTAssertTrue(fixtureEntry(in: app).waitForExistence(timeout: 5), "All did not show it again")

    let search = app.textFields["search-meetings"].firstMatch
    XCTAssertTrue(search.exists, "the search field is missing from the list column")
    XCTAssertFalse(app.buttons["delete-meeting"].firstMatch.exists, "the trash can is gone")
    let toggles = app.toolbars.buttons.matching(
      NSPredicate(format: "label CONTAINS[c] %@", "sidebar"))
    XCTAssertEqual(toggles.count, 0, "no sidebar toggle in the toolbar")

    let screenshot = XCTAttachment(screenshot: XCUIScreen.main.screenshot())
    screenshot.name = "main-light-selected"
    screenshot.lifetime = .keepAlways
    add(screenshot)

    // ⌘F focuses the field; a query nothing matches empties the list and
    // offers "Clear filters". SwiftUI moves focus on the next run-loop
    // pass and `XCUIElement` exposes no focus attribute on macOS, so the
    // typing lets one pass go by; the value check below is the assertion.
    app.typeKey("f", modifierFlags: .command)
    RunLoop.current.run(until: Date().addingTimeInterval(0.5))
    app.typeText("zzzznothing")
    XCTAssertTrue(
      waitUntil(timeout: 5) { (search.value as? String) == "zzzznothing" },
      "⌘F did not focus the search field; its value is \(String(describing: search.value))")
    let clear = app.buttons["clear-filters"].firstMatch
    XCTAssertTrue(clear.waitForExistence(timeout: 10), "the no-match empty state is missing")
    XCTAssertFalse(fixtureEntry(in: app).exists)
    clear.click()
    XCTAssertTrue(fixtureEntry(in: app).waitForExistence(timeout: 5), "Clear filters did not reset")
  }

  /// The date-grouped cards under the rich seed: the clicked entry exposes
  /// the `isSelected` trait, the fixture day's card header names the
  /// weekday, and the arrow keys move the selection through the entries.
  func testArrowKeysMoveTheSelectionThroughTheCards() throws {
    let app = launchAndSelectTheFixtureMeeting(["-steno-ui-testing", "-steno-rich-seed"])
    let fixture = fixtureEntry(in: app)
    XCTAssertTrue(
      waitUntil(timeout: 5) { fixture.isSelected }, "the clicked entry is not marked selected")

    let entries = app.buttons.matching(
      NSPredicate(format: "identifier MATCHES %@", "meeting-[0-9A-F-]{36}"))
    XCTAssertTrue(
      waitUntil(timeout: 10) { entries.count == 5 }, "expected five entries, got \(entries.count)")
    // 2026-09-24, the fixture's day, is a Thursday in the launch's UTC zone;
    // the fixture's own title is its calendar title, so only the card
    // header can say so.
    let weekday = app.staticTexts.allElementsBoundByIndex.contains { text in
      text.label.contains("Thursday") || ((text.value as? String)?.contains("Thursday") ?? false)
    }
    XCTAssertTrue(weekday, "no card header names the fixture's weekday")

    app.typeKey(.downArrow, modifierFlags: [])
    XCTAssertTrue(
      waitUntil(timeout: 5) { !fixture.isSelected }, "the down arrow left the fixture selected")
    let selected = entries.allElementsBoundByIndex.filter(\.isSelected)
    XCTAssertEqual(selected.count, 1, "exactly one entry is selected")
    XCTAssertNotEqual(selected.first?.identifier, fixture.identifier)

    app.typeKey(.upArrow, modifierFlags: [])
    XCTAssertTrue(
      waitUntil(timeout: 5) { fixture.isSelected }, "the up arrow did not return to the fixture")
  }

  /// `-steno-empty`: nothing is seeded, so the list shows "No meetings yet"
  /// and the detail pane its own empty state, both read through their title
  /// ids; no entry is listed, no "Clear filters" is offered, and the record
  /// control is still there.
  func testEmptyStoreShowsBothEmptyStates() throws {
    let app = launch(["-steno-ui-testing", "-steno-empty"])

    let window = app.windows.firstMatch
    XCTAssertTrue(window.waitForExistence(timeout: 10), "no window appeared")
    XCTAssertTrue(
      app.buttons["sidebar-record"].firstMatch.waitForExistence(timeout: 10),
      "the record control is missing")
    XCTAssertTrue(
      app.staticTexts["empty-meetings-title"].firstMatch.waitForExistence(timeout: 10),
      "the list's empty state is missing")
    XCTAssertTrue(
      app.staticTexts["empty-detail-title"].firstMatch.waitForExistence(timeout: 10),
      "the detail's empty state is missing")
    XCTAssertEqual(meetingRowCount(in: app), 0, "nothing is listed")
    XCTAssertFalse(
      app.buttons["clear-filters"].firstMatch.exists, "an empty store offers no Clear filters")
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
    let app = launch(["-steno-ui-testing"])

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

  /// The floating panel: launched with `-steno-show-prompt`, the detection
  /// prompt for "Zoom" appears; Record turns the same panel into the
  /// recording bubble; the bubble's stop hides it.
  func testPromptMorphsIntoTheBubbleAndStopHidesIt() throws {
    let app = launch(["-steno-ui-testing", "-steno-show-prompt"])

    let record = app.buttons["prompt-record"].firstMatch
    XCTAssertTrue(record.waitForExistence(timeout: 10), "the detection prompt did not appear")
    let title = app.staticTexts["prompt-title"].firstMatch
    XCTAssertTrue(title.exists, "the prompt title is missing")
    // A SwiftUI `Text` exposes its string as the element's value on macOS;
    // `label` is empty.
    let titleText = (title.value as? String) ?? title.label
    XCTAssertTrue(titleText.hasPrefix("Zoom"), "the title names the app: \(titleText)")
    attachScreenshot(named: "prompt.png")
    record.click()

    let stop = app.buttons["bubble-stop"].firstMatch
    XCTAssertTrue(stop.waitForExistence(timeout: 10), "the prompt did not become the bubble")
    let promptGone = XCTNSPredicateExpectation(
      predicate: NSPredicate(format: "exists == false"), object: record)
    XCTAssertEqual(XCTWaiter().wait(for: [promptGone], timeout: 10), .completed)
    XCTAssertTrue(
      waitUntil(timeout: 10) { stop.isEnabled }, "the stop square is enabled once recording")
    attachScreenshot(named: "bubble.png")
    clickCenter(of: stop, in: app)

    let bubbleGone = XCTNSPredicateExpectation(
      predicate: NSPredicate(format: "exists == false"), object: stop)
    XCTAssertEqual(XCTWaiter().wait(for: [bubbleGone], timeout: 10), .completed)
  }

  /// A recording started from the window shows the bubble too; its elapsed
  /// time advances (the one check of the product's 1 Hz clock); clicking its
  /// body brings the window to the live meeting (the selected row). On this
  /// path the stop square must be reachable through accessibility, which
  /// pins the asymmetry `clickCenter` works around.
  func testBubbleFollowsARecordingStartedFromTheWindow() throws {
    let app = launch(["-steno-ui-testing"])

    let window = app.windows.firstMatch
    XCTAssertTrue(window.waitForExistence(timeout: 10), "no window appeared")
    let record = app.buttons["sidebar-record"].firstMatch
    XCTAssertTrue(record.waitForExistence(timeout: 10), "the sidebar record control is missing")
    XCTAssertFalse(app.buttons["bubble-stop"].exists, "no bubble before the recording")
    record.click()

    let stop = app.buttons["bubble-stop"].firstMatch
    XCTAssertTrue(stop.waitForExistence(timeout: 10), "the bubble did not follow the recording")
    let open = app.buttons["bubble-open"].firstMatch
    XCTAssertTrue(open.waitForExistence(timeout: 5), "the bubble body is missing")
    XCTAssertTrue(
      waitUntil(timeout: 10) { !((open.value as? String) ?? "").isEmpty },
      "the bubble shows no elapsed time")
    let firstValue = open.value as? String
    XCTAssertTrue(
      waitUntil(timeout: 5) { (open.value as? String) != firstValue },
      "the elapsed time did not advance from \(firstValue ?? "nil")")
    open.click()
    XCTAssertTrue(
      waitUntil(timeout: 10) { selectedMeetingRowCount(in: app) == 1 },
      "expected the live row selected, got \(selectedMeetingRowCount(in: app))")

    XCTAssertTrue(
      stop.isHittable, "the stop square must stay AX-hittable when the bubble appears fresh")
    stop.click()
    XCTAssertTrue(
      app.buttons["sidebar-record"].firstMatch.waitForExistence(timeout: 10),
      "the control did not return to Record call")
  }

  /// The case the bubble exists for: with the main window closed, clicking
  /// the bubble's body reopens it (`openWindow` captured by `MenuBarLabel`).
  /// Ids only; the window's presence is read through `sidebar-stop`, since
  /// the panel may be listed among `app.windows`.
  func testBubbleOpenReopensAClosedMainWindow() throws {
    let app = launch(["-steno-ui-testing"])

    let window = app.windows.firstMatch
    XCTAssertTrue(window.waitForExistence(timeout: 10), "no window appeared")
    let record = app.buttons["sidebar-record"].firstMatch
    XCTAssertTrue(record.waitForExistence(timeout: 10), "the sidebar record control is missing")
    record.click()
    let stop = app.buttons["bubble-stop"].firstMatch
    XCTAssertTrue(stop.waitForExistence(timeout: 10), "the bubble did not follow the recording")
    XCTAssertTrue(app.buttons["sidebar-stop"].firstMatch.waitForExistence(timeout: 10))

    let close = window.buttons[XCUIIdentifierCloseWindow].firstMatch
    if close.exists {
      close.click()
    } else {
      app.typeKey("w", modifierFlags: .command)
    }
    XCTAssertTrue(
      waitUntil(timeout: 10) { !app.buttons["sidebar-stop"].firstMatch.exists },
      "the main window did not close")

    app.buttons["bubble-open"].firstMatch.click()
    XCTAssertTrue(
      app.buttons["sidebar-stop"].firstMatch.waitForExistence(timeout: 10),
      "the bubble did not bring the main window back")

    stop.click()
    XCTAssertTrue(
      app.buttons["sidebar-record"].firstMatch.waitForExistence(timeout: 10),
      "the control did not return to Record call")
  }

  /// The preview environment has no LLM endpoint and no vault, so the setup
  /// banner shows over the detail pane with both fixes. "Set up summaries"
  /// opens Settings on the Summaries section (`settings-header-summaries` is
  /// in the hierarchy only while that section is selected, which proves the
  /// request was applied; `SettingsSectionTests` pins the request itself);
  /// Settings is closed again
  /// before "Not now", which would otherwise sit under it on the runner's
  /// one display; "Not now" hides the banner for the launch.
  func testSetupBannerLinksToSettingsAndHides() throws {
    let app = launch(["-steno-ui-testing"])

    let window = app.windows.firstMatch
    XCTAssertTrue(window.waitForExistence(timeout: 10), "no window appeared")
    let setup = app.buttons["setup-summaries"].firstMatch
    XCTAssertTrue(setup.waitForExistence(timeout: 10), "the banner's summaries button is missing")
    XCTAssertTrue(
      app.buttons["choose-vault"].firstMatch.exists, "the banner's vault button is missing")
    let notNow = app.buttons["banner-not-now"].firstMatch
    XCTAssertTrue(notNow.exists, "the banner's Not now is missing")

    setup.click()
    let header = app.descendants(matching: .any)["settings-header-summaries"].firstMatch
    XCTAssertTrue(header.waitForExistence(timeout: 10), "Settings did not open on Summaries")
    let screenshot = XCTAttachment(screenshot: XCUIScreen.main.screenshot())
    screenshot.name = "setup-banner-and-llm-settings"
    screenshot.lifetime = .keepAlways
    add(screenshot)

    app.typeKey("w", modifierFlags: .command)
    XCTAssertTrue(waitUntil(timeout: 5) { !header.exists }, "Settings did not close")
    XCTAssertTrue(notNow.isHittable, "the banner's Not now is covered")
    notNow.click()
    XCTAssertTrue(
      waitUntil(timeout: 5) { !app.buttons["setup-summaries"].firstMatch.exists },
      "Not now did not hide the banner")
    XCTAssertFalse(app.buttons["banner-not-now"].firstMatch.exists)
  }

  /// Clicks the element's centre. After the prompt has crossfaded into the
  /// bubble, the accessibility hit test at the stop square has resolved to
  /// the bubble's container, so `click()` reports the button as not hittable
  /// although its frame is right and a mouse click reaches it; a coordinate
  /// click is the mouse path. The fallback is loud: an activity names it and
  /// the bubble's accessibility tree is attached, so the log shows whether
  /// the morph still leaves the square unreachable. Never used on the path
  /// where the bubble appears without a prompt; that path asserts
  /// `isHittable`.
  private func clickCenter(of element: XCUIElement, in app: XCUIApplication) {
    if element.isHittable {
      element.click()
      return
    }
    XCTContext.runActivity(
      named: "stop square not AX-hittable after the morph; clicking by coordinate"
    ) { activity in
      let tree = app.debugDescription.split(separator: "\n")
        .filter { $0.contains("bubble") || $0.contains("recording-bubble") }
        .joined(separator: "\n")
      let attachment = XCTAttachment(string: tree.isEmpty ? app.debugDescription : tree)
      attachment.name = "recording-bubble-accessibility-tree"
      attachment.lifetime = .keepAlways
      activity.add(attachment)
      element.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.5)).click()
    }
  }

  private func attachScreenshot(named name: String) {
    let screenshot = XCTAttachment(screenshot: XCUIScreen.main.screenshot())
    screenshot.name = name
    screenshot.lifetime = .keepAlways
    add(screenshot)
  }

  /// Distinct `meeting-<uuid>` identifiers among the elements that carry the
  /// selected trait (SwiftUI may stamp the identifier on the row and on its
  /// texts, so elements are not counted).
  private func selectedMeetingRowCount(in app: XCUIApplication) -> Int {
    let selected = NSPredicate(
      format: "identifier BEGINSWITH 'meeting-' AND selected == true")
    let elements = app.descendants(matching: .any).matching(selected)
    return Set(elements.allElementsBoundByIndex.map(\.identifier)).count
  }

  /// `-steno-start-recording` starts a call recording from the window at
  /// launch, so the live row is selected: the detail header shows the Stop
  /// control (`header-stop`) and every content tab body is the states
  /// table's "Recording" row (read through the title's id, since the
  /// `EmptyState` container id is not queryable on macOS). Stop ends the
  /// recording: the header control goes, the sidebar returns to Record
  /// call, and the row stays.
  func testHeaderStopEndsTheRecording() throws {
    let app = launch(["-steno-ui-testing", "-steno-start-recording"])

    let window = app.windows.firstMatch
    XCTAssertTrue(window.waitForExistence(timeout: 10), "no window appeared")
    let stop = app.buttons["header-stop"].firstMatch
    XCTAssertTrue(stop.waitForExistence(timeout: 20), "the header Stop control did not appear")
    XCTAssertTrue(app.buttons["sidebar-stop"].firstMatch.exists, "the sidebar shows Stop too")
    // "Stop" plus the elapsed time, ticking once a second (the synthetic
    // backend is faster than real time, the clock is not).
    let firstLabel = stop.label
    XCTAssertTrue(firstLabel.contains("Stop"), "the header Stop reads \(firstLabel)")
    XCTAssertTrue(
      waitUntil(timeout: 5) { stop.label != firstLabel },
      "the header elapsed time did not tick from \(firstLabel)")
    XCTAssertTrue(
      app.descendants(matching: .any)["header-levels"].firstMatch.waitForExistence(timeout: 5),
      "the header shows no level bars while recording")
    XCTAssertTrue(
      waitUntil(timeout: 10) { meetingRowCount(in: app) == 2 },
      "expected the fixture meeting plus the live row, got \(meetingRowCount(in: app))")

    let title = app.staticTexts["empty-summary-title"].firstMatch
    XCTAssertTrue(title.waitForExistence(timeout: 10), "the Summary tab shows no state row")
    // A SwiftUI `Text` exposes its string as the element's value on macOS;
    // `label` is empty.
    XCTAssertEqual((title.value as? String) ?? title.label, "Recording")
    XCTAssertTrue(app.buttons["tab-summary"].firstMatch.isSelected)
    // The recording state as the review sees it: header Stop, level bars,
    // the "Recording" row on the Summary tab.
    attachScreenshot(named: "detail-recording.png")
    app.buttons["tab-transcript"].firstMatch.click()
    let transcriptTitle = app.staticTexts["empty-transcript-title"].firstMatch
    XCTAssertTrue(transcriptTitle.waitForExistence(timeout: 5), "the Transcript tab shows no row")
    XCTAssertEqual((transcriptTitle.value as? String) ?? transcriptTitle.label, "Recording")
    app.buttons["tab-scratchpad"].firstMatch.click()
    XCTAssertTrue(
      app.textViews.firstMatch.waitForExistence(timeout: 5),
      "the scratchpad stays editable during the call")
    XCTAssertTrue(
      app.staticTexts["scratchpad-hint"].firstMatch.exists,
      "the scratchpad keeps its hint under the editor during the call")

    stop.click()
    XCTAssertTrue(
      waitUntil(timeout: 10) { !app.buttons["header-stop"].firstMatch.exists },
      "the header Stop control did not go away")
    XCTAssertTrue(
      app.buttons["sidebar-record"].firstMatch.waitForExistence(timeout: 10),
      "the sidebar control did not return to Record call")
    XCTAssertEqual(meetingRowCount(in: app), 2, "the stopped recording keeps its row")
  }

  /// The rich seed's failed meeting (`uuid(104)`, failed after transcription
  /// with a two-line reason): its entry selects it, the header carries the
  /// reason, the Summary tab shows the states table's "Processing failed"
  /// row with its "Try again" (present; enabled only with an endpoint, which
  /// the preview has none of). The processing entry is not read: the fakes
  /// resume and finish it within a second of launch.
  func testFailedMeetingShowsItsRowAndTryAgain() throws {
    let app = launch(["-steno-ui-testing", "-steno-rich-seed"])
    XCTAssertTrue(app.windows.firstMatch.waitForExistence(timeout: 10), "no window appeared")
    let entry = app.buttons["meeting-00000000-0000-0000-0000-000000000068"].firstMatch
    XCTAssertTrue(entry.waitForExistence(timeout: 10), "the failed meeting is not listed")
    entry.click()

    let title = app.staticTexts["empty-summary-title"].firstMatch
    XCTAssertTrue(title.waitForExistence(timeout: 10), "the Summary tab shows no state row")
    XCTAssertEqual((title.value as? String) ?? title.label, "Processing failed")
    let tryAgain = app.buttons["summary-try-again"].firstMatch
    XCTAssertTrue(tryAgain.waitForExistence(timeout: 5), "the failed row offers no Try again")
    XCTAssertFalse(tryAgain.isEnabled, "no endpoint in the preview: Try again is disabled")
    attachScreenshot(named: "detail-failed.png")
  }

  /// `-steno-show-onboarding` builds the preview with every permission
  /// unknown and opens the onboarding window: the step caption, the title,
  /// the subtitle and the four page 1 rows are there (ids, not copy), and
  /// Later moves to page 2 with its two rows and the Back and Finish pair.
  func testOnboardingWindowShowsBothPages() throws {
    let app = launch(["-steno-ui-testing", "-steno-show-onboarding"])

    let intro = app.staticTexts["onboarding-intro"].firstMatch
    XCTAssertTrue(intro.waitForExistence(timeout: 20), "the onboarding window did not open")
    XCTAssertFalse(((intro.value as? String) ?? "").isEmpty, "the subtitle is empty")
    let window = app.windows.containing(.staticText, identifier: "onboarding-intro").firstMatch
    XCTAssertLessThanOrEqual(
      intro.frame.maxX, window.frame.maxX, "the subtitle runs past the window instead of wrapping")
    XCTAssertTrue(app.staticTexts["onboarding-step"].firstMatch.exists, "no step caption")
    XCTAssertTrue(app.staticTexts["onboarding-title"].firstMatch.exists, "no title")
    for kind in ["microphone", "systemAudio", "calendar", "localNetwork"] {
      XCTAssertTrue(
        app.staticTexts["onboarding-step-\(kind)"].firstMatch.exists, "row \(kind) missing")
    }
    attachScreenshot(named: "onboarding-page-1.png")

    let later = app.buttons["onboarding-later"].firstMatch
    XCTAssertTrue(later.waitForExistence(timeout: 5), "Later is missing on page 1")
    later.click()
    XCTAssertTrue(
      app.buttons["onboarding-finish"].firstMatch.waitForExistence(timeout: 10),
      "Later did not reach page 2")
    XCTAssertTrue(
      waitUntil(timeout: 5) { !app.staticTexts["onboarding-step-microphone"].firstMatch.exists },
      "page 1 rows are still showing under page 2")
    XCTAssertTrue(app.buttons["onboarding-back"].firstMatch.exists)
    XCTAssertTrue(app.staticTexts["onboarding-setup-summaries"].firstMatch.exists)
    XCTAssertTrue(app.staticTexts["onboarding-setup-vault"].firstMatch.exists)
    attachScreenshot(named: "onboarding-page-2.png")
  }

  /// The entries, each a button carrying a `meeting-<uuid>` identifier and
  /// nothing else in the window does: the `meeting-list` container and the
  /// entry's texts are not buttons, so one query counts rows.
  private func meetingRowCount(in app: XCUIApplication) -> Int {
    app.buttons.matching(
      NSPredicate(format: "identifier MATCHES %@", "meeting-[0-9A-F-]{36}")
    ).count
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
