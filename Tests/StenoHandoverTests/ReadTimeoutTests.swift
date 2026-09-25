import Foundation
import NIOCore
import NIOEmbedded
import Synchronization
import Testing

@testable import StenoHandover

/// What `HTTPHandler` does with the idle handler's read event, on the child
/// pipeline `HandoverServer` builds (`configurePipeline`) over an embedded
/// channel: no listener, no sockets, no clock. NIO's `IdleStateHandler` owns
/// the timer and reads the wall clock (`NIODeadline.now()`, not the loop's
/// `now`), so an embedded loop's virtual time cannot fire it; these tests
/// fire its event themselves and check the decision, which is the part that
/// is ours. The timer over a real connection is the opt-in timing test at
/// the end.
@Suite struct ReadTimeoutTests {
  static let chunkSize = 256 * 1024
  static let timingTests =
    ProcessInfo.processInfo.environment["STENO_HANDOVER_TIMING_TESTS"] == "1"

  @Test func aSilentConnectionIsClosedOnTheReadTimeout() async throws {
    let connection = try await EmbeddedConnection.open()
    let channel = connection.channel
    try await channel.testingEventLoop.executeInContext {
      _ = try channel.pipeline.syncOperations.handler(type: IdleStateHandler.self)
    }

    // Half a request line, then nothing: without a read timeout any peer on
    // the Wi-Fi could hold hundreds of such connections open for good.
    try await channel.writeInbound(ByteBuffer(string: "GET /v1/hel"))
    #expect(connection.metrics.snapshot.requestHeads == 0)
    #expect(channel.isActive)

    // The embedded channel closes on its loop, inside `fireReadTimeout`, so
    // this is a decision to check, not a close to wait for: a handler that
    // keeps the connection fails here instead of hanging on `closeFuture`.
    try await connection.fireReadTimeout()
    try #require(!channel.isActive, "the server closes a connection that stays silent")
    let metrics = connection.metrics.snapshot
    #expect(metrics.timedOut == 1)
    #expect(metrics.requestHeads == 0, "no request line was ever completed")
    #expect(metrics.handledRequests == 0)
  }

  @Test func theReadTimeoutDoesNotCutARequestTheEngineIsStillHandling() async throws {
    let connection = try await EmbeddedConnection.open()
    let channel = connection.channel

    // The whole request is in; from here the silence is the Mac's (a long
    // verify or intake), not the client's.
    try await channel.writeInbound(
      ByteBuffer(string: "GET /v1/hello HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n"))
    await connection.engine.handling()

    // The idle handler fires once per timeout for as long as the silence lasts.
    try await connection.fireReadTimeout()
    try await connection.fireReadTimeout()
    try #require(channel.isActive, "the connection survives the engine's silence")
    #expect(connection.metrics.snapshot.timedOut == 0)
    #expect(connection.metrics.snapshot.handledRequests == 1)

    connection.engine.release()
    let response = try await channel.waitForOutboundWrite(as: ByteBuffer.self)
    #expect(String(buffer: response).hasPrefix("HTTP/1.1 200 OK\r\n"))
    #expect(connection.metrics.snapshot.statuses == [200])
    #expect(channel.isActive, "keep-alive: the connection stays open for the next request")

    // Once the response is out, the client's silence counts again.
    try await connection.fireReadTimeout()
    #expect(!channel.isActive)
    #expect(connection.metrics.snapshot.timedOut == 1)
  }

