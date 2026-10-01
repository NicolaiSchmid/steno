import XCTest

/// The UI smoke tests: the app launches in its UI-testing mode (preview
/// environment: in-memory database seeded with StenoCore's sample meeting,
/// fakes, synthetic audio, no permission prompts), the main window appears
/// with its web view, the floating panel shows the detection prompt and the
/// recording bubble, the onboarding window shows both pages, and Settings
/// opens on a deep-linked section. Every launch runs in UTC so derived
/// titles do not move with the runner. The app cannot join the SwiftPM
/// end-to-end test target, so these tests and the manual checklist are its
/// end-to-end proof.
///
/// The main window's content is the web UI (plan
/// `2026-09-29-macos-webview-ui.md`, WP2). WebKit exposes HTML by role and
/// label, not by identifier, so the main window tests assert the two things
/// it does expose, the web view element and the list column's "Meetings"
/// heading by its text, and take the review screenshots; Playwright and
/// `WebShellTests` cover the page's states. Identifiers on the SwiftUI
/// surfaces are pinned, never copy.
@MainActor
final class LaunchSmokeTests: XCTestCase {
  /// The app under test, launched in UTC with `arguments`. Every argument
  /// is a `-steno-*` flag with any value in the same token
  /// (`-steno-window=960x600`): AppKit pairs dash-prefixed arguments blindly
  /// at launch and opens whatever is left over as a document, after which
  /// SwiftUI leaves the primary window closed, so a value in its own token
  /// is refused here rather than found as a missing window.
  private func launch(_ arguments: [String]) -> XCUIApplication {
    for argument in arguments where !argument.hasPrefix("-steno-") {
      XCTFail("\(argument) is not a flag; write the value into the flag's own argument")
    }
    let app = XCUIApplication()
    app.launchArguments = arguments
    app.launchEnvironment["TZ"] = "UTC"
    app.launch()
    return app
  }

  /// The main window: `main-window`, the identifier `UITestWindowMarker`
  /// gives its `NSWindow`, or the scene's title as a fallback. No window has
  /// a title bar to match on, and the onboarding and Settings windows come
  /// to the front over it, so the first window is not it.
  private func mainWindow(in app: XCUIApplication) -> XCUIElement {
    app.windows.matching(
      NSPredicate(format: "identifier == %@ OR title == %@", "main-window", "Steno")
    ).firstMatch
  }

  /// Waits for the main window. When none appears the app's state is
  /// attached beside XCTest's own hierarchy dump, since a window that never
  /// shows leaves no other trace in the result bundle.
  private func requireMainWindow(in app: XCUIApplication) {
    if mainWindow(in: app).waitForExistence(timeout: 10) { return }
    attachLaunchLog(named: "no-main-window", state: app)
    XCTFail("no main window appeared")
  }

  /// The app's UI-test launch log (`UITestDiagnostics`, in `/tmp`, which
  /// both processes see) beside the app state, for a window or page that
  /// never showed.
  private func attachLaunchLog(named name: String, state app: XCUIApplication) {
    XCTContext.runActivity(named: name) { activity in
      let logURL = URL(fileURLWithPath: "/tmp/steno-ui-test.log")
      let log = (try? String(contentsOf: logURL, encoding: .utf8)) ?? "(no launch log)"
      let note = XCTAttachment(
        string: "app state \(app.state.rawValue); windows \(app.windows.count)\n\(log)\n"
          + "hierarchy:\n\(app.debugDescription.prefix(30_000))")
      note.name = name
      note.lifetime = .keepAlways
      activity.add(note)
    }
  }

  /// The page, through the two things WebKit exposes to XCUITest: the web
  /// view element and the page's text by label, here the list column's
  /// "Meetings" heading. WebKit's processes start slowly on a hosted runner,
  /// hence the long waits; a further moment lets the paint settle before a
  /// screenshot.
  private func waitForPage(in app: XCUIApplication) {
    if !app.webViews.firstMatch.waitForExistence(timeout: 20) {
      attachLaunchLog(named: "no-web-view", state: app)
      XCTFail("no web view in the main window")
    }
    XCTAssertTrue(
      app.staticTexts["Meetings"].firstMatch.waitForExistence(timeout: 20),
      "the page did not render its Meetings heading")
    RunLoop.current.run(until: Date().addingTimeInterval(1))
  }

