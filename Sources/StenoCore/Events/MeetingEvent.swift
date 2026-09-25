import Foundation

/// What the pipeline tells the UI beyond row changes (which arrive through
/// `MeetingStore.observe*`).
public enum MeetingEvent: Sendable, Equatable, Hashable {
  /// Posted once as each stage starts; `stage.fraction` is the progress bar
  /// value.
  case progress(meetingID: UUID, stage: PipelineStage)
  /// Posted after persist when any speaker is not `.confirmed`.
  case speakersNeedReview(meetingID: UUID, speakerIDs: [UUID])
}
