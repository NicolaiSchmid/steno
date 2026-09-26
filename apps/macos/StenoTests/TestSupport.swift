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
    processActivity: FakeProcessAudioActivity = FakeProcessAudioActivity(),
    makeSpeechEngine: @escaping @Sendable () -> any SpeechEngine = { FakeSpeechEngine() },
    calendar: (any CalendarProviding)? = nil
  ) async throws -> AppEnvironment {
    try await AppEnvironment.preview(
      clock: clock, now: { now }, handover: handover, seed: seed,
      makeCaptureSession: makeCaptureSession, processActivity: processActivity,
      makeSpeechEngine: makeSpeechEngine, calendar: calendar)
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

  /// Yields to the main actor a few times so a task started just before has
  /// reached its first suspension point.
  @MainActor
  static func settle() async {
    for _ in 0..<20 { await Task.yield() }
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

/// `take()` is true exactly once, from any thread.
final class OnceFlag: @unchecked Sendable {
  private let lock = NSLock()
  private var taken = false

  func take() -> Bool {
    lock.lock()
    defer { lock.unlock() }
    if taken { return false }
    taken = true
    return true
  }
}

/// Holds callers until opened; opening is idempotent and releases everyone.
actor Gate {
  private var isOpen = false
  private var waiters: [CheckedContinuation<Void, Never>] = []

  func wait() async {
    if isOpen { return }
    await withCheckedContinuation { waiters.append($0) }
  }

  func open() {
    isOpen = true
    let waiting = waiters
    waiters = []
    for waiter in waiting { waiter.resume() }
  }
}

/// `FakeSpeechEngine` whose `transcribe` waits at `gate`: a pipeline built
/// over it stays busy until the test opens the gate.
struct GatedSpeechEngine: SpeechEngine {
  let gate: Gate
  private let inner = FakeSpeechEngine()

  init(gate: Gate) {
    self.gate = gate
  }

  var id: String { inner.id }
  var supportedLanguages: Set<Locale.Language> { inner.supportedLanguages }

  func prepare() async throws {
    try await inner.prepare()
  }

  func transcribe(_ audio: AudioBuffer16k, hint: Locale.Language?) async throws -> [RawSegment] {
    await gate.wait()
    return try await inner.transcribe(audio, hint: hint)
  }
}

/// A calendar whose lookup waits at `gate`, so a recording sits in
/// `.starting` until the test lets it through.
@MainActor
final class GatedCalendar: CalendarProviding {
  let gate: Gate

  init(gate: Gate) {
    self.gate = gate
  }

  func events(on day: Date) async throws -> [CalendarEvent] {
    await gate.wait()
    return []
  }
}
