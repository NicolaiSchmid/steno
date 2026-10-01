import Foundation
import Observation
import StenoBridge
import StenoCore

/// What a window host hands its `TopicPublisher`: the snapshot of each topic
/// as its view models stand, and two hooks around a publish for the side
/// effects only that window has.
@MainActor
protocol TopicSource: AnyObject {
  /// The topic's snapshot. Read inside observation tracking, so the next
  /// change to anything it reads schedules the next publish. Nil publishes
  /// nothing; a topic that must reach the page as `null` returns
  /// `JSONValue.null`.
  func snapshot(for topic: BridgeTopic) -> (any Encodable)?
  /// Runs before the snapshot is built, outside tracking: a selection to
  /// apply, a model to swap. The default does nothing.
  func willPublish(_ topic: BridgeTopic)
  /// Runs after the publish; `emitted` is false while the page is not ready
  /// or no sink is attached. The default does nothing.
  func didPublish(_ topic: BridgeTopic, emitted: Bool)
}

extension TopicSource {
  func willPublish(_ topic: BridgeTopic) {}
  func didPublish(_ topic: BridgeTopic, emitted: Bool) {}
}

/// The one publishing loop behind `MainWindowBridge`, `SettingsBridge` and
/// `OnboardingBridge` (plan Decision 6). Owns the sink, the page's readiness,
/// the coalescing and the tracked snapshot build: every topic is rebuilt
/// inside `withObservationTracking`, a change to anything it read asks for a
/// publish on a later main-actor turn, and asks before that turn fold into
/// it. A topic with a minimum interval (`recording` at 20 Hz) waits out the
/// rest of its interval first. Nothing is sent before `page.ready`: the page
/// has no `window.steno` yet and an emit would be lost, so tracking is armed
/// and the snapshots are dropped; `page.ready` then publishes every topic.
@MainActor
final class TopicPublisher {
  /// The topics, in the order `start()` and `page.ready` publish them.
  let topics: [BridgeTopic]
  private weak var source: (any TopicSource)?
  private let minimumInterval: [BridgeTopic: Duration]
  private weak var events: (any BridgeEventSink)?
  /// True from `page.ready`.
  private(set) var pageReady = false
  /// True between `start()` and `stop()`.
  private(set) var isRunning = false
  private var pending: Set<BridgeTopic> = []
  private var lastPublish: [BridgeTopic: ContinuousClock.Instant] = [:]
  private var throttles: [BridgeTopic: Task<Void, Never>] = [:]

  init(
    topics: [BridgeTopic], source: any TopicSource, minimumInterval: [BridgeTopic: Duration] = [:]
  ) {
    self.topics = topics
    self.source = source
    self.minimumInterval = minimumInterval
  }

  /// The sink snapshots go to; `WebBridge` in the app, a recording sink in
  /// tests. Weak: the web view owns the configuration that owns the bridge.
  func attach(_ events: any BridgeEventSink) {
    self.events = events
  }

  /// Arms every topic's tracking and lets later changes publish. Idempotent.
  func start() {
    guard !isRunning else { return }
    isRunning = true
    for topic in topics { flush(topic) }
  }

  /// Stops following; a scheduled publish that arrives after this is dropped.
  func stop() {
    isRunning = false
    for throttle in throttles.values { throttle.cancel() }
    throttles = [:]
  }

  /// `page.ready`: from now on snapshots reach the sink, starting with every
  /// topic once, in order.
  func pageDidBecomeReady() {
    pageReady = true
    for topic in topics { flush(topic) }
  }

  /// Asks for a publish of `topic` on a later main-actor turn; a second ask
  /// before that turn is folded into it. A throttled topic waits out the
  /// rest of its interval first.
  func schedule(_ topic: BridgeTopic) {
    guard isRunning, pending.insert(topic).inserted else { return }
    if let interval = minimumInterval[topic], let last = lastPublish[topic] {
      let wait = interval - (ContinuousClock.now - last)
      if wait > .zero {
        throttles[topic] = Task { @MainActor [weak self] in
          try? await Task.sleep(for: wait)
          guard !Task.isCancelled else { return }
          self?.flush(topic)
        }
        return
      }
    }
    Task { @MainActor [weak self] in self?.flush(topic) }
  }

  /// Builds the topic's snapshot inside observation tracking and emits it
  /// once the page is ready, with the source's hooks around it.
  private func flush(_ topic: BridgeTopic) {
    guard isRunning, let source else { return }
    pending.remove(topic)
    source.willPublish(topic)
    var snapshot: (any Encodable)?
    withObservationTracking {
      snapshot = source.snapshot(for: topic)
    } onChange: { [weak self] in
      Task { @MainActor [weak self] in self?.schedule(topic) }
    }
    if minimumInterval[topic] != nil { lastPublish[topic] = ContinuousClock.now }
    var emitted = false
    if pageReady, let events, let snapshot {
      // The sink encodes once (`emit(_:snapshot:)`); the existential is
      // opened onto its generic parameter.
      events.emit(topic, snapshot: snapshot)
      emitted = true
    }
    source.didPublish(topic, emitted: emitted)
  }

  /// A bridge host lives as long as its window's task: this returns only
  /// when that task is cancelled, whatever its observers do meanwhile.
  nonisolated static func untilCancelled() async {
    while !Task.isCancelled {
      try? await Task.sleep(for: .seconds(3_600))
    }
  }
}
