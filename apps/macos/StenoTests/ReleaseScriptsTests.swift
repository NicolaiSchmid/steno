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
      // Nested code items get the team and timestamp checks, Sparkle's
      // helpers included, and bundles and executables the runtime flag;
      // no `-prune` at the framework boundary.
      "require_release_signature \"$nested\"",
      "-name 'Autoupdate'",
      "-name '*.xpc'",
      "*.dylib) echo library",
      // The whole `-o` disjunction is parenthesised ahead of `-print0`;
      // otherwise find prints the last branch only and skips the bundles.
      "\\) -print0",
      // The helper's diagnostics go to stderr, never into the captured
      // team, and the substitution's status is checked explicitly.
      "require_release_signature \"$app\")\" || exit 1",
      "require_release_signature \"$nested\" \"$nested_kind\")\" || exit 1",
    ] {
      XCTAssertTrue(script.contains(guardLine), "build-release.sh lost its guard: \(guardLine)")
    }
    XCTAssertFalse(script.contains("-prune"), "the nested walk must descend into frameworks")
    XCTAssertFalse(script.contains("--entitlements :-"), "deprecated codesign spelling")
  }

  // MARK: build-release.sh --verify-only

  /// A `codesign` shim: `--verify` succeeds, `--entitlements` prints the
  /// two keys (a third with CODESIGN_FAKE_EXTRA_ENTITLEMENT), `-dv` prints
  /// canned details on stderr as codesign does. Items named in
  /// CODESIGN_FAKE_NO_RUNTIME are signed without the hardened runtime flag
  /// (`flags=0x0(none)`, what Xcode 27 produces for its embedded
  /// libswiftCompatibilitySpan.dylib); items named in CODESIGN_FAKE_FOREIGN
  /// carry another team.
  private static let codesignShim = #"""
    #!/bin/bash
    item="${!#}"
    name="$(basename "$item")"
    case " $* " in
      *" --verify "*) exit 0 ;;
      *" --entitlements "*)
        extra=""
        [ -n "${CODESIGN_FAKE_EXTRA_ENTITLEMENT:-}" ] && extra="<key>$CODESIGN_FAKE_EXTRA_ENTITLEMENT</key><true/>"
        printf '<plist><dict><key>com.apple.security.device.audio-input</key><true/><key>com.apple.security.personal-information.calendars</key><true/>%s</dict></plist>\n' "$extra"
        exit 0 ;;
    esac
    flags='0x10000(runtime)'
    team='TEAM0000AA'
    case " ${CODESIGN_FAKE_NO_RUNTIME:-} " in *" $name "*) flags='0x0(none)' ;; esac
    case " ${CODESIGN_FAKE_FOREIGN:-} " in *" $name "*) team='FOREIGN00X' ;; esac
    {
      echo "Executable=$item"
      echo "Identifier=fake.$name"
      echo "Format=Mach-O universal (x86_64 arm64)"
      echo "CodeDirectory v=20500 size=1 flags=$flags hashes=1+1 location=embedded"
      echo "Authority=Developer ID Application: Fake ($team)"
      echo "Authority=Developer ID Certification Authority"
      echo "Timestamp=27. Sep 2026 at 22:22:20"
      echo "TeamIdentifier=$team"
    } >&2

    """#

  /// The nested code items of an exported Steno.app, relative to the app:
  /// Sparkle's framework, its helpers and XPC services, and the Swift
  /// compatibility dylib Xcode embeds for deployment targets below the
  /// SDK's.
  private static let nestedItems = [
    "Contents/Frameworks/Sparkle.framework",
    "Contents/Frameworks/Sparkle.framework/Versions/B/Autoupdate",
    "Contents/Frameworks/Sparkle.framework/Versions/B/Updater.app",
    "Contents/Frameworks/Sparkle.framework/Versions/B/XPCServices/Installer.xpc",
    "Contents/Frameworks/Sparkle.framework/Versions/B/XPCServices/Downloader.xpc",
    "Contents/Frameworks/libswiftCompatibilitySpan.dylib",
  ]

  /// A fake Steno.app holding `nestedItems` (empty files for the Mach-O
  /// executables, directories for the bundles) next to a `bin/` with the
  /// codesign shim. The caller removes `root`.
  private func fakeSignedApp() throws -> (root: URL, app: URL, bin: URL) {
    let root = try TestSupport.temporaryDirectory("steno-verify")
    let app = root.appendingPathComponent("Steno.app", isDirectory: true)
    let versions = app.appendingPathComponent(
      "Contents/Frameworks/Sparkle.framework/Versions/B", isDirectory: true)
    let files = FileManager.default
    for bundle in [
      "Updater.app/Contents/MacOS", "XPCServices/Installer.xpc/Contents/MacOS",
      "XPCServices/Downloader.xpc/Contents/MacOS",
    ] {
      try files.createDirectory(
        at: versions.appendingPathComponent(bundle, isDirectory: true),
        withIntermediateDirectories: true)
    }
    try Data().write(to: versions.appendingPathComponent("Autoupdate"))
    try Data().write(
      to: app.appendingPathComponent("Contents/Frameworks/libswiftCompatibilitySpan.dylib"))
    let bin = root.appendingPathComponent("bin", isDirectory: true)
    try files.createDirectory(at: bin, withIntermediateDirectories: true)
    let tool = bin.appendingPathComponent("codesign")
    try Self.codesignShim.write(to: tool, atomically: true, encoding: .utf8)
    try files.setAttributes([.posixPermissions: 0o755], ofItemAtPath: tool.path)
    return (root, app, bin)
  }

  /// Bytes read from a pipe on another queue.
  private final class CapturedOutput: @unchecked Sendable {
    var data = Data()
  }

  /// Runs `build-release.sh --verify-only <app>` with `bin` first on PATH.
  /// stdout and stderr are kept apart: the `::error::` lines must reach
  /// stderr, since the script captures the helper's stdout as the team.
  private func verifyOnly(app: URL, bin: URL, environment: [String: String] = [:]) throws -> (
    status: Int32, stdout: String, stderr: String
  ) {
    let process = Process()
    process.executableURL = URL(fileURLWithPath: "/bin/bash")
    process.arguments = [
      Self.scripts.appendingPathComponent("build-release.sh").path, "--verify-only", app.path,
    ]
    process.environment = ["PATH": "\(bin.path):/usr/bin:/bin"].merging(environment) { $1 }
    let out = Pipe()
    let err = Pipe()
    process.standardOutput = out
    process.standardError = err
    try process.run()
    let captured = CapturedOutput()
    let group = DispatchGroup()
    group.enter()
    DispatchQueue.global().async {
      captured.data = err.fileHandleForReading.readDataToEndOfFile()
      group.leave()
    }
    let stdout = out.fileHandleForReading.readDataToEndOfFile()
    group.wait()
    process.waitUntilExit()
    return (
      process.terminationStatus, String(decoding: stdout, as: UTF8.self),
      String(decoding: captured.data, as: UTF8.self)
    )
  }

  /// Every nested item is visited (the find expression is parenthesised
  /// ahead of `-print0`), and a dylib signed without the hardened runtime
  /// flag passes: notarisation requires the flag on executables and
  /// bundles only.
  func testVerifyOnlyChecksEveryNestedItemAndAcceptsADylibWithoutRuntime() throws {
    let (root, app, bin) = try fakeSignedApp()
    defer { try? FileManager.default.removeItem(at: root) }
    let result = try verifyOnly(
      app: app, bin: bin,
      environment: ["CODESIGN_FAKE_NO_RUNTIME": "libswiftCompatibilitySpan.dylib"])
    XCTAssertEqual(result.status, 0, result.stdout + result.stderr)
    for item in Self.nestedItems {
      XCTAssertTrue(
        result.stdout.contains("ok: \(item) ("), "\(item) not checked:\n\(result.stdout)")
    }
    XCTAssertTrue(
      result.stdout.contains("ok: Contents/Frameworks/libswiftCompatibilitySpan.dylib (library)"))
    XCTAssertTrue(
      result.stdout.contains(
        "ok: Contents/Frameworks/Sparkle.framework/Versions/B/Autoupdate (executable)"))
    XCTAssertTrue(
      result.stdout.contains("==> ok: \(app.path) (team TEAM0000AA, 6 nested items)"), result.stdout
    )
    XCTAssertFalse(result.stderr.contains("::error::"), result.stderr)
  }

  /// A helper without the hardened runtime fails before the notarisation
  /// upload, and loudly: the `::error::` line and codesign's details block
  /// land on stderr rather than in the captured team variable.
  func testVerifyOnlyFailsLoudlyOnABundleWithoutRuntime() throws {
    let (root, app, bin) = try fakeSignedApp()
    defer { try? FileManager.default.removeItem(at: root) }
    let result = try verifyOnly(
      app: app, bin: bin,
      environment: ["CODESIGN_FAKE_NO_RUNTIME": "libswiftCompatibilitySpan.dylib Installer.xpc"])
    XCTAssertEqual(result.status, 1, result.stdout + result.stderr)
    XCTAssertTrue(
      result.stderr.contains(
        "::error::hardened runtime flag missing on \(app.path)/Contents/Frameworks/Sparkle.framework/Versions/B/XPCServices/Installer.xpc"
      ), result.stderr)
    XCTAssertTrue(result.stderr.contains("flags=0x0(none)"), "the details block follows")
    XCTAssertFalse(result.stdout.contains("::error::"), "diagnostics never go to stdout")
    XCTAssertFalse(result.stdout.contains("==> ok:"), result.stdout)
    XCTAssertFalse(
      result.stdout.contains(
        "ok: Contents/Frameworks/Sparkle.framework/Versions/B/XPCServices/Installer.xpc"))

    // A standalone executable gets the same treatment as a bundle.
    let autoupdate = try verifyOnly(
      app: app, bin: bin, environment: ["CODESIGN_FAKE_NO_RUNTIME": "Autoupdate"])
    XCTAssertEqual(autoupdate.status, 1)
    XCTAssertTrue(
      autoupdate.stderr.contains("::error::hardened runtime flag missing on"), autoupdate.stderr)
    XCTAssertTrue(autoupdate.stderr.contains("/Versions/B/Autoupdate"), autoupdate.stderr)
  }

  func testVerifyOnlyRejectsANestedItemFromAnotherTeam() throws {
    let (root, app, bin) = try fakeSignedApp()
    defer { try? FileManager.default.removeItem(at: root) }
    let result = try verifyOnly(
      app: app, bin: bin, environment: ["CODESIGN_FAKE_FOREIGN": "Downloader.xpc"])
    XCTAssertEqual(result.status, 1, result.stdout + result.stderr)
    XCTAssertTrue(
      result.stderr.contains(
        "::error::\(app.path)/Contents/Frameworks/Sparkle.framework/Versions/B/XPCServices/Downloader.xpc is signed by team 'FOREIGN00X', expected 'TEAM0000AA'"
      ), result.stderr)
    XCTAssertFalse(result.stdout.contains("==> ok:"), result.stdout)
  }

  /// The entitlement count guard on the app is untouched by the nested
  /// checks.
  func testVerifyOnlyStillCountsTheEntitlements() throws {
    let (root, app, bin) = try fakeSignedApp()
    defer { try? FileManager.default.removeItem(at: root) }
    let result = try verifyOnly(
      app: app, bin: bin,
      environment: ["CODESIGN_FAKE_EXTRA_ENTITLEMENT": "com.apple.security.network.client"])
    XCTAssertEqual(result.status, 1, result.stdout + result.stderr)
    XCTAssertTrue(
      result.stderr.contains("::error::expected exactly two entitlements, found 3"), result.stderr)
    XCTAssertFalse(result.stdout.contains("verify nested signatures"), "fails before the walk")
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
