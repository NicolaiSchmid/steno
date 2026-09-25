import Foundation
import StenoCore
import Synchronization

@testable import StenoHandover

/// One `HandoverService` on loopback with the test identity, an in-memory
/// store, core's `FakeHandoverIntake`, a wall clock the tests advance and a
/// fresh temporary inbox. `advertise` is always false: nothing leaves
/// 127.0.0.1. `run` starts it, hands it to the body and stops it, awaited,
/// so no shutdown overlaps the next test.
struct TestService {
  let service: HandoverService
  let store: MeetingStore
  let intake: FakeHandoverIntake
  let directory: URL
  /// The wall clock as the service read it at start.
  let now: Date
  private let clock: WallClock

  /// The service before `start()`, for tests that watch it come up or drive
  /// the engine without a listener. A `customIntake` (such as a scripted one
  /// that fails first) replaces the fake behind the service; `intake` stays
  /// what `test.intake` reads.
  static func prepare(
    chunkSize: Int = 1024 * 1024,
    intake: FakeHandoverIntake = FakeHandoverIntake(),
    customIntake: (any HandoverIntake)? = nil,
    now: Date = Date(timeIntervalSince1970: 1_790_000_000),
    readTimeout: Duration = .seconds(30)
  ) throws -> TestService {
    let directory = try Fixtures.temporaryDirectory("handover")
    let store = try MeetingStore.inMemory()
    let clock = WallClock(now)
    let configuration = HandoverConfiguration(
      serviceName: "Test Mac", advertise: false, chunkSize: chunkSize,
      inboxDirectory: directory.appendingPathComponent("inbox", isDirectory: true),
      pairingWindow: .seconds(300), readTimeout: readTimeout)
    let service = HandoverService(
      configuration: configuration, store: store, intake: customIntake ?? intake,
      identity: try TestIdentity.load(), now: { clock.now })
    return TestService(
      service: service, store: store, intake: intake, directory: directory, now: now,
      clock: clock)
  }

  static func start(
    chunkSize: Int = 1024 * 1024,
    intake: FakeHandoverIntake = FakeHandoverIntake(),
    customIntake: (any HandoverIntake)? = nil,
    now: Date = Date(timeIntervalSince1970: 1_790_000_000),
    readTimeout: Duration = .seconds(30)
  ) async throws -> TestService {
    let test = try prepare(
      chunkSize: chunkSize, intake: intake, customIntake: customIntake, now: now,
      readTimeout: readTimeout)
    try await test.service.start()
    return test
  }

  /// Runs `body` against a service that is stopped, awaited, afterwards,
  /// whether the body returned or threw. `start: false` leaves starting to
  /// the body.
  static func run(
    chunkSize: Int = 1024 * 1024,
    intake: FakeHandoverIntake = FakeHandoverIntake(),
    customIntake: (any HandoverIntake)? = nil,
    now: Date = Date(timeIntervalSince1970: 1_790_000_000),
    readTimeout: Duration = .seconds(30),
    start: Bool = true,
    _ body: (TestService) async throws -> Void
  ) async throws {
    let test = try prepare(
      chunkSize: chunkSize, intake: intake, customIntake: customIntake, now: now,
      readTimeout: readTimeout)
    if start { try await test.service.start() }
    do {
      try await body(test)
    } catch {
      await test.stop()
      throw error
    }
    await test.stop()
  }

  func stop() async {
    await service.stop()
    try? FileManager.default.removeItem(at: directory)
  }

  /// Moves the wall clock the service reads.
  func advance(by duration: Duration) {
    clock.advance(by: duration)
  }

  func client(fingerprint: Data? = nil) throws -> LoopbackClient {
    try LoopbackClient.forService(service, fingerprint: fingerprint)
  }

  func rawClient() throws -> RawClient {
    guard case .listening(let port) = service.state else {
      throw LoopbackClient.ClientError.notListening
    }
    return RawClient(port: port, fingerprint: service.identity.fingerprint)
  }

  var metrics: ServerMetrics.Snapshot { service.metrics.snapshot }
}

/// The one time source the service reads (`HandoverService(now:)`); tests
/// move it to expire a pairing window or age a receipt.
final class WallClock: Sendable {
  private let time: Mutex<Date>

  init(_ start: Date) {
    self.time = Mutex(start)
  }

  var now: Date { time.withLock { $0 } }

  func advance(by duration: Duration) {
    time.withLock { $0 = $0.addingTimeInterval(duration / .seconds(1)) }
  }
}
