import Foundation
import Synchronization

/// A `Clock<Duration>` that only moves when a test calls `advance(by:)`.
/// Sleepers whose deadline has been reached wake in deadline order, each
/// exactly once; cancelling a sleeping task throws `CancellationError`.
public final class ManualClock: Clock, Sendable {
  public struct Instant: InstantProtocol, Sendable, Hashable, Comparable, CustomStringConvertible {
    public var offset: Duration

    public init(offset: Duration = .zero) {
      self.offset = offset
    }

    public func advanced(by duration: Duration) -> Instant {
      Instant(offset: offset + duration)
    }

    public func duration(to other: Instant) -> Duration {
      other.offset - offset
    }

    public static func < (lhs: Instant, rhs: Instant) -> Bool {
      lhs.offset < rhs.offset
    }

    public var description: String { "\(offset)" }
  }

  private struct Sleeper: Sendable {
    var id: UUID
    var deadline: Instant
    var continuation: CheckedContinuation<Void, any Error>
  }

  private struct State: Sendable {
    var current: Instant
    var sleepers: [Sleeper] = []
  }

  private let state: Mutex<State>

  public init(start: Instant = Instant()) {
    state = Mutex(State(current: start))
  }

  public var now: Instant {
    state.withLock { $0.current }
  }

  public var minimumResolution: Duration { .zero }

  /// Sleepers currently waiting for `advance(by:)`.
  public var pendingSleepers: Int {
    state.withLock { $0.sleepers.count }
  }

  public func sleep(until deadline: Instant, tolerance: Duration? = nil) async throws {
    let id = UUID()
    try await withTaskCancellationHandler {
      try await withCheckedThrowingContinuation {
        (continuation: CheckedContinuation<Void, any Error>) in
        let resumeNow = state.withLock { state -> Bool in
          if deadline <= state.current { return true }
          state.sleepers.append(Sleeper(id: id, deadline: deadline, continuation: continuation))
          return false
        }
        if resumeNow { continuation.resume() }
      }
    } onCancel: {
      let cancelled = state.withLock { state -> Sleeper? in
        guard let index = state.sleepers.firstIndex(where: { $0.id == id }) else { return nil }
        return state.sleepers.remove(at: index)
      }
      cancelled?.continuation.resume(throwing: CancellationError())
    }
  }

  /// Moves time forward and wakes every sleeper whose deadline has passed.
  public func advance(by duration: Duration) {
    let due = state.withLock { state -> [Sleeper] in
      state.current = state.current.advanced(by: duration)
      let reached = state.sleepers.filter { $0.deadline <= state.current }
      state.sleepers.removeAll { $0.deadline <= state.current }
      return reached.sorted { $0.deadline < $1.deadline }
    }
    for sleeper in due {
      sleeper.continuation.resume()
    }
  }

  /// Yields to the cooperative pool until `count` sleepers are waiting or
  /// `attempts` yields have passed; wall time never enters.
  public func waitForSleepers(_ count: Int, attempts: Int = 10_000) async -> Bool {
    for _ in 0..<attempts {
      if pendingSleepers >= count { return true }
      await Task.yield()
    }
    return pendingSleepers >= count
  }
}
