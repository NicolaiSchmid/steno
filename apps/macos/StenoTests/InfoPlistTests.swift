import XCTest

/// The Info.plist keys the app cannot run without. The tests are hostless,
/// so they read the built `Steno.app` beside their own bundle instead of
/// `Bundle.main`. Weakening any assertion here is the reviewer trap named in
/// the plan: a missing purpose string surfaces as a silent TCC denial.
final class InfoPlistTests: XCTestCase {
  static func appInfo() throws -> [String: Any] {
    let products = Bundle(for: InfoPlistTests.self).bundleURL.deletingLastPathComponent()
    let app = products.appendingPathComponent("Steno.app", isDirectory: true)
    let bundle = try XCTUnwrap(Bundle(url: app), "Steno.app not found beside the test bundle")
    return try XCTUnwrap(bundle.infoDictionary, "Steno.app has no Info.plist")
  }

  func testBundleIdentifierIsImmutable() throws {
    let info = try Self.appInfo()
    XCTAssertEqual(info["CFBundleIdentifier"] as? String, "uno.schmid.steno.mac")
  }

  func testPurposeStringsArePresentAsLiterals() throws {
    let info = try Self.appInfo()
    for key in [
      "NSAudioCaptureUsageDescription",
      "NSMicrophoneUsageDescription",
      "NSCalendarsFullAccessUsageDescription",
      "NSLocalNetworkUsageDescription",
    ] {
      let value = info[key] as? String
      XCTAssertNotNil(value, "\(key) missing")
      XCTAssertFalse(value?.isEmpty ?? true, "\(key) empty")
    }
  }

  func testBonjourServiceMatchesTheHandoverListener() throws {
    let info = try Self.appInfo()
    XCTAssertEqual(info["NSBonjourServices"] as? [String], ["_steno._tcp"])
  }

  func testSparkleKeys() throws {
    let info = try Self.appInfo()
    XCTAssertEqual(
      info["SUFeedURL"] as? String,
      "https://github.com/NicolaiSchmid/steno/releases/latest/download/appcast.xml")
    let key = try XCTUnwrap(info["SUPublicEDKey"] as? String)
    XCTAssertNotEqual(key, "REPLACE_ME", "the EdDSA public key is still the placeholder")
    let decoded = try XCTUnwrap(Data(base64Encoded: key), "SUPublicEDKey is not base64")
    XCTAssertEqual(decoded.count, 32, "an Ed25519 public key is 32 bytes")
    XCTAssertEqual(info["SUEnableAutomaticChecks"] as? Bool, true)
  }

  func testVersionsComeFromBuildSettings() throws {
    let info = try Self.appInfo()
    XCTAssertNotNil(info["CFBundleShortVersionString"] as? String)
    let build = try XCTUnwrap(info["CFBundleVersion"] as? String)
    XCTAssertNotNil(Int(build), "CFBundleVersion must be an integer for Sparkle ordering")
  }

  func testMinimumSystemVersionIsMacOS15() throws {
    let info = try Self.appInfo()
    XCTAssertEqual(info["LSMinimumSystemVersion"] as? String, "15.0")
  }
}
