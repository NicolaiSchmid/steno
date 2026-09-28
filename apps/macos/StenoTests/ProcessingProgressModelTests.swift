import StenoCore
import XCTest

/// `ProcessingProgressModel` driven directly through `apply` and
/// `meetingsChanged`, the two entry points `observe` feeds: the clamp within
/// a run, the reset a new run brings, and eviction. The bus-driven wiring is
/// proven in `AppControllerTests` and `MenuBarViewModelTests`.
@MainActor
final class ProcessingProgressModelTests: XCTestCase {
  private let meetingID = UUID()

  /// A model whose `now` follows `clock`, so a fresh `since` is observable.
  private func makeModel(clock: ManualClock = ManualClock()) -> ProcessingProgressModel {
    ProcessingProgressModel(now: {
      TestSupport.now.addingTimeInterval(clock.now.offset / .seconds(1))
    })
  }

  private func progress(_ stage: PipelineStage, _ fraction: Double, next: Double)
    -> ProcessingProgress
  {
    ProcessingProgress(
      stage: stage, fraction: fraction, nextFraction: next, estimatedRemaining: .seconds(60),
      isEstimateSeeded: false)
  }

  private func meeting(_ state: MeetingState) -> Meeting {
    var meeting = SampleData.meeting(state: state)
    meeting.id = meetingID
    return meeting
  }

  func testAListedProcessingMeetingGetsAWaitingEntry() throws {
    let model = makeModel()
    model.meetingsChanged([meeting(.processing)])
    let entry = try XCTUnwrap(model.entry(for: meetingID))
    XCTAssertNil(entry.progress)
    XCTAssertNil(entry.stage)
    XCTAssertEqual(entry.title, "Waiting to process")
    XCTAssertEqual(entry.fraction, 0)
    XCTAssertEqual(entry.since, TestSupport.now, "stamped with the injected clock")
  }

  func testALowerFractionForTheSameMeetingDoesNotLowerTheEntry() throws {
    let model = makeModel()
    model.meetingsChanged([meeting(.processing)])
    model.apply(.progress(meetingID: meetingID, progress: progress(.diarize, 0.5, next: 0.7)))
    model.apply(
      .progress(meetingID: meetingID, progress: progress(.matchSpeakers, 0.4, next: 0.45)))
    let entry = try XCTUnwrap(model.entry(for: meetingID))
    XCTAssertEqual(entry.fraction, 0.5, "the bar never moves backwards")
    XCTAssertEqual(entry.progress?.nextFraction, 0.5, "lifted to the clamped fraction")
    XCTAssertEqual(entry.title, "Matching speakers…", "the stage still advances")
  }

  func testADecodeEventStartsTheRunAgainAtZero() throws {
    let model = makeModel()
    model.meetingsChanged([meeting(.processing)])
    model.apply(.progress(meetingID: meetingID, progress: progress(.cleanup, 0.6, next: 0.9)))
    XCTAssertEqual(model.entry(for: meetingID)?.fraction, 0.6)
    model.apply(.progress(meetingID: meetingID, progress: progress(.decode, 0, next: 0.05)))
    let entry = try XCTUnwrap(model.entry(for: meetingID))
    XCTAssertEqual(entry.fraction, 0, "a new run starts at zero")
    XCTAssertEqual(entry.progress?.nextFraction, 0.05)
    XCTAssertEqual(entry.title, "Decoding…")
  }

  /// A `.decode` event creates the entry on its own, and a list snapshot
  /// that does not carry the meeting yet (it predates the row) leaves the
  /// entry alone; only a snapshot listing the meeting in another state, or
  /// the `deleted` event, evicts it.
  func testAMeetingAbsentFromTheListKeepsItsEntryUntilListedElsewhereOrDeleted() throws {
    let model = makeModel()
    model.apply(.progress(meetingID: meetingID, progress: progress(.decode, 0, next: 0.05)))
    XCTAssertEqual(model.entry(for: meetingID)?.stage, .decode, "the event alone creates it")
    model.meetingsChanged([])
    XCTAssertNotNil(model.entry(for: meetingID), "an early snapshot does not evict")
    model.meetingsChanged([meeting(.processing)])
    XCTAssertEqual(model.entry(for: meetingID)?.stage, .decode, "listing it keeps the progress")

    model.meetingsChanged([meeting(.ready)])
    XCTAssertNil(model.entry(for: meetingID), "listed as ready evicts")

    model.apply(.progress(meetingID: meetingID, progress: progress(.decode, 0, next: 0.05)))
    XCTAssertNotNil(model.entry(for: meetingID))
    model.apply(.deleted(meetingID: meetingID))
    XCTAssertNil(model.entry(for: meetingID), "deleted evicts")
    model.apply(.progress(meetingID: meetingID, progress: progress(.cleanup, 0.6, next: 0.9)))
    XCTAssertNil(model.entry(for: meetingID), "a later stage without an entry drives nothing")
  }

  /// Processing, ready, queued again: the entry from the first run goes with
  /// the `.ready` snapshot, and the re-entered meeting gets a fresh entry
  /// without progress stamped with the later time.
  func testAMeetingProcessedAgainReentersWithAFreshEntry() throws {
    let clock = ManualClock()
    let model = makeModel(clock: clock)
    model.meetingsChanged([meeting(.processing)])
    model.apply(.progress(meetingID: meetingID, progress: progress(.cleanup, 0.6, next: 0.9)))
    model.meetingsChanged([meeting(.ready)])
    XCTAssertNil(model.entry(for: meetingID))

    clock.advance(by: .seconds(90))
    model.meetingsChanged([meeting(.queued)])
    let entry = try XCTUnwrap(model.entry(for: meetingID))
    XCTAssertNil(entry.progress, "no progress from the earlier run")
    XCTAssertEqual(entry.title, "Waiting to process")
    XCTAssertEqual(entry.since, TestSupport.now.addingTimeInterval(90), "a fresh since")
  }

  func testOtherEventsAndOtherMeetingsDriveNothing() {
    let model = makeModel()
    model.meetingsChanged([meeting(.processing)])
    model.apply(.speakersNeedReview(meetingID: meetingID, speakerIDs: []))
    model.apply(.retentionApplied(meetingID: meetingID))
    XCTAssertNil(model.entry(for: meetingID)?.progress)
    let other = UUID()
    model.apply(.progress(meetingID: other, progress: progress(.summarize, 0.8, next: 0.9)))
    XCTAssertNil(model.entry(for: other), "a re-run on an untracked meeting is not tracked")
    XCTAssertEqual(model.entries.count, 1)
  }
}
