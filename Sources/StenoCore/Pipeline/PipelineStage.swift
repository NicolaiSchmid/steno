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

  public init(stage: PipelineStage, error: any Error) {
    if let failure = error as? PipelineFailure {
      self = failure
    } else {
      self.init(stage: stage, reason: String(describing: error))
    }
  }

  public var description: String { "\(stage.rawValue): \(reason)" }
}
