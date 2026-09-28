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

  /// The rule is the hyphen of a semantic-version pre-release suffix and
  /// nothing else: build metadata, spaces and the hostless "0.0.0"
  /// fallback stay on the stable lane, and a tag-style "v" prefix does not
  /// hide the suffix.
  func testOnlyAHyphenMarksAPreRelease() {
    for stable in ["", "0.0.0", "0.9.0+42", "0.9.0 rc1", "0.9.0.rc.1"] {
      XCTAssertTrue(UpdateChannels.allowed(forVersion: stable).isEmpty, "\"\(stable)\"")
    }
    for candidate in ["v0.9.0-rc.1", "0.9.0-rc.1+42", "0.9.0-", "1.0.0-alpha"] {
      XCTAssertEqual(UpdateChannels.allowed(forVersion: candidate), ["beta"], candidate)
    }
    XCTAssertEqual(UpdateChannels.preRelease, "beta", "the channel merge-appcast.py writes")
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
