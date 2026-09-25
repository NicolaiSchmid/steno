import Foundation
import StenoCore

/// Where the listener stands. Named `ListenerState` because core's
/// `HandoverState` is the per-recording receipt state.
public enum ListenerState: Sendable, Equatable {
  case stopped
  case listening(port: UInt16)
  case failed(String)
}

/// The Mac side of the handover as the app and the CLI see it: one actor
/// that owns the listener, the pairing session and the receipt stream. The
/// app renders `beginPairing().urlString` as a QR code, lists
/// `pairedDevices()`, calls `revoke(_:)`, and observes `states` and
/// `receipts`.
public actor HandoverService {
  public nonisolated let configuration: HandoverConfiguration
  public nonisolated let identity: HandoverIdentity
  private let store: MeetingStore
  private let intake: any HandoverIntake
  private let now: @Sendable () -> Date
  let engine: HandoverEngine
  nonisolated let metrics = ServerMetrics()
  private var server: HandoverServer?
  private var listenerStates = Broadcast<ListenerState>(initial: .stopped)

  public init(
    configuration: HandoverConfiguration,
    store: MeetingStore,
    intake: any HandoverIntake,
    identity: HandoverIdentity,
    clock: any Clock<Duration> = ContinuousClock(),
    now: @escaping @Sendable () -> Date = Date.init
  ) {
    self.configuration = configuration
    self.store = store
    self.intake = intake
    self.identity = identity
    self.now = now
    self.engine = HandoverEngine(
      configuration: configuration, macID: identity.macID, store: store, intake: intake, now: now)
  }

  /// The id in the Bonjour TXT record, the QR payload and `/v1/hello`.
  public nonisolated var macID: UUID { identity.macID }

  public var state: ListenerState { listenerStates.current }

  /// Yields the current state first, then every change. Each call is an
  /// independent subscription.
  public var states: AsyncStream<ListenerState> {
    listenerStates.subscribe { [weak self] id in
      Task { await self?.unsubscribeState(id) }
    }
  }

  private func unsubscribeState(_ id: UUID) {
    listenerStates.remove(id)
  }

  /// Binds the listener (and advertises when configured). Idempotent.
  public func start() async throws {
    guard server == nil else { return }
    do {
      let server = try await HandoverServer.start(
        configuration: configuration, identity: identity, engine: engine, metrics: metrics)
      self.server = server
      listenerStates.send(.listening(port: server.port))
    } catch {
      listenerStates.send(.failed(String(describing: error)))
      throw error
    }
  }

  public func stop() async {
    guard let server else { return }
    self.server = nil
    await server.stop()
    listenerStates.send(.stopped)
  }

  /// The bound port while listening.
  public var port: UInt16? { server?.port }

  /// `https://127.0.0.1:<port>` while listening (plain `http` on Linux).
  var loopbackURL: URL? {
    server.map { URL(string: "\($0.scheme)://127.0.0.1:\($0.port)")! }
  }
}

/// A current value plus a fan-out to any number of `AsyncStream` readers.
struct Broadcast<Value: Sendable>: Sendable {
  private(set) var current: Value
  private var continuations: [UUID: AsyncStream<Value>.Continuation] = [:]

  init(initial: Value) {
    self.current = initial
  }

  mutating func send(_ value: Value) {
    current = value
    for continuation in continuations.values {
      continuation.yield(value)
    }
  }

  /// A stream that yields `current` first. `onTerminate` runs off the owner's
  /// isolation when the reader stops; it must hop back to call `remove`.
  mutating func subscribe(onTerminate: @escaping @Sendable (UUID) -> Void) -> AsyncStream<Value> {
    let id = UUID()
    let (stream, continuation) = AsyncStream.makeStream(
      of: Value.self, bufferingPolicy: .bufferingNewest(16))
    continuation.yield(current)
    continuation.onTermination = { _ in onTerminate(id) }
    continuations[id] = continuation
    return stream
  }

  mutating func remove(_ id: UUID) {
    continuations.removeValue(forKey: id)?.finish()
  }

  mutating func finishAll() {
    for continuation in continuations.values {
      continuation.finish()
    }
    continuations.removeAll()
  }
}
