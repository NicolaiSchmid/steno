import Foundation
import StenoCore
import Synchronization

@testable import StenoHandover

/// The engine's receipt saves (`HandoverEngine.saveReceipt`) over a store.
/// Holds the first save that `holds` picks (by default the first that lists
/// a chunk; the announce's lists none) after it left the actor and before
/// the store takes it, until `release()`; with `failing`, that save then
/// throws instead of reaching the store. Records the chunk set of the held
/// save and of every later one as each goes on to the store.
final class HeldSave: Sendable {
  /// The error a `failing` held save throws on release.
  struct Refused: Error {}

  private struct State {
    var armed = true
    var reachedStore: [[Int]] = []
  }

  private let store: MeetingStore
  private let holds: @Sendable (HandoverReceipt) -> Bool
  private let failing: Bool
  private let state = Mutex(State())
  private let (heldSignal, heldContinuation) = AsyncStream<Void>.makeStream()
  private let (released, releasing) = AsyncStream<Void>.makeStream()

  init(
    store: MeetingStore, failing: Bool = false,
    holding holds: @escaping @Sendable (HandoverReceipt) -> Bool = { !$0.receivedChunks.isEmpty }
  ) {
    self.store = store
    self.failing = failing
    self.holds = holds
  }

  func save(_ receipt: HandoverReceipt) async throws {
    let picked = holds(receipt)
    let (hold, record) = state.withLock { state in
      let hold = state.armed && picked
      if hold { state.armed = false }
      return (hold, !state.armed)
    }
    if hold {
      heldContinuation.yield()
      for await _ in released {}
      if failing { throw Refused() }
    }
    if record { state.withLock { $0.reachedStore.append(receipt.receivedChunks) } }
    try await store.save(receipt)
  }

  /// Returns once the save `holds` picked is held.
  func held() async {
    for await _ in heldSignal { return }
  }

  /// The chunk set of the held save and of every later one, in the order
  /// they went on to the store.
  var reachedStore: [[Int]] { state.withLock { $0.reachedStore } }

  /// Lets the held save go on to the store, or throw when `failing`; a save
  /// that comes later is not held.
  func release() {
    releasing.finish()
  }
}
