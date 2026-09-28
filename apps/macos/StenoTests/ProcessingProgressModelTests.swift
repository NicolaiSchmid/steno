import StenoCore
import XCTest

/// `ProcessingProgressModel` fed from the bus and the meeting list as
/// `AppController.launch()` feeds it: the clamp within a run and the reset a
/// new run brings.
@MainActor
final class ProcessingProgressModelTests: XCTestCase {
  private var observing: [Task<Void, Never>] = []

  /// A nonisolated override under Swift 6.0: hop to the main actor for the
  /// isolated state.
  override func tearDown() async throws {
    await MainActor.run {
      for task in observing { task.cancel() }
      observing = []
    }
  }

  /// The model subscribed to the bus and the list before returning, so an
  /// event posted right after is seen; one processing meeting is stored.
  private func makeModel(_ environment: AppEnvironment) async throws -> (
    model: ProcessingProgressModel, meetingID: UUID
  ) {
    let model = ProcessingProgressModel(now: { TestSupport.now })
    let events = await environment.events.subscribe()
    observing.append(
      Task {
        for await event in events { model.apply(event) }
      })
    observing.append(
      Task {
        do {
          for try await meetings in environment.store.observeMeetings() {
            model.meetingsChanged(meetings)
          }
        } catch {
          XCTFail("meeting list unavailable: \(error)")
        }
      })
    var meeting = SampleData.meeting(state: .processing)
    meeting.id = UUID()
    try await environment.store.save(meeting)
    await TestSupport.waitUntil("the meeting on the model") { model.entry(for: meeting.id) != nil }
    let entry = try XCTUnwrap(model.entry(for: meeting.id))
    XCTAssertNil(entry.progress)
    XCTAssertEqual(entry.since, TestSupport.now, "stamped with the injected clock")
    return (model, meeting.id)
  }

  private func progress(_ stage: PipelineStage, _ fraction: Double, next: Double)
    -> ProcessingProgress
  {
    ProcessingProgress(
      stage: stage, fraction: fraction, nextFraction: next, estimatedRemaining: .seconds(60),
      isEstimateSeeded: false)
  }

  func testALowerFractionForTheSameMeetingDoesNotLowerTheEntry() async throws {
    let environment = try await TestSupport.environment(seed: false)
    let (model, meetingID) = try await makeModel(environment)

    await environment.events.post(
      .progress(meetingID: meetingID, progress: progress(.diarize, 0.5, next: 0.7)))
    await TestSupport.waitUntil("diarize on the model") {
      model.entry(for: meetingID)?.stage == .diarize
    }
    await environment.events.post(
      .progress(meetingID: meetingID, progress: progress(.matchSpeakers, 0.4, next: 0.45)))
    await TestSupport.waitUntil("matchSpeakers on the model") {
      model.entry(for: meetingID)?.stage == .matchSpeakers
    }
    let entry = try XCTUnwrap(model.entry(for: meetingID))
    XCTAssertEqual(entry.fraction, 0.5, "the bar never moves backwards")
    XCTAssertEqual(entry.progress?.nextFraction, 0.5, "lifted to the clamped fraction")
    XCTAssertEqual(entry.title, "Matching speakers…", "the stage still advances")
  }

  func testADecodeEventStartsTheRunAgainAtZero() async throws {
    let environment = try await TestSupport.environment(seed: false)
    let (model, meetingID) = try await makeModel(environment)

    await environment.events.post(
      .progress(meetingID: meetingID, progress: progress(.cleanup, 0.6, next: 0.9)))
    await TestSupport.waitUntil("cleanup on the model") {
      model.entry(for: meetingID)?.stage == .cleanup
    }
    XCTAssertEqual(model.entry(for: meetingID)?.fraction, 0.6)

    await environment.events.post(
      .progress(meetingID: meetingID, progress: progress(.decode, 0, next: 0.05)))
    await TestSupport.waitUntil("decode on the model") {
      model.entry(for: meetingID)?.stage == .decode
    }
    let entry = try XCTUnwrap(model.entry(for: meetingID))
    XCTAssertEqual(entry.fraction, 0, "a new run starts at zero")
    XCTAssertEqual(entry.progress?.nextFraction, 0.05)
    XCTAssertEqual(entry.title, "Decoding…")
  }
}