  /// Launches with `arguments` and waits for the main window and its page.
  @discardableResult
  private func launchMainWindow(_ arguments: [String]) -> XCUIApplication {
    let app = launch(arguments)
    XCTAssertEqual(app.state, .runningForeground)
    requireMainWindow(in: app)
    waitForPage(in: app)
    return app
  }

  /// The main window launches in the UI-testing environment, hosts the web
  /// view and the page renders its list heading; everything else inside it
  /// is the web tests' business.
  func testMainWindowOpens() throws {
    launchMainWindow(["-steno-ui-testing"])
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

  /// A recording started from the Record menu (⌘⇧R, the same
  /// `RecordingController` the page's Record control calls) shows the
  /// bubble; its elapsed time advances (the one check of the product's 1 Hz
  /// clock). On this path the stop square must be reachable through
  /// accessibility, which pins the asymmetry `clickCenter` works around.
  func testBubbleFollowsARecordingStartedFromTheMenu() throws {
    let app = launchMainWindow(["-steno-ui-testing"])
    XCTAssertFalse(app.buttons["bubble-stop"].exists, "no bubble before the recording")
    app.typeKey("r", modifierFlags: [.command, .shift])

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

    XCTAssertTrue(
      stop.isHittable, "the stop square must stay AX-hittable when the bubble appears fresh")
    stop.click()
    let bubbleGone = XCTNSPredicateExpectation(
      predicate: NSPredicate(format: "exists == false"), object: stop)
    XCTAssertEqual(XCTWaiter().wait(for: [bubbleGone], timeout: 10), .completed)
  }

  /// The case the bubble exists for: with the main window closed, clicking
  /// the bubble's body reopens it (`openWindow` captured by `MenuBarLabel`).
  /// The window's presence is read through its web view, since the panel
  /// may be listed among `app.windows`.
  func testBubbleOpenReopensAClosedMainWindow() throws {
    let app = launchMainWindow(["-steno-ui-testing"])
    app.typeKey("r", modifierFlags: [.command, .shift])
    let stop = app.buttons["bubble-stop"].firstMatch
    XCTAssertTrue(stop.waitForExistence(timeout: 10), "the bubble did not follow the recording")

    let window = mainWindow(in: app)
    let close = window.buttons[XCUIIdentifierCloseWindow].firstMatch
    if close.exists {
      close.click()
    } else {
      app.typeKey("w", modifierFlags: .command)
    }
    XCTAssertTrue(
      waitUntil(timeout: 10) { !mainWindow(in: app).exists }, "the main window did not close")

    app.buttons["bubble-open"].firstMatch.click()
    XCTAssertTrue(
      mainWindow(in: app).waitForExistence(timeout: 10),
      "the bubble did not bring the main window back")

    stop.click()
    let bubbleGone = XCTNSPredicateExpectation(
      predicate: NSPredicate(format: "exists == false"), object: stop)
    XCTAssertEqual(XCTWaiter().wait(for: [bubbleGone], timeout: 10), .completed)
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

  /// The review evidence in light (`testScreenshotMatrixLight`).
  func testScreenshotMatrixLight() throws {
    captureScreenshotMatrix(appearance: "light")
  }

  /// The same set in dark, since the hosted runner is light and one
  /// screenshot per window cannot show that both appearances ship.
  func testScreenshotMatrixDark() throws {
    captureScreenshotMatrix(appearance: "dark")
  }

  /// One launch per window state under `-steno-appearance` and
  /// `-steno-window=960x600`, each attached with `.keepAlways` as
  /// `<window>-<appearance>-<state>.png`, so `xcrun xcresulttool export
  /// attachments` on the CI bundle yields the review set: the main window
  /// empty, with the rich seed (the page selects the newest meeting), and
  /// recording; onboarding page 1 and page 2; Settings on the Recording
  /// section, opened with ⌘, and deep-linked by `-steno-settings-section`.
  /// The main window states wait for the web view and the page's list
  /// heading; the rest of what the page renders is the web tests' evidence.
  /// The Settings state checks that the page's Recording heading lands
  /// inside the Settings window. 960 x 600 fits the runner's 1024 x 768
  /// display, as does the 760 x 520 Settings window over it.
  private func captureScreenshotMatrix(appearance: String) {
    /// Launches under the matrix flags plus `arguments`.
    func launched(_ arguments: [String]) -> XCUIApplication {
      launch(
        ["-steno-ui-testing", "-steno-appearance=\(appearance)", "-steno-window=960x600"]
          + arguments)
    }
    /// Runs `ready` on `app`, waits for the main window to be 960 wide, then
    /// attaches the screen as `<window>-<appearance>-<state>.png`, inside an
    /// activity named for the state so a failure names it. The width wait
    /// covers `UITestWindowMarker`'s second pass, which undoes the frame
    /// SwiftUI restores from the previous launch a second after the window
    /// shows, and proves the size fits the display.
    func capture(
      _ window: String, _ state: String, in app: XCUIApplication,
      ready: (XCUIApplication) -> Void
    ) {
      XCTContext.runActivity(named: "\(window) \(state) in \(appearance)") { _ in
        ready(app)
        let main = mainWindow(in: app)
        XCTAssertTrue(
          waitUntil(timeout: 5) { abs(main.frame.width - 960) < 1 },
          "the main window is \(main.frame.width) wide, not 960")
        attachScreenshot(named: "\(window)-\(appearance)-\(state).png")
      }
    }
    /// The main window and its page.
    func mainReady(_ app: XCUIApplication) {
      requireMainWindow(in: app)
      waitForPage(in: app)
    }

    capture("main", "empty", in: launched(["-steno-empty"]), ready: mainReady)
    capture("main", "selected", in: launched(["-steno-rich-seed"]), ready: mainReady)
    capture("main", "recording", in: launched(["-steno-start-recording"])) { app in
      mainReady(app)
      XCTAssertTrue(
        app.buttons["bubble-stop"].firstMatch.waitForExistence(timeout: 20),
        "the recording started from the window shows no bubble")
    }

    let onboarding = launched(["-steno-show-onboarding"])
    capture("onboarding", "page-1", in: onboarding) { app in
      XCTAssertTrue(
        app.staticTexts["onboarding-intro"].firstMatch.waitForExistence(timeout: 20),
        "the onboarding window did not open")
      XCTAssertTrue(
        app.staticTexts["onboarding-step-microphone"].firstMatch.waitForExistence(timeout: 5),
        "page 1 rows are missing")
    }
    capture("onboarding", "page-2", in: onboarding) { app in
      let later = app.buttons["onboarding-later"].firstMatch
      XCTAssertTrue(later.waitForExistence(timeout: 5), "Later is missing")
      later.click()
      XCTAssertTrue(
        app.buttons["onboarding-finish"].firstMatch.waitForExistence(timeout: 10),
        "Later did not reach page 2")
      XCTAssertTrue(
        app.staticTexts["onboarding-setup-summaries"].firstMatch.waitForExistence(timeout: 5),
        "page 2 rows are missing")
    }

    // The section comes from the scenario's deep link, which the Settings
    // page reads from the `app` snapshot when its window opens; ⌘, opens it
    // as `AppCommands` does. The window is `settings-window`
    // (`UITestWindowMarker`); the page inside is web content, so its
    // Recording heading is found by text, the one thing WebKit exposes.
    capture("settings", "recording", in: launched(["-steno-settings-section=recording"])) {
      app in
      mainReady(app)
      app.typeKey(",", modifierFlags: .command)
      let window = app.windows["settings-window"].firstMatch
      XCTAssertTrue(window.waitForExistence(timeout: 10), "the Settings window did not open")
      XCTAssertTrue(
        window.webViews.firstMatch.waitForExistence(timeout: 20),
        "no web view in the Settings window")
      let header = window.staticTexts["Recording"].firstMatch
      XCTAssertTrue(header.waitForExistence(timeout: 20), "Settings did not open on Recording")

      // The page paints inside its window: the heading is checked to lie
      // inside the 760 by 520 frame, as the SwiftUI split view once failed to.
      XCTAssertTrue(
        waitUntil(timeout: 5) { abs(window.frame.width - 760) < 1 },
        "the Settings window is \(window.frame.width) wide, not 760")
      XCTAssertTrue(
        window.frame.contains(header.frame),
        "the Recording heading \(header.frame) lies outside the Settings window \(window.frame)")
      RunLoop.current.run(until: Date().addingTimeInterval(1))
    }
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
