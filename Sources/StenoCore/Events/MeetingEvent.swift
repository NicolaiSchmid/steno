import Foundation

/// What the pipeline and the store tell the UI beyond row changes (which
/// arrive through `MeetingStore.observe*`).
public enum MeetingEvent: Sendable, Equatable, Hashable {
  /// Posted once as each stage starts; `stage.fraction` is the progress bar
  /// value.
  case progress(meetingID: UUID, stage: PipelineStage)
  /// Posted after persist when any speaker is not `.confirmed`.
  case speakersNeedReview(meetingID: UUID, speakerIDs: [UUID])
  /// Posted by `MeetingStore.delete(meetingID:)` once the rows are gone, so a
  /// view showing the meeting can close before its observation ends with
  /// `nil`.
  case deleted(meetingID: UUID)
}
