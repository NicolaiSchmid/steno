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

  static let purposeKeys = [
    "NSAudioCaptureUsageDescription",
    "NSMicrophoneUsageDescription",
    "NSCalendarsFullAccessUsageDescription",
    "NSLocalNetworkUsageDescription",
  ]

  func testPurposeStringsArePresentAsLiterals() throws {
    let info = try Self.appInfo()
    for key in Self.purposeKeys {
      let value = info[key] as? String
      XCTAssertNotNil(value, "\(key) missing")
      XCTAssertFalse(value?.isEmpty ?? true, "\(key) empty")
    }
    for key in ["NSAudioCaptureUsageDescription", "NSMicrophoneUsageDescription"] {
      XCTAssertTrue(
        (info[key] as? String)?.contains("never leaves your Mac") ?? false,
        "\(key) states the privacy promise")
    }
  }

  /// The purpose strings are the literals in `project.yml`, verbatim: no
  /// `INFOPLIST_KEY_*` build setting (Xcode ignores it for the audio capture
  /// key) and no localisation indirection sits between the spec and the
  /// bundle.
  func testPurposeStringsMatchTheProjectSpecLiterals() throws {
    let info = try Self.appInfo()
    let spec = try String(
      contentsOf: TestSupport.appRoot.appendingPathComponent("project.yml"), encoding: .utf8)
    let pattern = try NSRegularExpression(
      pattern: #"^\s+(NS[A-Za-z]+UsageDescription): "(.+)"$"#, options: .anchorsMatchLines)
    var literals: [String: String] = [:]
    for match in pattern.matches(in: spec, range: NSRange(spec.startIndex..., in: spec)) {
      guard let keyRange = Range(match.range(at: 1), in: spec),
        let valueRange = Range(match.range(at: 2), in: spec)
      else { continue }
      literals[String(spec[keyRange])] = String(spec[valueRange])
    }
    XCTAssertEqual(
      Set(literals.keys), Set(Self.purposeKeys), "project.yml lists every purpose string")
    for (key, literal) in literals {
      XCTAssertEqual(info[key] as? String, literal, key)
    }
    XCTAssertFalse(
      spec.contains("INFOPLIST_KEY_"), "purpose strings never go through build settings")
  }

  func testApplicationMetadata() throws {
    let info = try Self.appInfo()
    XCTAssertEqual(info["CFBundleName"] as? String, "Steno")
    XCTAssertEqual(info["NSPrincipalClass"] as? String, "NSApplication")
    XCTAssertEqual(
      info["LSApplicationCategoryType"] as? String, "public.app-category.productivity")
    XCTAssertEqual(
      info["NSSupportsSuddenTermination"] as? Bool, false,
      "a recording must never be killed without stop()")
    XCTAssertEqual(info["NSSupportsAutomaticTermination"] as? Bool, false)
    XCTAssertNil(info["LSUIElement"], "v1 is a regular app whose menu bar item outlives the window")
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
    XCTAssertEqual(info["SUScheduledCheckInterval"] as? Int, 86_400, "one check a day")
    XCTAssertNil(info["SUEnableInstallerLauncherService"], "no XPC services outside the sandbox")
    XCTAssertNil(info["SUEnableDownloaderService"])
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