  /// The timer itself, over a real connection to the listener (TLS on
  /// macOS). Wall-clock, so opt-in: a two-second timeout is waited out under
  /// a ceiling ten times as long, and nothing asserts an upper bound on the
  /// elapsed time, so a loaded machine can make it slow but not fail.
  @Test(
    .enabled(
      if: timingTests,
      "set STENO_HANDOVER_TIMING_TESTS=1 to wait out the read timeout over a real connection"))
  func theReadTimeoutOverARealConnection() async throws {
    let intake = ScriptedIntake(meetingID: UUID(), failures: 0, delay: .seconds(4))
    try await TestService.run(
      chunkSize: Self.chunkSize, customIntake: intake, readTimeout: .seconds(2)
    ) { test in
      // Half a request line, then nothing: closed by the server.
      let raw = try test.rawClient()
      let closed = try await raw.holdOpen(Data("GET /v1/hel".utf8), timeout: .seconds(20))
      #expect(closed, "the server closes a connection that stays silent")
      #expect(test.metrics.timedOut == 1)
      #expect(test.metrics.requestHeads == 0, "no request line was ever completed")

      // A `complete` whose intake takes twice the timeout: the silence is the
      // Mac's, so the connection that carries it is answered, not cut. The
      // assertion is about that connection; the phone's own connections may
      // legitimately idle out meanwhile.
      let phone = try await Phone.pair(test.service)
      let bytes = Phone.seededBytes(count: Self.chunkSize, seed: 31)
      let metadata = phone.metadata(for: bytes, chunkSize: Self.chunkSize)
      try await phone.uploadAll(metadata, bytes)
      let completed = try await raw.exchange(
        .POST, "/v1/recordings/\(metadata.recordingID.uuidString)/complete",
        headers: [("Authorization", "Bearer \(phone.token)")], closeGrace: .milliseconds(100),
        timeout: .seconds(20))
      #expect(completed.status == 200)
      #expect(!completed.closedByServer, "the server kept the connection while the intake ran")
    }
  }
}

/// One accepted connection as the handler sees it: the pipeline
/// `HandoverServer` builds, an engine the test holds open, and the metrics
/// the handler writes.
struct EmbeddedConnection {
  let channel: NIOAsyncTestingChannel
  let engine: HeldEngine
  let metrics: ServerMetrics

  static func open() async throws -> EmbeddedConnection {
    let engine = HeldEngine()
    let metrics = ServerMetrics()
    let configuration = HandoverConfiguration(
      serviceName: "Test Mac", advertise: false,
      inboxDirectory: FileManager.default.temporaryDirectory, readTimeout: .seconds(2))
    let channel = try await NIOAsyncTestingChannel { channel in
      try HandoverServer.configurePipeline(
        of: channel, engine: engine, configuration: configuration, metrics: metrics)
    }
    try await channel.connect(to: SocketAddress(ipAddress: "127.0.0.1", port: 4242)).get()
    return EmbeddedConnection(channel: channel, engine: engine, metrics: metrics)
  }

  /// The idle handler's read event, as it fires after `readTimeout` of
  /// silence, entering the pipeline where the idle handler sits.
  func fireReadTimeout() async throws {
    let channel = self.channel
    try await channel.testingEventLoop.executeInContext {
      channel.pipeline.fireUserInboundEventTriggered(IdleStateHandler.IdleStateEvent.read)
    }
  }
}

/// An engine that lets every request through the gate and holds `handle`
/// until the test releases it, so a request is "still being handled" for
/// exactly as long as the test says.
final class HeldEngine: RequestHandling, Sendable {
  private struct State {
    var handling = false
    var released = false
    var onHandling: CheckedContinuation<Void, Never>?
    var onRelease: CheckedContinuation<Void, Never>?
  }

  private let state = Mutex(State())

  func authenticate(_ route: Route, authorization: String?) async -> AuthOutcome {
    .allowed(.anonymous)
  }

  func handle(_ request: HandoverRequest) async -> HandoverResponse {
    let waiting = state.withLock { held -> CheckedContinuation<Void, Never>? in
      held.handling = true
      defer { held.onHandling = nil }
      return held.onHandling
    }
    waiting?.resume()
    await withCheckedContinuation { continuation in
      let released = state.withLock { held -> Bool in
        if held.released { return true }
        held.onRelease = continuation
        return false
      }
      if released { continuation.resume() }
    }
    return .empty(.ok)
  }

  /// Returns once `handle` has been entered.
  func handling() async {
    await withCheckedContinuation { continuation in
      let already = state.withLock { held -> Bool in
        if held.handling { return true }
        held.onHandling = continuation
        return false
      }
      if already { continuation.resume() }
    }
  }

  /// Lets `handle` return.
  func release() {
    let waiting = state.withLock { held -> CheckedContinuation<Void, Never>? in
      held.released = true
      defer { held.onRelease = nil }
      return held.onRelease
    }
    waiting?.resume()
  }
}
