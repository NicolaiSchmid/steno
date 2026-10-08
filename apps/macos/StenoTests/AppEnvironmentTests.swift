import StenoCore
import StenoHandover
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

  /// The handover listener is built only over a store whose commits are on the
  /// disk: a checkpoint that fails (here one another connection blocks) throws
  /// before the identity is read, which `live` turns into a startup warning
  /// with the handover off; once the checkpoint succeeds the listener is built.
  /// Rust: `a_store_that_cannot_sync_keeps_the_handover_off`.
  func testAStoreThatCannotSyncKeepsTheHandoverOff() async throws {
    let directory = try TestSupport.temporaryDirectory("steno-sync")
    defer { try? FileManager.default.removeItem(at: directory) }
    let (store, other) = try MeetingStore.checkpointBlocked(
      at: directory.appendingPathComponent("steno.sqlite"))
    let configuration = HandoverConfiguration(
      serviceName: "Test Mac", advertise: false,
      inboxDirectory: directory.appendingPathComponent("inbox", isDirectory: true))
    var identityReads = 0
    let identity = {
      identityReads += 1
      return try TestSupport.testIdentity()
    }

    do {
      _ = try await AppEnvironment.makeHandover(
        store: store, intake: FakeHandoverIntake(), configuration: configuration,
        identity: identity)
      XCTFail("a store that cannot sync gets no listener")
    } catch let error as StoreNotSynced {
      XCTAssertTrue(
        "\(error)".hasPrefix("the database could not be synced to the disk: "), "\(error)")
    }
    XCTAssertEqual(identityReads, 0, "the identity is not read")

    try other.release()
    let handover = try await AppEnvironment.makeHandover(
      store: store, intake: FakeHandoverIntake(), configuration: configuration,
      identity: identity)
    XCTAssertEqual(identityReads, 1)
    XCTAssertEqual(handover.state, .stopped)
  }

  /// `preview()` grants every permission; `preview(permissions:)` takes the
  /// checker it is given (`-steno-show-onboarding` passes one with every
  /// kind unknown, so the window opens on page 1).
  func testPreviewTakesThePermissionsItIsGiven() async throws {
    let granted = try await AppEnvironment.preview()
    for kind in PermissionKind.allCases {
      let state = await granted.permissions.state(of: kind)
      XCTAssertEqual(state, .granted, "\(kind.rawValue) by default")
    }
    let unknown = try await AppEnvironment.preview(permissions: FakePermissions())
    for kind in PermissionKind.allCases {
      let state = await unknown.permissions.state(of: kind)
      XCTAssertEqual(state, .unknown, "\(kind.rawValue) as given")
    }
  }

  /// The `-steno-rich-seed` set: the fixture stays newest, every filter row
  /// has a count, the meetings span three days, and the processing meeting
  /// has a master on disk so the pipeline resumes it instead of failing it.
  func testRichSeedSpansThreeDaysAndEveryState() async throws {
    let environment = try await TestSupport.environment(fixtures: .rich)
    let meetings = try await environment.store.meetings().sorted { $0.startedAt > $1.startedAt }
    XCTAssertEqual(meetings.count, 5)
    XCTAssertEqual(meetings.first?.id, SampleData.meetingID, "the fixture stays newest")

    func count(_ filter: MeetingListViewModel.StateFilter) -> Int {
      meetings.filter { filter.matches($0) }.count
    }
    XCTAssertEqual(count(.all), 5)
    XCTAssertEqual(count(.processing), 1)
    XCTAssertEqual(count(.ready), 3)
    XCTAssertEqual(count(.failed), 1)
    XCTAssertEqual(
      Set(meetings.map(\.titleOrigin)), [.default, .calendar, .summary],
      "every origin the display title distinguishes, except a user rename")

    var utc = Calendar(identifier: .gregorian)
    utc.timeZone = TimeZone(identifier: "UTC")!
    let groups = MeetingListViewModel.DayGroup.group(meetings, calendar: utc)
    XCTAssertEqual(groups.count, 3)
    XCTAssertEqual(groups.map(\.meetings.count), [1, 2, 2])
    XCTAssertEqual(groups.first?.meetings.first?.id, SampleData.meetingID)

    let processing = try XCTUnwrap(meetings.first { $0.state == .processing })
    let assetOptional = try await environment.store.asset(meetingID: processing.id)
    let asset = try XCTUnwrap(assetOptional)
    XCTAssertEqual(asset.meetingID, processing.id)
    XCTAssertTrue(FileManager.default.fileExists(atPath: asset.url.path), asset.url.path)
    let ready = meetings.filter { $0.state == .ready && $0.id != SampleData.meetingID }
    XCTAssertEqual(ready.map { $0.summary?.sections.first?.bullets.count }, [2, 2])
  }

  /// The seeded fixture is a calendar meeting, so the list entry shows its
  /// calendar title and not a derived weekday; the smoke tests find it by
  /// that title in the entry.
  func testSampleMeetingKeepsItsCalendarTitle() {
    let meeting = SampleData.meeting()
    XCTAssertEqual(meeting.titleOrigin, .calendar)
    XCTAssertEqual(
      meeting.displayTitle(now: TestSupport.now, calendar: .current, locale: .current),
      "Produktstrategie 90/10")
  }

  /// The rich seed's processing meeting resumes at launch as the product
  /// resumes one left over by a crash: the synthetic master is found under
  /// the remapped asset, the fakes finish it, the failed fixture is left
  /// alone and nothing is warned about.
  func testRichSeedProcessingMeetingResumesInsteadOfFailing() async throws {
    let environment = try await TestSupport.environment(fixtures: .rich)
    let resumed = await environment.resumeUnfinishedProcessing()
    XCTAssertEqual(resumed, [SampleData.uuid(102)])
    await environment.pipeline.waitUntilIdle()
    let processed = try await environment.store.meeting(id: SampleData.uuid(102))
    XCTAssertEqual(processed?.state, .ready, String(describing: processed?.state))
    let failed = try await environment.store.meeting(id: SampleData.uuid(104))
    XCTAssertEqual(failed?.state.isFailed, true, "the failed fixture is left alone")
    XCTAssertEqual(environment.startupWarnings, [])
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
        guard firstBuild.take() else { return FakeSpeechEngine() }
        var held = FakeSpeechEngine()
        held.onTranscribe = { await gate.wait() }
        return held
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
    XCTAssertEqual(stored.defaultRetention, .keepForever, "untouched fields survive")
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
