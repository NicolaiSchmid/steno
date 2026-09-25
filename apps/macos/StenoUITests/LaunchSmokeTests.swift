import XCTest

/// The one UI smoke test: the app launches in its UI-testing mode (preview
/// environment: in-memory database, fakes, no permission prompts) and the
/// main window appears. The app cannot join the SwiftPM end-to-end test
/// target, so this test and the manual checklist are its end-to-end proof.
final class LaunchSmokeTests: XCTestCase {
  func testMainWindowOpens() throws {
    let app = XCUIApplication()
    app.launchArguments = ["-steno-ui-testing"]
    app.launch()

    XCTAssertEqual(app.state, .runningForeground)
    XCTAssertTrue(app.windows.firstMatch.waitForExistence(timeout: 10), "no window appeared")
  }
}
