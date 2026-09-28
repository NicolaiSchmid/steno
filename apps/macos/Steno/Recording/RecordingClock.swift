import Foundation
import Observation

/// One ticking clock for the surfaces outside the main window (the floating
/// bubble, the menu bar label): `now` advances once a second on the injected
/// clock while the recorder is `.recording`, and holds otherwise. The views
/// read `now` and re-render through observation, so no view owns a timer
/// and no `TimelineView` runs inside a panel or a status item.
@MainActor
@Observable
final class RecordingClock {
  private(set) var now: Date
  private let clock: any Clock<Duration>
  private let dateNow: @Sendable () -> Date
  private var ticker: Task<Void, Never>?

  init(
    clock: any Clock<Duration> = ContinuousClock(),
    now dateNow: @escaping @Sendable () -> Date = Date.init
  ) {
    self.clock = clock
    self.dateNow = dateNow
    self.now = dateNow()
  }

  var isTicking: Bool { ticker != nil }

  /// Starts ticking on `.recording`, stops on every other state.
  func update(for state: RecordingState) {
    if case .recording = state {
      start()
    } else {
      stop()
    }
  }

  /// Seconds from `since` to the last tick, never negative.
  func elapsed(since: Date) -> TimeInterval {
    max(0, now.timeIntervalSince(since))
  }

  private func start() {
    guard ticker == nil else { return }
    now = dateNow()
    let clock = self.clock
    ticker = Task { [weak self] in
      while !Task.isCancelled {
        do {
          try await clock.sleep(for: .seconds(1))
        } catch {
          return
        }
        guard let self, !Task.isCancelled else { return }
        self.now = self.dateNow()
      }
    }
  }

  private func stop() {
    ticker?.cancel()
    ticker = nil
  }
}
