import Foundation

/// What the pipeline and the store tell the UI beyond row changes (which
/// arrive through `MeetingStore.observe*`).
public enum MeetingEvent: Sendable, Equatable, Hashable {
  /// Posted once as each stage starts; `stage.fraction` is the progress bar
  /// value.
  case progress(meetingID: UUID, stage: PipelineStage)
  /// Posted after persist when any speaker is not `.confirmed`.
  case speakersNeedReview(meetingID: UUID, speakerIDs: [UUID])
  /// Posted once the `retention` stage has written the asset's `expiresAt`
  /// (nil for `.keepForever`): the run is over and the files may be due.
  /// The app runs `RetentionSweep.run(now:)` on this event and at launch.
  /// The `.ready` row change is not a sweep trigger: `persist` writes it
  /// before `deliver` and `retention` run, so a sweep started from
  /// `observeMeetings` finds no expired asset yet.
  case retentionApplied(meetingID: UUID)
  /// Posted by `MeetingStore.delete(meetingID:)` once the rows are gone, so a
  /// view showing the meeting can close before its observation ends with
  /// `nil`.
  case deleted(meetingID: UUID)
}
