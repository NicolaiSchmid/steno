import Foundation

/// What the pipeline tells the UI beyond row changes (which arrive through
/// `MeetingStore.observe*`).
public enum MeetingEvent: Sendable, Equatable, Hashable {
  /// Posted once as each stage starts; `fraction` is the stage's position in
  /// `PipelineStage.allCases`.
  case progress(meetingID: UUID, stage: PipelineStage, fraction: Double)
  /// Posted after persist when any speaker is not `.confirmed`.
  case speakersNeedReview(meetingID: UUID, speakerIDs: [UUID])
}
