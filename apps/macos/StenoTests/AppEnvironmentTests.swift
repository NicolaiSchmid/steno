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

  /// Save in the LLM or Speech settings while a meeting is processing: the
  /// swap is immediate, the next recording lands on the new pipeline, and
  /// the meeting in flight still finishes on the old one.
  func testReloadPipelineSwapsFirstAndLetsTheOldOneFinish() async throws {
    let gate = Gate()
    let firstBuild = OnceFlag()
    let environment = try await TestSupport.environment(
      seed: false,
      makeSpeechEngine: { () -> any SpeechEngine in
        firstBuild.take() ? GatedSpeechEngine(gate: gate) : FakeSpeechEngine()
      })
    let recorder = RecordingController(environment: environment)
    await recorder.start(mode: .call)
    await recorder.stop()
    let firstOptional = try await environment.store.meetings().first?.id
    let first = try XCTUnwrap(firstOptional)
    let retired = environment.pipeline
    await TestSupport.waitUntil("the first meeting is processing on the old pipeline") {
      (try? await environment.store.meeting(id: first))?.state == .processing
    }

    var reloaded = false
    let reload = Task {
      try await environment.reloadPipeline()
      reloaded = true
    }
    await TestSupport.waitUntil("the reload returned while the old pipeline was busy") { reloaded }
    try await reload.value
    XCTAssertFalse(retired === environment.pipeline)

    await recorder.start(mode: .inPerson)
    await recorder.stop()
    let secondOptional = try await environment.store.meetings().first { $0.id != first }?.id
    let second = try XCTUnwrap(secondOptional)
    await environment.pipeline.waitUntilIdle()
    let secondStored = try await environment.store.meeting(id: second)
    XCTAssertEqual(secondStored?.state, .ready, "enqueued after the swap: the new pipeline ran it")
    let firstStored = try await environment.store.meeting(id: first)
    XCTAssertEqual(firstStored?.state, .processing, "the old pipeline is still held at the gate")

    await gate.open()
    await retired.waitUntilIdle()
    let firstFinished = try await environment.store.meeting(id: first)
    XCTAssertEqual(firstFinished?.state, .ready, "the retired pipeline stayed alive to finish it")
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
