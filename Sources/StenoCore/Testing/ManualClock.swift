import Foundation

/// A `Clock<Duration>` that only moves when a test calls `advance(by:)`.
/// Sleepers whose deadline has been reached wake in deadline order, each
/// exactly once; cancelling a sleeping task throws `CancellationError`.
public final class ManualClock: Clock, @unchecked Sendable {
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

  private struct Sleeper {
    var id: UUID
    var deadline: Instant
    var continuation: CheckedContinuation<Void, any Error>
  }

  private let lock = NSLock()
  private var current: Instant
  private var sleepers: [Sleeper] = []

  public init(start: Instant = Instant()) {
    current = start
  }

  public var now: Instant {
    lock.withLock { current }
  }

  public var minimumResolution: Duration { .zero }

  /// Sleepers currently waiting for `advance(by:)`.
  public var pendingSleepers: Int {
    lock.withLock { sleepers.count }
  }

  public func sleep(until deadline: Instant, tolerance: Duration? = nil) async throws {
    let id = UUID()
    try await withTaskCancellationHandler {
      try await withCheckedThrowingContinuation {
        (continuation: CheckedContinuation<Void, any Error>) in
        let resumeNow: Bool = lock.withLock {
          if deadline <= current { return true }
          sleepers.append(Sleeper(id: id, deadline: deadline, continuation: continuation))
          return false
        }
        if resumeNow { continuation.resume() }
      }
    } onCancel: {
      let cancelled: Sleeper? = lock.withLock {
        guard let index = sleepers.firstIndex(where: { $0.id == id }) else { return nil }
        return sleepers.remove(at: index)
      }
      cancelled?.continuation.resume(throwing: CancellationError())
    }
  }

  /// Moves time forward and wakes every sleeper whose deadline has passed.
  public func advance(by duration: Duration) {
    let due: [Sleeper] = lock.withLock {
      current = current.advanced(by: duration)
      let reached = sleepers.filter { $0.deadline <= current }.sorted { $0.deadline < $1.deadline }
      sleepers.removeAll { $0.deadline <= current }
      return reached
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
