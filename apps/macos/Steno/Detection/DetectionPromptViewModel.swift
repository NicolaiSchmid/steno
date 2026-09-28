import Foundation

/// One detection prompt: the app that opened the microphone, a countdown on
/// the injected clock (the shared `Countdown`, no tick loop of its own) and
/// the two actions. Auto-dismisses after `timeout`.
@MainActor
@Observable
final class DetectionPromptViewModel: Identifiable {
  enum Outcome: Equatable, Sendable {
    case started
    case dismissed
    case timedOut
  }

  let id = UUID()
  let appName: String
  let timeout: Duration
  let countdown: Countdown
  private(set) var outcome: Outcome?
  /// Runs once, with the outcome, when the prompt closes.
  var onClose: ((Outcome) async -> Void)?

  init(appName: String, clock: any Clock<Duration>, timeout: Duration = .seconds(60)) {
    self.appName = appName
    self.timeout = timeout
    self.countdown = Countdown(duration: timeout, clock: clock)
    countdown.onElapsed = { [weak self] in await self?.close(.timedOut) }
  }

  var remainingSeconds: Int {
    Int(countdown.remaining.components.seconds)
  }

  /// Ticks once per second on the clock until the deadline, then times out.
  func begin() {
    guard outcome == nil else { return }
    countdown.begin()
  }

  func start() async {
    await close(.started)
  }

  func dismiss() async {
    await close(.dismissed)
  }

  private func close(_ outcome: Outcome) async {
    guard self.outcome == nil else { return }
    self.outcome = outcome
    countdown.cancel()
    await onClose?(outcome)
  }
}
