import Foundation
import Observation

/// What a countdown renders: the `m:ss` text and the fraction of the
/// duration still to run. A value, so the bubble's presentation can carry
/// it without touching the main-actor class.
struct CountdownPresentation: Equatable, Sendable {
  var remainingText: String
  var fractionRemaining: Double

  static func make(remaining: Duration, duration: Duration) -> CountdownPresentation {
    CountdownPresentation(
      remainingText: Self.text(remaining),
      fractionRemaining: Self.fraction(remaining: remaining, duration: duration))
  }

  /// "1:29", "0:05".
  static func text(_ remaining: Duration) -> String {
    let clamped = max(remaining, .zero)
    return clamped.formatted(.time(pattern: .minuteSecond))
  }

  /// Clamped to 0...1; a zero duration reads as elapsed.
  static func fraction(remaining: Duration, duration: Duration) -> Double {
    guard duration > .zero else { return 0 }
    let fraction = Self.seconds(remaining) / Self.seconds(duration)
    return min(1, max(0, fraction))
  }

  private static func seconds(_ duration: Duration) -> Double {
    let components = duration.components
    return Double(components.seconds) + Double(components.attoseconds) / 1e18
  }
}

/// One countdown on the injected clock: ticks once a second from
/// `duration` to zero, then runs `onElapsed` once. The detection prompt
/// holds one (no number shown); the auto-stop shows both the hairline and
/// the number. `cancel()` stops the ticks; nothing here sleeps on wall time
/// unless the clock does.
@MainActor
@Observable
final class Countdown {
  let duration: Duration
  private(set) var remaining: Duration
  private(set) var hasElapsed = false
  private let clock: any Clock<Duration>
  /// Runs once, on the main actor, when `remaining` reaches zero.
  var onElapsed: @MainActor () -> Void
  private var ticker: Task<Void, Never>?

  init(
    duration: Duration, clock: any Clock<Duration>,
    onElapsed: @escaping @MainActor () -> Void = {}
  ) {
    self.duration = duration
    self.remaining = duration
    self.clock = clock
    self.onElapsed = onElapsed
  }

  /// 1 at the start, 0 when elapsed.
  var fractionRemaining: Double {
    CountdownPresentation.fraction(remaining: remaining, duration: duration)
  }

  /// "1:29".
  var remainingText: String { CountdownPresentation.text(remaining) }

  var presentation: CountdownPresentation {
    CountdownPresentation.make(remaining: remaining, duration: duration)
  }

  /// Starts ticking; a second call while ticking or after elapsing is a no-op.
  func begin() {
    guard ticker == nil, !hasElapsed else { return }
    if remaining <= .zero {
      elapse()
      return
    }
    let clock = self.clock
    ticker = Task { [weak self] in
      while !Task.isCancelled {
        do {
          try await clock.sleep(for: .seconds(1))
        } catch {
          return
        }
        guard let self, !Task.isCancelled else { return }
        self.remaining = max(self.remaining - .seconds(1), .zero)
        if self.remaining <= .zero {
          self.ticker = nil
          self.elapse()
          return
        }
      }
    }
  }

  func cancel() {
    ticker?.cancel()
    ticker = nil
  }

  private func elapse() {
    guard !hasElapsed else { return }
    hasElapsed = true
    onElapsed()
  }
}

#if DEBUG
  extension Countdown {
    /// A countdown frozen at `remaining`, for previews.
    static func preview(duration: Duration, remaining: Duration) -> Countdown {
      let countdown = Countdown(duration: duration, clock: ContinuousClock())
      countdown.remaining = remaining
      return countdown
    }
  }
#endif
