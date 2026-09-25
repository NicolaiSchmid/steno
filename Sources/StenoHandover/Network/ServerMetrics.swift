import Foundation
import Synchronization

/// Counters the tests read to prove the auth gate and the body limit did
/// what the plan says: heads seen, body bytes discarded after a rejection,
/// statuses written, connections the server closed.
final class ServerMetrics: Sendable {
  struct Snapshot: Sendable, Equatable {
    var requestHeads = 0
    var handledRequests = 0
    var discardedBodyBytes = 0
    var statuses: [UInt] = []
    var closedByServer = 0
  }

  private let state = Mutex(Snapshot())

  var snapshot: Snapshot { state.withLock { $0 } }

  func update(_ change: @Sendable (inout Snapshot) -> Void) {
    state.withLock { change(&$0) }
  }
}
