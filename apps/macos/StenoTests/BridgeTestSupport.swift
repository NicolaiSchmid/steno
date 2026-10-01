import Foundation
import StenoBridge
import StenoCore
import Testing

// What the hostless bridge tests share (`MainWindowSnapshotsTests`,
// `SettingsSnapshotsTests`, `OnboardingSnapshotsTests`, `WebHostTests`): a
// sink that records where `WebBridge` would evaluate JavaScript, a poll for
// the main-actor hops a publish takes, and the request envelope as the page
// posts it.

/// Records every event a host publishes.
@MainActor
final class RecordingSink: BridgeEventSink {
  private(set) var events: [BridgeEvent] = []

  func emit(_ event: BridgeEvent) {
    events.append(event)
  }

  /// The payload of the last event, whatever its topic.
  var last: JSONValue? { events.last?.payload }

  /// The payload of the last event of `topic`.
  func last(_ topic: BridgeTopic) -> JSONValue? {
    events.last { $0.topic == topic }?.payload
  }
}

/// Polls `condition` every 10 ms up to `timeout` and records an issue on
/// timeout. Used only where a store observation or a main-actor hop must be
/// given time to deliver.
@MainActor
func eventually(
  _ description: String, timeout: Duration = .seconds(10), _ condition: @MainActor () -> Bool
) async {
  let clock = ContinuousClock()
  let deadline = clock.now + timeout
  while !condition() {
    if clock.now >= deadline {
      Issue.record("timed out waiting for \(description)")
      return
    }
    try? await Task.sleep(for: .milliseconds(10))
  }
}

/// A request body as `webkit.messageHandlers.steno.postMessage` posts it.
func request(_ id: String, _ method: String, _ params: Any = NSNull()) -> [String: Any] {
  ["id": id, "method": method, "params": params]
}
