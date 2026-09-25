import Foundation

/// One detection prompt: which app opened the microphone, a countdown on
/// the injected clock, and the two actions. Auto-dismisses after `timeout`.
@MainActor
@Observable
final class DetectionPromptViewModel: Identifiable {
  enum Trigger: Equatable, Sendable {
    case microphoneOpened(bundleID: String?, appName: String)
  }

  enum Outcome: Equatable, Sendable {
    case started
    case dismissed
    case timedOut
  }

  let id = UUID()
  let trigger: Trigger
  let timeout: Duration
  private(set) var remaining: Duration
  private(set) var outcome: Outcome?
  private let clock: any Clock<Duration>
  private var countdown: Task<Void, Never>?
  /// Runs once, with the outcome, when the prompt closes.
  var onClose: ((Outcome) async -> Void)?

  init(trigger: Trigger, clock: any Clock<Duration>, timeout: Duration = .seconds(60)) {
    self.trigger = trigger
    self.clock = clock
    self.timeout = timeout
    self.remaining = timeout
  }

  var appName: String {
    switch trigger {
    case .microphoneOpened(_, let appName): appName
    }
  }

  var remainingSeconds: Int {
    Int(remaining.components.seconds)
  }

  /// Ticks once per second on the clock until the deadline, then times out.
  func begin() {
    guard countdown == nil, outcome == nil else { return }
    let clock = self.clock
    countdown = Task { [weak self] in
      while true {
        do {
          try await clock.sleep(for: .seconds(1))
        } catch {
          return
        }
        guard let self, self.outcome == nil else { return }
        self.remaining -= .seconds(1)
        if self.remaining <= .zero {
          self.countdown = nil
          await self.close(.timedOut)
          return
        }
      }
    }
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
    countdown?.cancel()
    countdown = nil
    await onClose?(outcome)
  }
}
