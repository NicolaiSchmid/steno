import Foundation
import StenoCore
import Synchronization

@testable import StenoHandover

/// The engine's receipt writes (`HandoverEngine.saveReceipt`) over a store,
/// holding the first that lists a chunk on its way there, after it left the
/// actor and before the store takes it, until `release()`. Records the chunk
/// set of that write and of every later one as each goes on to the store.
final class HeldSave: Sendable {
  private struct State {
    var armed = true
    var released = false
    var waiter: CheckedContinuation<Void, Never>?
    var reachedStore: [[Int]] = []
  }

  private let store: MeetingStore
  private let state = Mutex(State())

  init(store: MeetingStore) {
    self.store = store
  }

  func save(_ receipt: HandoverReceipt) async throws {
    let (hold, record) = state.withLock { state in
      guard state.armed else { return (false, true) }
      guard !receipt.receivedChunks.isEmpty else { return (false, false) }
      state.armed = false
      return (true, true)
    }
    if hold {
      await withCheckedContinuation { continuation in
        let goOn = state.withLock { state in
          if state.released { return true }
          state.waiter = continuation
          return false
        }
        if goOn { continuation.resume() }
      }
    }
    if record { state.withLock { $0.reachedStore.append(receipt.receivedChunks) } }
    try await store.save(receipt)
  }

  /// Whether a write is held now.
  var isHolding: Bool { state.withLock { $0.waiter != nil } }

  /// The chunk set of the held write and of every later one, in the order
  /// they went on to the store.
  var reachedStore: [[Int]] { state.withLock { $0.reachedStore } }

  func release() {
    let waiter = state.withLock { state in
      state.released = true
      defer { state.waiter = nil }
      return state.waiter
    }
    waiter?.resume()
  }
}
