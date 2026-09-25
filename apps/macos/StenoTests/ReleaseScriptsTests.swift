import XCTest

/// The release scripts' guards, run under `/bin/bash` (3.2 on macOS, the
/// shell GitHub Actions uses for `run:` steps on the macOS runners), plus
/// the reviewer traps the plan names: the secrets check stays the first
/// step of `release.yml` and the `codesign` grep stays in
/// `build-release.sh`.
final class ReleaseScriptsTests: XCTestCase {
  private static var scripts: URL {
    TestSupport.appRoot.appendingPathComponent("scripts", isDirectory: true)
  }

  private static let everySecret = [
    "P12": "base64", "P12_PASSWORD": "pw", "ASC_KEY_ID": "KEY", "ASC_ISSUER_ID": "ISSUER",
    "ASC_PRIVATE_KEY": "pem", "SPARKLE_PRIVATE_KEY": "ed25519",
  ]

  private func run(_ script: String, _ environment: [String: String]) throws -> (
    status: Int32, output: String
  ) {
    let process = Process()
    process.executableURL = URL(fileURLWithPath: "/bin/bash")
    process.arguments = [Self.scripts.appendingPathComponent(script).path]
    process.environment = ["PATH": "/usr/bin:/bin"].merging(environment) { $1 }
    let pipe = Pipe()
    process.standardOutput = pipe
    process.standardError = pipe
    try process.run()
    let data = pipe.fileHandleForReading.readDataToEndOfFile()
    process.waitUntilExit()
    return (process.terminationStatus, String(decoding: data, as: UTF8.self))
  }

  private func missingNames(in output: String) -> [String] {
    guard let range = output.range(of: "::error::missing repository secrets: ") else { return [] }
    let rest = output[range.upperBound...]
    guard let end = rest.firstIndex(of: ".") else { return [] }
    return rest[..<end].split(separator: " ").map(String.init)
  }

  // MARK: check-release-secrets.sh

  func testReleaseWithEverySecretPasses() throws {
    let result = try run(
      "check-release-secrets.sh", Self.everySecret.merging(["DRY_RUN": "false"]) { $1 })
    XCTAssertEqual(result.status, 0, result.output)
    XCTAssertTrue(
      result.output.contains("all release secrets present (dry run: false)"), result.output)
  }

  func testReleaseNamesEveryMissingSecretInOrder() throws {
    let result = try run("check-release-secrets.sh", ["DRY_RUN": "false"])
    XCTAssertEqual(result.status, 1)
    XCTAssertEqual(
      missingNames(in: result.output),
      [
        "MACOS_CERTIFICATE_P12_BASE64", "MACOS_CERTIFICATE_PASSWORD", "ASC_KEY_ID", "ASC_ISSUER_ID",
        "ASC_PRIVATE_KEY", "SPARKLE_PRIVATE_KEY",
      ], result.output)
    XCTAssertTrue(result.output.contains("apps/macos/README.md"), "points at the setup doc")
  }

  func testReleaseNamesOnlyTheMissingSecret() throws {
    for (variable, name) in [
      ("SPARKLE_PRIVATE_KEY", "SPARKLE_PRIVATE_KEY"), ("ASC_PRIVATE_KEY", "ASC_PRIVATE_KEY"),
      ("P12", "MACOS_CERTIFICATE_P12_BASE64"),
    ] {
      var environment = Self.everySecret
      environment[variable] = nil
      environment["DRY_RUN"] = "false"
      let result = try run("check-release-secrets.sh", environment)
      XCTAssertEqual(result.status, 1, variable)
      XCTAssertEqual(missingNames(in: result.output), [name], result.output)
    }
  }

  func testAnEmptyValueCountsAsMissing() throws {
    var environment = Self.everySecret
    environment["P12_PASSWORD"] = ""
    environment["DRY_RUN"] = "false"
    let result = try run("check-release-secrets.sh", environment)
    XCTAssertEqual(result.status, 1)
    XCTAssertEqual(missingNames(in: result.output), ["MACOS_CERTIFICATE_PASSWORD"])
  }

