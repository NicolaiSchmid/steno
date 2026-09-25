import StenoCore
import XCTest

@MainActor
final class AppEnvironmentTests: XCTestCase {
  func testPreviewRootBuildsAndIsSeeded() async throws {
    let environment = try await TestSupport.environment()
    XCTAssertTrue(environment.isPreview)
    let meetings = try await environment.store.meetings()
    XCTAssertEqual(meetings.map(\.id), [SampleData.meetingID])
    let export = try await environment.store.export(meetingID: SampleData.meetingID)
    XCTAssertEqual(export.segments.count, 3)
    XCTAssertEqual(export.speakers.count, 2)
    XCTAssertEqual(export.tasks.count, 1)
    XCTAssertNotNil(export.meeting.summary)
    let settings = try await environment.settings.load()
    XCTAssertFalse(settings.launchAtLogin)
    XCTAssertNil(environment.handover)
  }

  func testReloadPipelineReplacesTheInstance() async throws {
    let environment = try await TestSupport.environment()
    let before = environment.pipeline
    try await environment.reloadPipeline()
    XCTAssertFalse(before === environment.pipeline)
  }

  func testReconcileMarksInterruptedRecordingsFailed() async throws {
    let environment = try await TestSupport.environment()
    var meeting = SampleData.meeting(state: .recording)
    meeting.id = UUID()
    try await environment.store.save(meeting)
    await environment.reconcileInterruptedRecordings()
    let stored = try await environment.store.meeting(id: meeting.id)
    XCTAssertEqual(stored?.state.kind, .failed)
    let untouched = try await environment.store.meeting(id: SampleData.meetingID)
    XCTAssertEqual(untouched?.state, .ready)
  }

}
