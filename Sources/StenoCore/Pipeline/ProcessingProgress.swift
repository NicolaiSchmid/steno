import Foundation

/// What a `progress` event carries: one fraction weighted by how long each
/// stage is expected to take on this Mac, and the time the run is expected
/// to still need. Computed by the pipeline from a `ProcessingRun`; nothing
/// else computes a fraction or a pace.
public struct ProcessingProgress: Sendable, Equatable, Hashable {
  public var stage: PipelineStage
  /// elapsed / (elapsed + expectedRemaining) at the moment of posting,
  /// clamped so it never decreases within one run.
  public var fraction: Double
  /// Where the next progress event is expected to land: the end of the
  /// stage's share, or the lane boundary inside transcribe. At most 1,
  /// never below `fraction`.
  public var nextFraction: Double
  /// Wall clock the run is expected to still take.
  public var estimatedRemaining: Duration
  /// True while any rate behind the estimate is still a seed.
  public var isEstimateSeeded: Bool

  public init(
    stage: PipelineStage,
    fraction: Double,
    nextFraction: Double,
    estimatedRemaining: Duration,
    isEstimateSeeded: Bool
  ) {
    self.stage = stage
    self.fraction = fraction
    self.nextFraction = nextFraction
    self.estimatedRemaining = estimatedRemaining
    self.isEstimateSeeded = isEstimateSeeded
  }

  /// (nextFraction - fraction) / (1 - fraction) of `estimatedRemaining`, so
  /// no presenter divides.
  public var expectedTimeToNextEvent: Duration {
    guard fraction < 1 else { return .zero }
    let share = max(0, min(1, (nextFraction - fraction) / (1 - fraction)))
    return estimatedRemaining * share
  }
}
