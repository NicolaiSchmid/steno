/// The pipeline's stages in execution order. `progress` is posted as each
/// stage starts, once per lane inside `transcribe`.
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
