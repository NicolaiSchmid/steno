import Foundation

/// What a surface renders of a countdown: the remaining time as `m:ss` and
/// the fraction left for a hairline. A value, so `nonisolated` presentation
/// builders can carry it.
struct CountdownPresentation: Equatable, Sendable {
  var remainingText: String
  var fractionRemaining: Double
}

/// One countdown on the injected clock, shared by the detection prompt and
/// the recorder's auto-stop so the app has one tick loop. Ticks once a
/// second from `begin()`, runs `onElapsed` once when the duration is up, and
/// stops on `cancel()`. Every tick is one `clock.sleep`, so a test on
/// `ManualClock` advances a second at a time.
@MainActor
@Observable
final class Countdown {
  let duration: Duration
  private(set) var remaining: Duration
  /// True once `onElapsed` has run; a cancelled countdown never sets it.
  private(set) var hasElapsed = false
  /// Runs on the main actor when the countdown reaches zero. Settable so an
  /// owner can point it at itself after its own `init`.
  var onElapsed: @MainActor () async -> Void
  private let clock: any Clock<Duration>
  /// Not observed: surfaces track `remaining` and `hasElapsed`; `isRunning`
  /// is for owners and tests. Stored plainly so `deinit` can cancel it.
  @ObservationIgnored private var ticker: Task<Void, Never>?

  init(
    duration: Duration, clock: any Clock<Duration>,
    onElapsed: @escaping @MainActor () async -> Void = {}
  ) {
    self.duration = duration
    self.remaining = duration
    self.clock = clock
    self.onElapsed = onElapsed
  }

  deinit {
    ticker?.cancel()
  }

  var isRunning: Bool { ticker != nil }

  /// `remaining / duration`, clamped to 0...1.
  var fractionRemaining: Double {
    let total = Self.seconds(duration)
    guard total > 0 else { return 0 }
    return min(1, max(0, Self.seconds(remaining) / total))
  }

  /// "1:29" for 89 seconds, "0:05" for five; never negative.
  var remainingText: String {
    let whole = max(0, Int(remaining.components.seconds))
    return "\(whole / 60):" + String(format: "%02d", whole % 60)
  }

  var presentation: CountdownPresentation {
    CountdownPresentation(remainingText: remainingText, fractionRemaining: fractionRemaining)
  }

  /// Starts ticking; a second call while running or after the end is a
  /// no-op. A `cancel()` then `begin()` leaves one loop: the cancelled task
  /// checks its own cancellation after the sleep, so it never mistakes the
  /// new ticker for itself.
  func begin() {
    guard ticker == nil, !hasElapsed else { return }
    let clock = self.clock
    ticker = Task { [weak self] in
      while true {
        do {
          try await clock.sleep(for: .seconds(1))
        } catch {
          return  // cancelled
        }
        guard !Task.isCancelled, let self, self.ticker != nil else { return }
        self.remaining = max(.zero, self.remaining - .seconds(1))
        if self.remaining <= .zero {
          self.ticker = nil
          self.hasElapsed = true
          await self.onElapsed()
          return
        }
      }
    }
  }

  /// Stops the ticks; `onElapsed` never runs after this. `remaining` keeps
  /// its last value.
  func cancel() {
    ticker?.cancel()
    ticker = nil
  }

  private static func seconds(_ duration: Duration) -> Double {
    let components = duration.components
    return Double(components.seconds) + Double(components.attoseconds) / 1e18
  }
}
