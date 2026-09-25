import Foundation
import Security
import StenoAudio
import StenoCore
import StenoHandover
import XCTest

/// Shared helpers: a preview environment on a `ManualClock` and a fixed
/// `now`, the committed handover test identity (the app cannot import the
/// handover test target, so the p12 import is repeated here), and a poll
/// that never sleeps on wall time longer than needed.
enum TestSupport {
  static let now = Date(timeIntervalSince1970: 1_790_250_000)

  @MainActor
  static func environment(
    clock: ManualClock = ManualClock(), seed: Bool = true, handover: HandoverService? = nil,
    makeCaptureSession: AppEnvironment.MakeCaptureSession? = nil,
    processActivity: FakeProcessAudioActivity = FakeProcessAudioActivity()
  ) async throws -> AppEnvironment {
    try await AppEnvironment.preview(
      clock: clock, now: { now }, handover: handover, seed: seed,
      makeCaptureSession: makeCaptureSession, processActivity: processActivity)
  }

  /// A capture session over the synthetic backend that reports device loss
  /// after `loseDeviceAfter` seconds of audio (an unplugged microphone).
  static func deviceLosingCaptureSession(after loseDeviceAfter: TimeInterval)
    -> AppEnvironment.MakeCaptureSession
  {
    { configuration in
      try CaptureSession(
        configuration: configuration,
        backend: SyntheticCaptureBackend(
          lanes: configuration.lanes, tone: [.mic: 440, .system: 660, .mixed: 440],
          seconds: 30, loseDeviceAfter: loseDeviceAfter))
    }
  }

  /// A fresh directory under the temporary folder, removed by the caller.
  static func temporaryDirectory(_ label: String) throws -> URL {
    let url = FileManager.default.temporaryDirectory
      .appendingPathComponent("\(label)-\(UUID().uuidString)", isDirectory: true)
    try FileManager.default.createDirectory(at: url, withIntermediateDirectories: true)
    return url
  }

  /// `Tests/Fixtures/` from this file's location.
  static var fixtures: URL {
    URL(fileURLWithPath: #filePath)
      .deletingLastPathComponent()  // StenoTests
      .deletingLastPathComponent()  // macos
      .deletingLastPathComponent()  // apps
      .deletingLastPathComponent()  // repo root
      .appendingPathComponent("Tests/Fixtures", isDirectory: true)
  }

  static var repositoryRoot: URL {
    fixtures.deletingLastPathComponent().deletingLastPathComponent()
  }

  /// `apps/macos/` from this file's location.
  static var appRoot: URL {
    URL(fileURLWithPath: #filePath)
      .deletingLastPathComponent()  // StenoTests
      .deletingLastPathComponent()  // macos
  }

  /// The committed test identity, imported to memory only (no keychain).
  static func testIdentity() throws -> HandoverIdentity {
    let data = try Data(contentsOf: fixtures.appendingPathComponent("handover/test-identity.p12"))
    var items: CFArray?
    let options: [CFString: Any] = [
      kSecImportExportPassphrase: "steno-test",
      kSecImportToMemoryOnly: true,
    ]
    let status = SecPKCS12Import(data as CFData, options as CFDictionary, &items)
    guard status == errSecSuccess else {
      throw IdentityError.security("SecPKCS12Import", status)
    }
    guard let first = (items as? [[CFString: Any]])?.first,
      let identity = first[kSecImportItemIdentity]
    else {
      throw IdentityError.malformed("the test p12 holds no identity")
    }
    return try HandoverIdentity(
      secIdentity: unsafeBitCast(identity as CFTypeRef, to: SecIdentity.self))
  }

  /// Polls `condition` every 10 ms up to `timeout` (default 10 s). Used only
  /// where a store observation or a pipeline task must be given time to
  /// deliver; every timer under test runs on `ManualClock`.
  @MainActor
  static func waitUntil(
    _ description: String, timeout: TimeInterval = 10, file: StaticString = #filePath,
    line: UInt = #line, _ condition: @MainActor () async -> Bool
  ) async {
    let deadline = Date().addingTimeInterval(timeout)
    while Date() < deadline {
      if await condition() { return }
      try? await Task.sleep(for: .milliseconds(10))
    }
    XCTFail("timed out waiting for \(description)", file: file, line: line)
  }
}
