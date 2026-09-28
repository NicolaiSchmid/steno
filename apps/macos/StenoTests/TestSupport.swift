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

  /// `seed` false leaves the store empty; `seedSet` picks the fixture set
  /// when it is seeded (`.sample` unless a test asks for `.rich`).
  @MainActor
  static func environment(
    clock: ManualClock = ManualClock(), seed: Bool = true, seedSet: PreviewSeed.Set = .sample,
    handover: HandoverService? = nil,
    makeCaptureSession: AppEnvironment.MakeCaptureSession? = nil,
    processActivity: FakeProcessAudioActivity = FakeProcessAudioActivity(),
    makeSpeechEngine: @escaping @Sendable () -> any SpeechEngine = { FakeSpeechEngine() },
    makeDiarizer: @escaping @Sendable () -> any Diarizer = { FakeDiarizer() },
    makeSummarizer: @escaping @Sendable () -> any MeetingSummarizer = { FakeSummarizer() },
    calendar: (any CalendarProviding)? = nil
  ) async throws -> AppEnvironment {
    try await AppEnvironment.preview(
      clock: clock, now: { now }, handover: handover, seed: seed ? seedSet : nil,
      makeCaptureSession: makeCaptureSession, processActivity: processActivity,
      makeSpeechEngine: makeSpeechEngine, makeDiarizer: makeDiarizer,
      makeSummarizer: makeSummarizer, calendar: calendar)
  }

  /// A capture session over a synthetic backend whose device changes after
  /// `changeDeviceAfter` seconds of audio, on `clock` (the test's
  /// `ManualClock`, so the restart backoff and the relay waits never sleep on
  /// wall time). `restartsThatFail` restarts throw before one succeeds; with
  /// none failing the session rebuilds at once and keeps recording. Each
  /// session gets its own backend, so a second recording changes its device
  /// the same way. The relay headroom is raised as in core's own tests: the
  /// synthetic backend delivers faster than real time.
  static func deviceChangingCaptureSession(
    after changeDeviceAfter: TimeInterval, restartsThatFail: Int = 0, clock: ManualClock
  ) -> AppEnvironment.MakeCaptureSession {
    { configuration in
      try CaptureSession(
        configuration: configuration,
        backend: SyntheticCaptureBackend(
          lanes: configuration.lanes, tone: [.mic: 440, .system: 660, .mixed: 440],
          seconds: 5, changeDeviceAfter: changeDeviceAfter, restartsThatFail: restartsThatFail),
        writerHeadroomFrames: 1_000, clock: clock)
    }
  }

  /// The device never comes back: every restart fails, so the session ends
  /// in `.deviceLost` once the test has advanced `clock` through the backoff
  /// ladder (`advanceThroughTheRestartLadder`).
  static func deviceLosingCaptureSession(after changeDeviceAfter: TimeInterval, clock: ManualClock)
    -> AppEnvironment.MakeCaptureSession
  {
    deviceChangingCaptureSession(
      after: changeDeviceAfter, restartsThatFail: CaptureSession.restartAttempts, clock: clock)
  }

  /// Advances `clock` through every backoff sleep of a rebuild, each once
  /// the session's sleeper is registered; `beforeEach` runs before every
  /// advance so a test can assert what is true while a restart is pending.
  @MainActor
  static func advanceThroughTheRestartLadder(
    _ clock: ManualClock, file: StaticString = #filePath, line: UInt = #line,
    beforeEach: @MainActor () -> Void = {}
  ) async {
    for step in CaptureSession.restartBackoff {
      let sleeping = await waitUntilSleeping(
        on: clock, count: 1, "the rebuild sleeps on the injected clock", file: file, line: line)
      guard sleeping else { return }
      beforeEach()
      clock.advance(by: step)
    }
  }

  /// Waits on wall time (10 ms polls, up to `timeout`) until `count` tasks
  /// sleep on `clock`. Named apart from `ManualClock.waitForSleepers`, which
  /// only yields: enough for a main-actor countdown but not for a session
  /// whose rebuild joins the backend and processing threads before it sleeps.
  @MainActor
  @discardableResult
  static func waitUntilSleeping(
    on clock: ManualClock, count: Int, _ description: String, timeout: TimeInterval = 10,
    file: StaticString = #filePath, line: UInt = #line
  ) async -> Bool {
    let deadline = Date().addingTimeInterval(timeout)
    while clock.pendingSleepers < count {
      guard Date() < deadline else {
        XCTFail("timed out waiting for \(description)", file: file, line: line)
        return false
      }
      await Task.yield()
      try? await Task.sleep(for: .milliseconds(10))
    }
    return true
  }

  /// Ticks a `Countdown` on `clock` `seconds` times: each one-second advance
  /// waits for the countdown's sleeper first, so no tick is lost, and yields
  /// afterwards so the ticker runs on the main actor between advances. Read
  /// the result through `waitUntil`: the resumed ticker is its own main-actor
  /// job and `settle` is a courtesy, not a guarantee.
  @MainActor
  static func tick(
    _ clock: ManualClock, seconds: Int, file: StaticString = #filePath, line: UInt = #line
  ) async {
    for _ in 0..<seconds {
      let sleeping = await waitUntilSleeping(
        on: clock, count: 1, "the countdown sleeps on the injected clock", file: file, line: line)
      guard sleeping else { return }
      clock.advance(by: .seconds(1))
      await settle()
    }
  }

  /// Polls `condition` like `waitUntil` and, whenever at least `sleepers`
  /// tasks wait on `clock`, advances it by 5 ms: a rebuild waits out a full
  /// relay in 5 ms steps on the clock, and a fast writer may never make it
  /// wait at all, so the waits are driven as they appear rather than
  /// counted. A test with a countdown armed passes `sleepers: 2` so the
  /// countdown's own one-second sleeper never triggers an advance.
  @MainActor
  static func waitDrivingTheClock(
    _ clock: ManualClock, sleepers: Int = 1, _ description: String, timeout: TimeInterval = 10,
    file: StaticString = #filePath, line: UInt = #line, _ condition: @MainActor () -> Bool
  ) async {
    let deadline = Date().addingTimeInterval(timeout)
    while !condition() {
      guard Date() < deadline else {
        return XCTFail("timed out waiting for \(description)", file: file, line: line)
      }
      if await clock.waitForSleepers(sleepers, attempts: 100) {
        clock.advance(by: .milliseconds(5))
      } else {
        try? await Task.sleep(for: .milliseconds(10))
      }
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

  /// The built `Steno.app` beside the test bundle. The hostless tests read
  /// its Info.plist and resources through this path instead of `Bundle.main`.
  static var builtApp: URL {
    Bundle(for: BundleToken.self).bundleURL.deletingLastPathComponent()
      .appendingPathComponent("Steno.app", isDirectory: true)
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

  /// The one meeting row a stopped recording left behind, for tests that
  /// read the stored end reason and state.
  static func stoppedMeeting(in environment: AppEnvironment) async throws -> Meeting {
    let meetings = try await environment.store.meetings()
    XCTAssertEqual(meetings.count, 1, "one recording, one row")
    let meeting = try XCTUnwrap(meetings.first)
    XCTAssertNotEqual(meeting.state, .recording, "stop left the recording state")
    return meeting
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

/// `Bundle(for:)` needs a class; `TestSupport` is an enum.
private final class BundleToken {}
