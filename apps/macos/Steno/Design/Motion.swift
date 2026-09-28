import SwiftUI

/// Motion tokens mirroring `mobile/src/lib/motion.ts`: one tempo system.
/// Functional swaps are fast and curve-driven, entrances slightly slower,
/// spatial moves use the one critically damped spring. No inline durations
/// at call sites.
enum Motion {
  /// Opacity and colour swaps between existing states.
  static let durationFunctional: TimeInterval = 0.150
  /// Exits are quicker than entries so the incoming state reads first.
  static let durationExit: TimeInterval = 0.120
  /// Content arriving.
  static let durationEntrance: TimeInterval = 0.250
  /// Press-in is near-instant; release relaxes at the functional tempo.
  static let durationPressIn: TimeInterval = 0.100

  /// The one spatial spring: about 250 ms settle, no visible bounce.
  static let springDamping: Double = 28
  static let springMass: Double = 1
  static let springStiffness: Double = 320

  static let pressScale: CGFloat = 0.97
  static let pressOpacity: Double = 0.85
  /// Mac buttons press less than touch targets: the pointer is already on
  /// the control, so the recipe is a 2 % scale and a 5 % dim.
  static let controlPressScale: CGFloat = 0.98
  static let controlPressOpacity: Double = 0.95
  /// A disabled control, in both button styles.
  static let disabledOpacity: Double = 0.5
  static let hitSlop: CGFloat = 10

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
  static var spatial: Animation {
    .interpolatingSpring(mass: springMass, stiffness: springStiffness, damping: springDamping)
  }

  /// The clock tempo: a countdown hairline drains one linear step per tick.
  static let durationCountdown: TimeInterval = 1
  static var countdown: Animation { .linear(duration: durationCountdown) }
  /// The countdown steps without animation under Reduce Motion.
  static func countdown(reduceMotion: Bool) -> Animation? {
    reduceMotion ? nil : countdown
  }

  /// The one ambient animation per viewport: opacity 1 to `pulseOpacity`
  /// and back over a second, ease-in-out, repeating. Only the list entry's
  /// recording dot pulses; nothing in the floating bubble does.
  static let durationPulse: TimeInterval = 1
  static let pulseOpacity: Double = 0.4
  static var pulse: Animation {
    .easeInOut(duration: durationPulse).repeatForever(autoreverses: true)
  }
  /// The pulse rule: under Reduce Motion the element holds at opacity 1.
  static func pulse(reduceMotion: Bool) -> Animation? {
    reduceMotion ? nil : pulse
  }
}
