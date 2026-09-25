import Foundation
import StenoCore
import Synchronization

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
/// `receipts`; `state` and both streams are `nonisolated`, so a view reads
/// them without an `await`.
public actor HandoverService {
  public nonisolated let configuration: HandoverConfiguration
  public nonisolated let identity: HandoverIdentity
  private let store: MeetingStore
  nonisolated let engine: HandoverEngine
  nonisolated let metrics = ServerMetrics()
  private var server: HandoverServer?
  private nonisolated let listenerStates = Broadcast<ListenerState>(initial: .stopped)
  private nonisolated let receiptUpdates = Broadcast<[HandoverReceipt]>(initial: [])

  /// `now` is the one time source: it stamps receipts and devices and
  /// decides when the pairing window has closed. Tests advance it.
  public init(
    configuration: HandoverConfiguration,
    store: MeetingStore,
    intake: any HandoverIntake,
    identity: HandoverIdentity,
    now: @escaping @Sendable () -> Date = Date.init
  ) {
    self.configuration = configuration
    self.identity = identity
    self.store = store
    self.engine = HandoverEngine(
      configuration: configuration, identity: identity, store: store, intake: intake,
      receipts: receiptUpdates, now: now)
  }

  // MARK: - Pairing and devices

  /// Opens a pairing window and returns what the QR code shows
  /// (`urlString`). Replaces any open session; the secret is single use and
  /// expires after `configuration.pairingWindow`.
  public func beginPairing() async -> PairingPayload {
    await engine.beginPairing()
  }

  public func cancelPairing() async {
    await engine.cancelPairing()
  }

  public func pairedDevices() async throws -> [PairedDevice] {
    try await store.pairedDevices()
  }

  /// Forgets the phone: its next request is answered 401, which the phone
  /// shows as unpaired.
  public func revoke(_ deviceID: UUID) async throws {
    try await engine.revoke(deviceID)
  }

  // MARK: - Observation

  public nonisolated var state: ListenerState { listenerStates.current }

  /// Yields the current state first, then every change. Each call is an
  /// independent subscription.
  public nonisolated var states: AsyncStream<ListenerState> { listenerStates.subscribe() }

  /// Every handover receipt touched since start, oldest first, updated as
  /// chunks arrive and a recording completes. The UI reads it directly;
  /// `receivedBytes ≈ receivedChunks.count * chunkSize`.
  public nonisolated var receipts: AsyncStream<[HandoverReceipt]> { receiptUpdates.subscribe() }

  // MARK: - Lifecycle

  /// Binds the listener (and advertises when configured). Idempotent. Sweeps
  /// orphaned inbox files first.
  public func start() async throws {
    guard server == nil else { return }
    await engine.sweepOrphans()
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
}

/// A current value plus a fan-out to any number of `AsyncStream` readers.
/// `Sendable` over a `Mutex`, so an actor hands out `current` and new
/// streams from `nonisolated` members, and a reader that stops is removed
/// from wherever `onTermination` runs, without a hop back to the owner.
final class Broadcast<Value: Sendable>: Sendable {
  private struct State {
    var current: Value
    var continuations: [UUID: AsyncStream<Value>.Continuation] = [:]
  }

  private let state: Mutex<State>

  init(initial: Value) {
    self.state = Mutex(State(current: initial))
  }

  var current: Value { state.withLock { $0.current } }

  func send(_ value: Value) {
    let continuations = state.withLock { state in
      state.current = value
      return Array(state.continuations.values)
    }
    for continuation in continuations {
      continuation.yield(value)
    }
  }

  /// A stream that yields `current` first, then every `send`, buffering the
  /// newest sixteen for a slow reader.
  func subscribe() -> AsyncStream<Value> {
    let id = UUID()
    let (stream, continuation) = AsyncStream.makeStream(
      of: Value.self, bufferingPolicy: .bufferingNewest(16))
    continuation.onTermination = { [weak self] _ in self?.remove(id) }
    state.withLock { state in
      continuation.yield(state.current)
      state.continuations[id] = continuation
    }
    return stream
  }

  private func remove(_ id: UUID) {
    state.withLock { $0.continuations.removeValue(forKey: id) }?.finish()
  }
}
