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
    let dryRunEnv = "DRY_RUN: ${{ github.event_name == 'workflow_dispatch' && inputs.dry_run }}"
    let dryRun = try XCTUnwrap(
      workflow.range(of: dryRunEnv)?.lowerBound, "the dry-run switch is defined once")
    XCTAssertLessThan(dryRun, stepHeader, "as job-level env, ahead of the guard step")
    XCTAssertEqual(
      workflow.components(separatedBy: "inputs.dry_run").count - 1, 1,
      "every conditional step reads env.DRY_RUN instead of repeating the predicate")
    XCTAssertTrue(workflow.contains("if: env.DRY_RUN == 'true'"))
    XCTAssertTrue(workflow.contains("if: env.DRY_RUN != 'true'"))
  }

  /// The runner user's keychain search list is saved before the throwaway
  /// keychain is added and restored from that file, never reset to a guess.
  func testReleaseWorkflowRestoresTheKeychainSearchList() throws {
    let workflow = try String(
      contentsOf: TestSupport.repositoryRoot.appendingPathComponent(
        ".github/workflows/release.yml"),
      encoding: .utf8)
    let saved = try XCTUnwrap(
      workflow.range(of: "security list-keychains -d user | ")?.lowerBound,
      "the import step reads the current search list")
    let cleanup = try XCTUnwrap(workflow.range(of: "- name: Remove keychain and keys")?.lowerBound)
    XCTAssertLessThan(saved, cleanup)
    let cleanupBody = workflow[cleanup...]
    XCTAssertTrue(
      cleanupBody.contains("< \"$KEYCHAINS_BEFORE\""), "the cleanup reads the saved list back")
    XCTAssertTrue(cleanupBody.contains("security list-keychains -d user -s \"${before[@]}\""))
    XCTAssertFalse(
      workflow.contains("list-keychain -d user -s login.keychain-db"),
      "no hard-coded reset to the login keychain alone")
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
      // Machine-readable entitlements; `:-` is deprecated on Xcode 27 and
      // the bare `-` prints a format with no `<key>` lines.
      "codesign -d --entitlements - --xml",
      // Nested code items get the same three checks, Sparkle's helpers
      // included; no `-prune` at the framework boundary.
      "require_release_signature \"$nested\"",
      "-name 'Autoupdate'",
      "-name '*.xpc'",
    ] {
      XCTAssertTrue(script.contains(guardLine), "build-release.sh lost its guard: \(guardLine)")
    }
    XCTAssertFalse(script.contains("-prune"), "the nested walk must descend into frameworks")
    XCTAssertFalse(script.contains("--entitlements :-"), "deprecated codesign spelling")
  }

  // MARK: install-xcodegen.sh

  private func installXcodegen(runnerTemp: URL, path: String) throws -> (
    status: Int32, output: String
  ) {
    let process = Process()
    process.executableURL = URL(fileURLWithPath: "/bin/bash")
    process.arguments = [Self.scripts.appendingPathComponent("install-xcodegen.sh").path]
    process.environment = ["PATH": path, "RUNNER_TEMP": runnerTemp.path]
    let pipe = Pipe()
    process.standardOutput = pipe
    process.standardError = pipe
    try process.run()
    let data = pipe.fileHandleForReading.readDataToEndOfFile()
    process.waitUntilExit()
    return (process.terminationStatus, String(decoding: data, as: UTF8.self))
  }

  /// A fake `xcodegen` on PATH reporting `version`.
  private func fakeXcodegen(version: String) throws -> URL {
    let bin = try TestSupport.temporaryDirectory("steno-fake-bin")
    let tool = bin.appendingPathComponent("xcodegen")
    let script = "#!/bin/bash\necho \"Version: \(version)\"\n"
    try script.write(to: tool, atomically: true, encoding: .utf8)
    try FileManager.default.setAttributes([.posixPermissions: 0o755], ofItemAtPath: tool.path)
    return bin
  }

  func testInstallXcodegenRefusesAZipWithTheWrongChecksum() throws {
    let temp = try TestSupport.temporaryDirectory("steno-xcodegen")
    defer { try? FileManager.default.removeItem(at: temp) }
    let root = temp.appendingPathComponent("xcodegen-2.46.0", isDirectory: true)
    try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
    let zip = root.appendingPathComponent("xcodegen.zip")
    try Data("not the release".utf8).write(to: zip)

    let result = try installXcodegen(runnerTemp: temp, path: "/usr/bin:/bin")
    XCTAssertEqual(result.status, 1, result.output)
    XCTAssertTrue(result.output.contains("::error::xcodegen.zip checksum mismatch"), result.output)
    XCTAssertFalse(
      FileManager.default.fileExists(atPath: zip.path), "the bad zip is removed for a clean retry")
  }

  func testInstallXcodegenUsesThePinnedVersionOnPathAndNothingElse() throws {
    let temp = try TestSupport.temporaryDirectory("steno-xcodegen")
    defer { try? FileManager.default.removeItem(at: temp) }
    let pinned = try fakeXcodegen(version: "2.46.0")
    defer { try? FileManager.default.removeItem(at: pinned) }
    let accepted = try installXcodegen(runnerTemp: temp, path: "\(pinned.path):/usr/bin:/bin")
    XCTAssertEqual(accepted.status, 0, accepted.output)
    XCTAssertTrue(accepted.output.contains("already on PATH"), accepted.output)

    // An older xcodegen on PATH is not good enough: the script falls through
    // to the pinned download, which here hits a bad cached zip.
    let drifted = try fakeXcodegen(version: "2.40.0")
    defer { try? FileManager.default.removeItem(at: drifted) }
    let root = temp.appendingPathComponent("xcodegen-2.46.0", isDirectory: true)
    try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
    try Data("stale".utf8).write(to: root.appendingPathComponent("xcodegen.zip"))
    let rejected = try installXcodegen(runnerTemp: temp, path: "\(drifted.path):/usr/bin:/bin")
    XCTAssertEqual(rejected.status, 1, rejected.output)
    XCTAssertTrue(rejected.output.contains("is '2.40.0', want 2.46.0"), rejected.output)
    XCTAssertTrue(rejected.output.contains("checksum mismatch"), rejected.output)
  }

  // MARK: xcodebuild-quiet.sh

  /// The shared runner fails when the log lacks the success marker, even
  /// though the piped grep swallowed xcodebuild's exit status.
  func testXcodebuildQuietFailsWithoutTheSuccessMarker() throws {
    let temp = try TestSupport.temporaryDirectory("steno-quiet")
    defer { try? FileManager.default.removeItem(at: temp) }
    // A fake xcodebuild that prints a result line of its own choosing.
    let bin = temp.appendingPathComponent("bin", isDirectory: true)
    try FileManager.default.createDirectory(at: bin, withIntermediateDirectories: true)
    let tool = bin.appendingPathComponent("xcodebuild")
    try "#!/bin/bash\necho \"note: $*\"\necho \"** $XCODEBUILD_FAKE_RESULT **\"\n"
      .write(to: tool, atomically: true, encoding: .utf8)
    try FileManager.default.setAttributes([.posixPermissions: 0o755], ofItemAtPath: tool.path)

    func run(_ fakeResult: String) throws -> (status: Int32, output: String) {
      let process = Process()
      process.executableURL = URL(fileURLWithPath: "/bin/bash")
      process.arguments = [
        Self.scripts.appendingPathComponent("xcodebuild-quiet.sh").path,
        temp.appendingPathComponent("out.log").path, "BUILD SUCCEEDED", "--", "build",
        "-scheme", "Steno",
      ]
      process.environment = [
        "PATH": "\(bin.path):/usr/bin:/bin", "XCODEBUILD_FAKE_RESULT": fakeResult,
      ]
      let pipe = Pipe()
      process.standardOutput = pipe
      process.standardError = pipe
      try process.run()
      let data = pipe.fileHandleForReading.readDataToEndOfFile()
      process.waitUntilExit()
      return (process.terminationStatus, String(decoding: data, as: UTF8.self))
    }

    let passing = try run("BUILD SUCCEEDED")
    XCTAssertEqual(passing.status, 0, passing.output)
    XCTAssertTrue(passing.output.contains("** BUILD SUCCEEDED **"))
    XCTAssertFalse(passing.output.contains("note:"), "chatter stays in the log file")
    let log = try String(
      contentsOf: temp.appendingPathComponent("out.log"), encoding: .utf8)
    XCTAssertTrue(log.contains("note: build -scheme Steno"), "the full output is in the log")

    let failing = try run("BUILD FAILED")
    XCTAssertEqual(failing.status, 1)
    XCTAssertTrue(failing.output.contains("did not report '** BUILD SUCCEEDED **'"), failing.output)
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
