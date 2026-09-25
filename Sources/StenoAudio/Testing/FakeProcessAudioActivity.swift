import Foundation

/// A scripted `ProcessAudioActivitySource` for `MeetingDetectorTests`:
/// `set(_:)` replaces the snapshot and fires a change, `setSilently(_:)`
/// replaces it without one (the poll must notice), `failure` makes
/// `snapshot()` throw.
public final class FakeProcessAudioActivity: ProcessAudioActivitySource, @unchecked Sendable {
  private let lock = NSLock()
  private var activities: [ProcessAudioActivity]
  private var continuations: [UUID: AsyncStream<Void>.Continuation] = [:]
  private var _snapshotCount = 0
  private var _failure: (any Error)?

  public init(_ initial: [ProcessAudioActivity] = []) {
    activities = initial
  }

  /// How often the detector asked.
  public var snapshotCount: Int {
    lock.lock()
    defer { lock.unlock() }
    return _snapshotCount
  }

  public var failure: (any Error)? {
    get {
      lock.lock()
      defer { lock.unlock() }
      return _failure
    }
    set {
      lock.lock()
      _failure = newValue
      lock.unlock()
    }
  }

  public func set(_ activities: [ProcessAudioActivity]) {
    lock.lock()
    self.activities = activities
    let targets = Array(continuations.values)
    lock.unlock()
    for continuation in targets { continuation.yield(()) }
  }

  public func setSilently(_ activities: [ProcessAudioActivity]) {
    lock.lock()
    self.activities = activities
    lock.unlock()
  }

  public func snapshot() throws -> [ProcessAudioActivity] {
    lock.lock()
    defer { lock.unlock() }
    _snapshotCount += 1
    if let _failure { throw _failure }
    return activities
  }

  public func changes() -> AsyncStream<Void> {
    let id = UUID()
    return AsyncStream { continuation in
      lock.lock()
      continuations[id] = continuation
      lock.unlock()
      continuation.onTermination = { [weak self] _ in
        guard let self else { return }
        self.lock.lock()
        self.continuations[id] = nil
        self.lock.unlock()
      }
    }
  }
}
