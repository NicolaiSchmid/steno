import Foundation

/// Fans every posted `MeetingEvent` out to every live subscriber. Events are
/// not replayed: a subscriber sees what is posted after it subscribed.
public actor MeetingEventBus {
  private var subscribers: [UUID: AsyncStream<MeetingEvent>.Continuation] = [:]

  public init() {}

  public func subscribe() -> AsyncStream<MeetingEvent> {
    let id = UUID()
    let (stream, continuation) = AsyncStream<MeetingEvent>.makeStream(
      bufferingPolicy: .unbounded)
    subscribers[id] = continuation
    continuation.onTermination = { [weak self] _ in
      Task { await self?.remove(id) }
    }
    return stream
  }

  public func post(_ event: MeetingEvent) {
    for continuation in subscribers.values {
      continuation.yield(event)
    }
  }

  /// Ends every subscription: each stream delivers what was posted before
  /// this call and then finishes, so a consumer loop can drain and end
  /// instead of being cancelled mid-buffer. A post after this reaches
  /// nobody; a later `subscribe()` starts a fresh subscription.
  public func finish() {
    for continuation in subscribers.values {
      continuation.finish()
    }
    subscribers.removeAll()
  }

  private func remove(_ id: UUID) {
    subscribers[id] = nil
  }
}
