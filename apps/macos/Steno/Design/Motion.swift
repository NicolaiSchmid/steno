import SwiftUI

/// Motion tokens for the native surfaces (the menu bar popover, the bubble,
/// the detection prompt), mirroring `mobile/src/lib/motion.ts` and the web
/// UI's `--duration-*` tokens: one tempo system. Functional swaps are fast
/// and curve-driven, entrances slightly slower. No inline durations at call
/// sites.
enum Motion {
  /// Opacity and colour swaps between existing states.
  static let durationFunctional: TimeInterval = 0.150
  /// Exits are quicker than entries so the incoming state reads first.
  static let durationExit: TimeInterval = 0.120
  /// Content arriving.
  static let durationEntrance: TimeInterval = 0.250
  /// One tick of a value sampled at 1 Hz (a countdown, a progress bar): the
  /// tween lasts exactly until the next sample, so the steps read as one
  /// continuous motion. Also the `TimelineView` period of every such view.
  static let durationCountdown: TimeInterval = 1
  /// One half-cycle of an indeterminate pulse.
  static let durationPulse: TimeInterval = 1
  /// The pulse's low opacity; the high is 1.
  static let pulseOpacity: Double = 0.5

  /// Mac buttons press less than touch targets: the pointer is already on
  /// the control, so the recipe is a 2 % scale and a 5 % dim.
  static let controlPressScale: CGFloat = 0.98
  static let controlPressOpacity: Double = 0.95
  /// A disabled control, in both button styles.
  static let disabledOpacity: Double = 0.5

  static var functional: Animation {
    .timingCurve(0.4, 0, 0.2, 1, duration: durationFunctional)
  }
  /// The swap rule: every state change in a control animates over
  /// `functional`, and none of them animate under Reduce Motion.
  static func swap(reduceMotion: Bool) -> Animation? {
    reduceMotion ? nil : functional
  }
  static var exit: Animation { .timingCurve(0.4, 0, 0.2, 1, duration: durationExit) }
  static var entrance: Animation { .timingCurve(0.4, 0, 0.2, 1, duration: durationEntrance) }
  /// Linear, so consecutive 1 Hz samples join without a visible ease.
  static var countdown: Animation { .linear(duration: durationCountdown) }
  /// The countdown steps without animation under Reduce Motion.
  static func countdown(reduceMotion: Bool) -> Animation? {
    reduceMotion ? nil : countdown
  }
  /// Ease-in-out between opacity 1 and `pulseOpacity`; callers repeat it
  /// with `repeatForever(autoreverses: true)` and drop it under Reduce Motion.
  /// Only the list entry's recording dot and the processing bar pulse;
  /// nothing in the floating bubble does.
  static var pulse: Animation { .easeInOut(duration: durationPulse) }
  /// The pulse rule: under Reduce Motion the element holds at opacity 1.
  static func pulse(reduceMotion: Bool) -> Animation? {
    reduceMotion ? nil : pulse
  }
}
