import XCTest

/// The one UI smoke test: the app launches in its UI-testing mode (preview
/// environment: in-memory database seeded with StenoCore's sample meeting,
/// fakes, no permission prompts), the main window appears, the fixture
/// meeting is listed and each of the four tabs is selectable. The app cannot
/// join the SwiftPM end-to-end test target, so this test and the manual
/// checklist are its end-to-end proof.
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

    for tab in ["summary", "transcript", "tasks", "scratchpad"] {
      let button = app.buttons["tab-\(tab)"].firstMatch
      XCTAssertTrue(button.waitForExistence(timeout: 10), "tab \(tab) missing")
      button.click()
      let content = app.otherElements["tab-content-\(tab)"].firstMatch
      XCTAssertTrue(content.waitForExistence(timeout: 5), "tab \(tab) content missing")
    }
  }
}
