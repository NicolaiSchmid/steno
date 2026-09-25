import Foundation
import StenoCore

@testable import StenoHandover

/// One running `HandoverService` on loopback with the test identity, an
/// in-memory store, core's `FakeHandoverIntake`, a `ManualClock` and a fresh
/// temporary inbox. `advertise` is always false: nothing leaves 127.0.0.1.
struct TestService {
  let service: HandoverService
  let store: MeetingStore
  let intake: FakeHandoverIntake
  let clock: ManualClock
  let directory: URL
  let now: Date

  /// The service before `start()`, for tests that watch it come up. A
  /// `customIntake` (such as a scripted one that fails first) replaces the
  /// fake behind the service; `intake` stays what `test.intake` reads.
  static func prepare(
    chunkSize: Int = 1024 * 1024,
    intake: FakeHandoverIntake = FakeHandoverIntake(),
    customIntake: (any HandoverIntake)? = nil,
    now: Date = Date(timeIntervalSince1970: 1_790_000_000),
    readTimeout: Duration = .seconds(30)
  ) throws -> TestService {
    let directory = try Fixtures.temporaryDirectory("handover")
    let store = try MeetingStore.inMemory()
    let clock = ManualClock()
    let configuration = HandoverConfiguration(
      serviceName: "Test Mac", advertise: false, chunkSize: chunkSize,
      inboxDirectory: directory.appendingPathComponent("inbox", isDirectory: true),
      pairingWindow: .seconds(300), readTimeout: readTimeout)
    let service = HandoverService(
      configuration: configuration, store: store, intake: customIntake ?? intake,
      identity: try TestIdentity.load(), clock: clock, now: { now })
    return TestService(
      service: service, store: store, intake: intake, clock: clock, directory: directory, now: now)
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

  func stop() async {
    await service.stop()
    try? FileManager.default.removeItem(at: directory)
  }

  func client(fingerprint: Data? = nil) async throws -> LoopbackClient {
    try await LoopbackClient.forService(service, fingerprint: fingerprint)
  }

  func rawClient() async throws -> RawClient {
    guard let port = await service.port else { throw ServerError.notListening }
    return RawClient(port: port, fingerprint: service.identity.fingerprint)
  }

  var metrics: ServerMetrics.Snapshot { service.metrics.snapshot }
}
