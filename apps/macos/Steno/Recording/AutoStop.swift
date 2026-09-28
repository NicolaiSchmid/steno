import Foundation

/// The armed auto-stop as every surface renders it: the app that closed the
/// microphone and the countdown's state. `line` is the one owner of the
/// countdown copy; the menu bar popover, the sidebar control and the bubble
/// render this value and never compose the sentence themselves. A value, so
/// `nonisolated` presentation builders can carry it.
struct AutoStopPresentation: Equatable, Sendable {
  /// nil when the app could not be named.
  var appName: String?
  var remainingText: String
  var fractionRemaining: Double

  /// "Zen closed the microphone. Stopping in 1:29." or, without a name, "The
  /// call app closed the microphone. Stopping in 1:29."
  var line: String {
    "\(appName ?? "The call app") closed the microphone. Stopping in \(remainingText)."
  }

  /// The one action next to the line on every surface.
  static let keepRecordingLabel = "Keep recording"
}

/// The recorder's armed auto-stop: the call app that released the microphone
/// and the grace countdown on the injected clock. `RecordingController` owns
/// the policy (when to arm, cancel and stop); this is what it exposes.
@MainActor
struct AutoStop {
  let appName: String?
  let countdown: Countdown

  var presentation: AutoStopPresentation {
    let countdown = countdown.presentation
    return AutoStopPresentation(
      appName: appName, remainingText: countdown.remainingText,
      fractionRemaining: countdown.fractionRemaining)
  }
}
