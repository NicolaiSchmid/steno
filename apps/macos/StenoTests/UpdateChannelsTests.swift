import XCTest

/// The pre-release lane follows the installed version string, and the
/// General section's update status is plain words.
final class UpdateChannelsTests: XCTestCase {
  func testReleaseCandidatesReadTheBetaLane() {
    XCTAssertEqual(UpdateChannels.allowed(forVersion: "0.9.0-rc.1"), ["beta"])
    XCTAssertEqual(UpdateChannels.allowed(forVersion: "1.0.0-beta.2"), ["beta"])
    XCTAssertEqual(UpdateChannels.allowed(forVersion: "0.0.0-dryrun.5"), ["beta"])
  }

  func testStableBuildsReadTheDefaultChannelOnly() {
    for version in ["0.9.0", "1.0.0", "0.0.0", "10.2.13"] {
      XCTAssertTrue(UpdateChannels.allowed(forVersion: version).isEmpty, version)
    }
  }

  func testUpdateStatusIsPlainWords() {
    let now = Date(timeIntervalSince1970: 1_790_250_000)
    XCTAssertEqual(
      GeneralSettingsViewModel.updateStatus(outcome: .notChecked, lastCheck: nil, now: now),
      "Not checked yet")
    XCTAssertEqual(
      GeneralSettingsViewModel.updateStatus(outcome: .upToDate, lastCheck: nil, now: now),
      "Up to date")
    let checked = GeneralSettingsViewModel.updateStatus(
      outcome: .upToDate, lastCheck: now.addingTimeInterval(-7_200), now: now)
    XCTAssertTrue(checked.hasPrefix("Up to date, checked "), checked)
    XCTAssertEqual(
      GeneralSettingsViewModel.updateStatus(outcome: .available("0.9.1"), lastCheck: now, now: now),
      "Update available: 0.9.1")
    let failed = GeneralSettingsViewModel.updateStatus(
      outcome: .failed("SUSparkleErrorDomain 2001"), lastCheck: now, now: now)
    XCTAssertEqual(failed, "Could not check for updates")
    XCTAssertFalse(failed.contains("Sparkle"), "the raw error stays in the details")
  }
}
