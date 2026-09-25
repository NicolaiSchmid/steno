/// The pipeline's stages in execution order. `progress` is posted as each
/// stage starts.
public enum PipelineStage: String, CaseIterable, Sendable, Codable, Equatable, Hashable {
  case decode
  case transcribe
  case diarize
  case matchSpeakers
  case merge
  case cleanup
  case summarize
  case persist
  case deliver
  case retention

  /// The stage's position in `allCases`, `0` for the first and below `1`
  /// for the last; what a progress bar shows when the stage starts.
  public var fraction: Double {
    let index = Self.allCases.firstIndex(of: self) ?? 0
    return Double(index) / Double(Self.allCases.count)
  }
}

/// The one failure type: any error thrown inside a stage becomes this, and
/// `ProcessingPipeline.process` marks the meeting `.failed(reason)` in one
/// place.
public struct PipelineFailure: Error, Sendable, Equatable, Hashable, CustomStringConvertible {
  public var stage: PipelineStage
  public var reason: String

  public init(stage: PipelineStage, reason: String) {
    self.stage = stage
    self.reason = reason
  }

  /// `error` itself when it already is a `PipelineFailure` (the stage it
  /// carries wins), else a failure for `stage` describing `error`.
  public static func wrapping(_ error: any Error, stage: PipelineStage) -> PipelineFailure {
    error as? PipelineFailure ?? PipelineFailure(stage: stage, reason: String(describing: error))
  }

  public var description: String { "\(stage.rawValue): \(reason)" }
}
