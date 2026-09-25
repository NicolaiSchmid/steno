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

  func testReloadPipelineWaitsForTheInFlightMeeting() async throws {
    let environment = try await TestSupport.environment(seed: false)
    let model = MenuBarViewModel(environment: environment)
    await model.start(mode: .call)
    await model.stop()
    let meetingID = try XCTUnwrap(model.lastStoppedMeetingID)
    let before = environment.pipeline

    // The old pipeline is processing the recording right now; the reload
    // returns only once it is idle, so the meeting is final at that point.
    try await environment.reloadPipeline()
    XCTAssertFalse(before === environment.pipeline)
    let storedOptional = try await environment.store.meeting(id: meetingID)
    let stored = try XCTUnwrap(storedOptional)
    XCTAssertEqual(stored.state, .ready, "the in-flight meeting finished before the swap")

    // The replacement serves the next request.
    try await environment.pipeline.rerunSummary(meetingID: meetingID, templateID: "interview")
    let rerun = try await environment.store.meeting(id: meetingID)
    XCTAssertEqual(rerun?.summary?.templateID, "interview")
  }

  func testUpdateSettingsLoadsMutatesAndSaves() async throws {
    let environment = try await TestSupport.environment(seed: false)
    let returned = try await environment.updateSettings {
      $0.defaultTemplateID = "interview"
      $0.llmContextTokens = 4096
    }
    let stored = try await environment.settings.load()
    XCTAssertEqual(stored, returned, "the return value is what was saved")
    XCTAssertEqual(stored.defaultTemplateID, "interview")
    XCTAssertEqual(stored.llmContextTokens, 4096)
    XCTAssertEqual(stored.defaultRetention, .keepDays(30), "untouched fields survive")
  }

  func testRetentionSweepFailureIsAWarningNotAFatal() async throws {
    let environment = try await TestSupport.environment(seed: false)
    // An expired asset whose master sits in a read-only folder cannot be removed.
    let folder = try TestSupport.temporaryDirectory("steno-locked")
    defer {
      try? FileManager.default.setAttributes([.posixPermissions: 0o755], ofItemAtPath: folder.path)
      try? FileManager.default.removeItem(at: folder)
    }
    var meeting = SampleData.meeting(state: .ready)
    meeting.id = UUID()
    var asset = SampleData.audioAsset()
    asset.id = UUID()
    asset.meetingID = meeting.id
    asset.url = folder.appendingPathComponent("master.caf")
    asset.sidecars16k = [:]
    asset.mixdownURL = nil
    asset.expiresAt = TestSupport.now.addingTimeInterval(-1)
    try Data("x".utf8).write(to: asset.url)
    try FileManager.default.setAttributes([.posixPermissions: 0o555], ofItemAtPath: folder.path)
    try await environment.store.save(meeting, asset: asset)

    let removed = await environment.runRetentionSweep()
    XCTAssertEqual(removed, [])
    XCTAssertEqual(environment.startupWarnings.count, 1)
    XCTAssertTrue(
      environment.startupWarnings.first?.hasPrefix("Retention sweep incomplete:") ?? false,
      environment.startupWarnings.first ?? "")
    let assetAfter = try await environment.store.asset(meetingID: meeting.id)
    XCTAssertNotNil(assetAfter?.expiresAt, "the expiry stays so the next sweep retries")
  }
}
