import SwiftUI

/// Motion tokens mirroring `mobile/src/lib/motion.ts`: one tempo system.
/// Functional swaps are fast and curve-driven, entrances slightly slower,
/// spatial moves use the one critically damped spring. No inline durations
/// at call sites.
enum Motion {
  /// The standard curve for every timing-driven change (0.4, 0, 0.2, 1).
  static let easeStandard = UnitCurve.bezier(
    startControlPoint: UnitPoint(x: 0.4, y: 0), endControlPoint: UnitPoint(x: 0.2, y: 1))

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
  static let hitSlop: CGFloat = 10

  static var functional: Animation {
    .timingCurve(0.4, 0, 0.2, 1, duration: durationFunctional)
  }
  static var exit: Animation { .timingCurve(0.4, 0, 0.2, 1, duration: durationExit) }
  static var entrance: Animation { .timingCurve(0.4, 0, 0.2, 1, duration: durationEntrance) }
  static var spatial: Animation {
    .interpolatingSpring(mass: springMass, stiffness: springStiffness, damping: springDamping)
  }
}
