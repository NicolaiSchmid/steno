/// Blocks tasks until opened. Tests hold a fake mid-call at a gate so a
/// stage, a download or a warm-up stays in flight while the test looks at
/// the state around it. Opening is idempotent and releases everyone;
/// `releaseOne()` lets one waiter through and keeps the gate closed.
public actor Gate {
  private var opened = false
  private var waiting: [CheckedContinuation<Void, Never>] = []
  private var blocked: [(count: Int, continuation: CheckedContinuation<Void, Never>)] = []

  public init() {}

  public func wait() async {
    if opened { return }
    await withCheckedContinuation { continuation in
      waiting.append(continuation)
      let due = blocked.filter { $0.count <= waiting.count }
      blocked.removeAll { $0.count <= waiting.count }
      for observer in due { observer.continuation.resume() }
    }
  }

  /// Returns once `count` tasks are waiting.
  public func waitUntilBlocked(_ count: Int = 1) async {
    if waiting.count >= count { return }
    await withCheckedContinuation { blocked.append((count, $0)) }
  }

  /// Lets one waiting task through and keeps the gate closed.
  public func releaseOne() {
    guard !waiting.isEmpty else { return }
    waiting.removeFirst().resume()
  }

  public func open() {
    opened = true
    for continuation in waiting { continuation.resume() }
    waiting.removeAll()
  }
}