  func testDryRunNeedsOnlyTheCertificate() throws {
    let passing = try run(
      "check-release-secrets.sh", ["DRY_RUN": "true", "P12": "base64", "P12_PASSWORD": "pw"])
    XCTAssertEqual(passing.status, 0, passing.output)
    XCTAssertTrue(passing.output.contains("dry run: true"), passing.output)

    let failing = try run("check-release-secrets.sh", ["DRY_RUN": "true", "P12": "base64"])
    XCTAssertEqual(failing.status, 1)
    XCTAssertEqual(
      missingNames(in: failing.output), ["MACOS_CERTIFICATE_PASSWORD"],
      "notarisation and Sparkle secrets are not needed for a dry run")
  }

  func testAnUnsetDryRunMeansRelease() throws {
    var environment = Self.everySecret
    environment["SPARKLE_PRIVATE_KEY"] = nil
    let result = try run("check-release-secrets.sh", environment)
    XCTAssertEqual(result.status, 1, "without DRY_RUN the full set is required")
    XCTAssertEqual(missingNames(in: result.output), ["SPARKLE_PRIVATE_KEY"])
  }

  // MARK: release.yml and build-release.sh (reviewer traps)

  func testReleaseWorkflowRunsTheGuardBeforeAnyToolOrBuild() throws {
    let workflow = try String(
      contentsOf: TestSupport.repositoryRoot.appendingPathComponent(
        ".github/workflows/release.yml"),
      encoding: .utf8)
    let guardIndex = try XCTUnwrap(
      workflow.range(of: "run: apps/macos/scripts/check-release-secrets.sh")?.lowerBound,
      "release.yml runs the secrets guard")
    for later in [
      "Install xcodegen", "setup-xcode", "build-release.sh", "make-dmg.sh", "make-appcast.sh",
    ] {
      let index = try XCTUnwrap(workflow.range(of: later)?.lowerBound, later)
      XCTAssertLessThan(guardIndex, index, "\(later) runs after the secrets guard")
    }
    let stepHeader = try XCTUnwrap(workflow.range(of: "- name: Check secrets")?.lowerBound)
    let stepBody = workflow[stepHeader..<guardIndex]
    for secret in [
      "MACOS_CERTIFICATE_P12_BASE64", "MACOS_CERTIFICATE_PASSWORD", "ASC_KEY_ID", "ASC_ISSUER_ID",
      "ASC_PRIVATE_KEY", "SPARKLE_PRIVATE_KEY",
    ] {
      XCTAssertTrue(stepBody.contains("secrets.\(secret)"), "the guard step maps \(secret)")
    }
    XCTAssertTrue(stepBody.contains("DRY_RUN:"), "the guard step passes the dry-run switch")
  }

  func testBuildReleaseKeepsTheSignatureGuards() throws {
    let script = try String(
      contentsOf: Self.scripts.appendingPathComponent("build-release.sh"), encoding: .utf8)
    for guardLine in [
      "codesign --verify --deep --strict",
      "Authority=Developer ID Application",
      "runtime",
      "Timestamp=",
      "com.apple.security.device.audio-input",
      "com.apple.security.personal-information.calendars",
      "com.apple.security.app-sandbox",
      "expected exactly two entitlements",
      "TeamIdentifier=",
    ] {
      XCTAssertTrue(script.contains(guardLine), "build-release.sh lost its guard: \(guardLine)")
    }
  }

  func testEveryScriptParsesUnderBash() throws {
    let names = try FileManager.default.contentsOfDirectory(atPath: Self.scripts.path)
      .filter { $0.hasSuffix(".sh") }
      .sorted()
    XCTAssertTrue(names.contains("check-release-secrets.sh"))
    XCTAssertTrue(names.contains("build-release.sh"))
    for name in names {
      let process = Process()
      process.executableURL = URL(fileURLWithPath: "/bin/bash")
      process.arguments = ["-n", Self.scripts.appendingPathComponent(name).path]
      try process.run()
      process.waitUntilExit()
      XCTAssertEqual(process.terminationStatus, 0, "\(name) does not parse")
    }
  }
}
