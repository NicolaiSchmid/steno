import StenoAdapters
import StenoCore
import XCTest

@MainActor
final class MeetingDetailViewModelTests: XCTestCase {
  private var observing: [Task<Void, Never>] = []

  /// A nonisolated override under Swift 6.0: hop to the main actor for the
  /// isolated state.
  override func tearDown() async throws {
    await MainActor.run {
      for task in observing { task.cancel() }
      observing = []
    }
  }

  /// A detail model with its store observations running, as the view's
  /// `.task`s would run them, and its export loaded.
  private func makeModel(_ environment: AppEnvironment) async -> MeetingDetailViewModel {
    let model = MeetingDetailViewModel(meetingID: SampleData.meetingID, environment: environment)
    observing.append(Task { await model.observe() })
    observing.append(Task { await model.observeDeliveries() })
    observing.append(Task { await model.observeSettings() })
    await TestSupport.waitUntil("export loaded") { model.export != nil }
    return model
  }

  /// Points the seeded asset's master at a real file (the seed's URLs under
  /// `/tmp/steno` do not exist), so the files-present cases can render.
  private func placeMaster(
    _ environment: AppEnvironment, retention: AudioRetention, expiresAt: Date?
  ) async throws -> URL {
    let folder = try TestSupport.temporaryDirectory("steno-master")
    try Data([1, 2, 3]).write(to: folder.appendingPathComponent("master.caf"))
    try await updateAsset(environment) {
      $0.url = folder.appendingPathComponent("master.caf")
      $0.retention = retention
      $0.expiresAt = expiresAt
    }
    return folder
  }

  private func updateAsset(_ environment: AppEnvironment, _ mutate: (inout AudioAsset) -> Void)
    async throws
  {
    let assetOptional = try await environment.store.asset(meetingID: SampleData.meetingID)
    var asset = try XCTUnwrap(assetOptional)
    mutate(&asset)
    try await environment.store.save(asset)
  }

  func testExportAndSummaryRender() async throws {
    let environment = try await TestSupport.environment()
    let model = await makeModel(environment)
    XCTAssertEqual(model.meeting?.title, "Produktstrategie 90/10")
    XCTAssertTrue(model.summaryMarkdown.hasPrefix("## "), model.summaryMarkdown)
    XCTAssertTrue(model.summaryMarkdown.contains("**Nicolai**"), "confirmed speaker is bolded")
    XCTAssertEqual(model.unconfirmedSpeakers.map(\.clusterLabel), ["Speaker 2"])
    XCTAssertEqual(model.displayName(forSpeaker: SampleData.speakerOneID), "Nicolai")
    XCTAssertEqual(model.displayName(forSpeaker: nil), "Unknown")
  }

  /// Cancelling the observation (the view going away) ends the updates.
  func testObservationEndsWhenCancelled() async throws {
    let environment = try await TestSupport.environment()
    let model = await makeModel(environment)
    for task in observing {
      task.cancel()
      _ = await task.value
    }
    try await environment.store.update(meetingID: SampleData.meetingID, now: TestSupport.now) {
      $0.title = "Renamed after the view closed"
    }
    await TestSupport.settle()
    try await Task.sleep(for: .milliseconds(100))
    XCTAssertEqual(model.meeting?.title, "Produktstrategie 90/10")
  }

  /// Three edits within the window save once, with the last text, after one
  /// quiet debounce interval on the injected clock (one sleeper re-arms
  /// while edits keep coming).
  func testScratchpadSavesOnceAfterTheDebounce() async throws {
    let clock = ManualClock()
    let environment = try await TestSupport.environment(clock: clock)
    let model = await makeModel(environment)

    func scratchpad() async throws -> String? {
      try await environment.store.meeting(id: SampleData.meetingID)?.scratchpad
    }
    model.saveScratchpad("a")
    model.saveScratchpad("ab")
    model.saveScratchpad("abc")
    _ = await clock.waitForSleepers(1)
    XCTAssertEqual(clock.pendingSleepers, 1, "one debounce sleeper for three edits")
    var stored = try await scratchpad()
    XCTAssertEqual(stored, "Nachfassen wegen Budget.", "nothing saved yet")
    clock.advance(by: MeetingDetailViewModel.scratchpadDebounce)
    await TestSupport.waitUntil("saved once") { (try? await scratchpad()) == "abc" }
    let meeting = try await environment.store.meeting(id: SampleData.meetingID)
    XCTAssertEqual(meeting?.updatedAt, TestSupport.now)
    XCTAssertEqual(clock.pendingSleepers, 0, "no sleeper left once saved")

    // An edit that lands while the sleeper sleeps re-arms the same sleeper
    // instead of starting a second one; the save waits for a quiet window.
    model.saveScratchpad("abcd")
    _ = await clock.waitForSleepers(1)
    model.saveScratchpad("abcde")
    XCTAssertEqual(clock.pendingSleepers, 1, "still one sleeper")
    clock.advance(by: MeetingDetailViewModel.scratchpadDebounce)
    _ = await clock.waitForSleepers(1)
    stored = try await scratchpad()
    XCTAssertEqual(stored, "abc", "the late edit pushed the save out by one interval")
    clock.advance(by: MeetingDetailViewModel.scratchpadDebounce)
    await TestSupport.waitUntil("saved with the last text") {
      (try? await scratchpad()) == "abcde"
    }
    XCTAssertEqual(clock.pendingSleepers, 0)
  }

  func testTemplateChangeRerunsSummaryAndReexportRedelivers() async throws {
    let environment = try await TestSupport.environment()
    let vault = FileManager.default.temporaryDirectory
      .appendingPathComponent("steno-vault-\(UUID().uuidString)", isDirectory: true)
    try FileManager.default.createDirectory(at: vault, withIntermediateDirectories: true)
    defer { try? FileManager.default.removeItem(at: vault) }
    var settings = try await environment.settings.load()
    settings.obsidian = ObsidianSettings(
      vaultPath: vault.path, peopleFolder: nil, includeAudio: false, taskTag: nil)
    try await environment.settings.save(settings)

    let model = await makeModel(environment)
    XCTAssertTrue(model.canRerun)

    await model.setTemplate("interview")
    XCTAssertNil(model.error, model.error ?? "")
    await TestSupport.waitUntil("summary re-run") {
      model.meeting?.summary?.templateID == "interview"
    }
    XCTAssertEqual(model.meeting?.templateID, "interview")
    await TestSupport.waitUntil("delivered after re-run") {
      model.deliveries.first?.status == .delivered
    }

    let firstAttempt = model.deliveries.first?.lastAttemptAt
    await model.reexport()
    XCTAssertNil(model.error, model.error ?? "")
    await TestSupport.waitUntil("re-export delivered") {
      model.deliveries.first?.status == .delivered
    }
    XCTAssertEqual(model.deliveries.count, 1, "one row per destination")
    XCTAssertNotNil(firstAttempt)
    XCTAssertTrue(
      FileManager.default.fileExists(
        atPath: vault.appendingPathComponent("Meetings").path), "the vault received the folder")
  }

  func testKeepAudioTogglesRetention() async throws {
    let environment = try await TestSupport.environment()
    try await environment.updateSettings { $0.defaultRetention = .keepDays(7) }
    let model = await makeModel(environment)
    XCTAssertFalse(model.keepsAudio)

    await model.setKeepAudio(true)
    let assetOptional = try await environment.store.asset(meetingID: SampleData.meetingID)
    var asset = try XCTUnwrap(assetOptional)
    XCTAssertEqual(asset.retention, .keepForever)
    XCTAssertNil(asset.expiresAt)

    let events = await environment.events.subscribe()
    await model.setKeepAudio(false)
    let assetReloaded = try await environment.store.asset(meetingID: SampleData.meetingID)
    asset = try XCTUnwrap(assetReloaded)
    XCTAssertEqual(asset.retention, .keepDays(7))
    XCTAssertEqual(asset.expiresAt, TestSupport.now.addingTimeInterval(7 * 86_400))
    let posted = await TestSupport.drain(events, from: environment.events)
    XCTAssertEqual(posted, [.retentionApplied(meetingID: SampleData.meetingID)], "sweep trigger")

    // A failed export defers the stamp, as the pipeline's retention stage does.
    var failed = SampleData.delivery()
    failed.status = .failed("vault missing")
    try await environment.store.save(failed)
    await model.setKeepAudio(true)
    let deferredEvents = await environment.events.subscribe()
    await model.setKeepAudio(false)
    let deferredOptional = try await environment.store.asset(meetingID: SampleData.meetingID)
    let deferred = try XCTUnwrap(deferredOptional)
    XCTAssertEqual(deferred.retention, .keepDays(7))
    XCTAssertNil(deferred.expiresAt, "audio never expires before the export succeeds")
    let none = await TestSupport.drain(deferredEvents, from: environment.events)
    XCTAssertEqual(none, [], "nothing to sweep")
  }

  func testRecordingStatusSaysWhatTheDefaultDoesNot() async throws {
    let environment = try await TestSupport.environment()
    let model = await makeModel(environment)
    // The seed's asset is stamped and its files do not exist.
    XCTAssertEqual(model.recordingStatus, .deleted)
    XCTAssertEqual(model.recordingStatusText, "Recording deleted")
    XCTAssertFalse(model.showsKeepToggle, "no file, nothing to keep")

    let expiry = TestSupport.now.addingTimeInterval(30 * 86_400)
    let folder = try await placeMaster(environment, retention: .keepDays(30), expiresAt: expiry)
    defer { try? FileManager.default.removeItem(at: folder) }
    await TestSupport.waitUntil("master observed") { model.recordingFilesExist }
    XCTAssertEqual(model.recordingStatus, .deletes(on: expiry))
    let text = try XCTUnwrap(model.recordingStatusText)
    XCTAssertTrue(text.hasPrefix("Deletes on "), text)
    XCTAssertFalse(text.contains("today"))

    try await updateAsset(environment) { $0.expiresAt = TestSupport.now }
    await TestSupport.waitUntil("today") { model.recordingStatusText == "Deletes today" }

    try await updateAsset(environment) {
      $0.retention = .keepForever
      $0.expiresAt = nil
    }
    await TestSupport.waitUntil("kept forever") { model.export?.audio?.retention == .keepForever }
    XCTAssertNil(model.recordingStatus, "the default says it all")
    XCTAssertNil(model.recordingStatusText)

    // A finite rule with no stamp: the meeting's state and exports decide.
    try await updateAsset(environment) { $0.retention = .keepDays(7) }
    await TestSupport.waitUntil("unstamped, ready, nothing to export") {
      model.recordingStatus == .keptWhileProcessing
    }
    var failed = SampleData.delivery()
    failed.status = .failed("vault missing")
    try await environment.store.save(failed)
    await TestSupport.waitUntil("export outstanding") {
      model.recordingStatus == .keptUntilExportSucceeds
    }
    XCTAssertEqual(model.recordingStatusText, "Kept until the export succeeds")

    try await environment.store.setState(
      .failed(reason: "boom"), meetingID: SampleData.meetingID, now: TestSupport.now)
    await TestSupport.waitUntil("processing failed") {
      model.recordingStatus == .keptProcessingFailed
    }
    XCTAssertEqual(model.recordingStatusText, "Kept; processing failed")

    try await environment.store.setState(
      .processing, meetingID: SampleData.meetingID, now: TestSupport.now)
    await TestSupport.waitUntil("processing") { model.recordingStatus == .keptWhileProcessing }
    XCTAssertEqual(model.recordingStatusText, "Kept while processing")
  }

  func testKeepToggleShowsOnlyWhenTheDefaultIsNotForever() async throws {
    let environment = try await TestSupport.environment()
    let model = await makeModel(environment)
    let folder = try await placeMaster(
      environment, retention: .keepDays(30), expiresAt: TestSupport.now.addingTimeInterval(86_400))
    defer { try? FileManager.default.removeItem(at: folder) }
    await TestSupport.waitUntil("master observed") { model.recordingFilesExist }
    XCTAssertEqual(model.defaultRetention, .keepForever)
    XCTAssertFalse(model.showsKeepToggle, "the default already keeps everything")

    try await environment.updateSettings { $0.defaultRetention = .keepDays(7) }
    await TestSupport.waitUntil("toggle appears") { model.showsKeepToggle }
    try await environment.updateSettings { $0.defaultRetention = .keepForever }
    await TestSupport.waitUntil("toggle hides") { !model.showsKeepToggle }
  }

  /// Turning the keep off under "Until processed, then delete" with every
  /// export done would delete the recording at the next sweep, so the
  /// toggle asks first; with an export outstanding nothing would be
  /// deleted and it applies at once.
  func testTurningKeepOffUnderDeleteAfterProcessingAsksFirst() async throws {
    let environment = try await TestSupport.environment()
    try await environment.updateSettings { $0.defaultRetention = .deleteAfterProcessing }
    let model = await makeModel(environment)
    await model.setKeepAudio(true)

    await model.toggleKeepAudio(false)
    XCTAssertTrue(model.confirmsDeleteNow)
    let heldOptional = try await environment.store.asset(meetingID: SampleData.meetingID)
    XCTAssertEqual(try XCTUnwrap(heldOptional).retention, .keepForever, "nothing until confirmed")

    await model.setKeepAudio(false)
    XCTAssertFalse(model.confirmsDeleteNow)
    let stampedOptional = try await environment.store.asset(meetingID: SampleData.meetingID)
    let stamped = try XCTUnwrap(stampedOptional)
    XCTAssertEqual(stamped.retention, .deleteAfterProcessing)
    XCTAssertEqual(stamped.expiresAt, TestSupport.now)

    var failed = SampleData.delivery()
    failed.status = .failed("vault missing")
    try await environment.store.save(failed)
    await model.setKeepAudio(true)
    await model.toggleKeepAudio(false)
    XCTAssertFalse(model.confirmsDeleteNow, "a deferred stamp deletes nothing")
    let deferredOptional = try await environment.store.asset(meetingID: SampleData.meetingID)
    let deferred = try XCTUnwrap(deferredOptional)
    XCTAssertEqual(deferred.retention, .deleteAfterProcessing)
    XCTAssertNil(deferred.expiresAt)
  }

  func testTagsTypedAreNormalised() async throws {
    XCTAssertEqual(MeetingDetailViewModel.tags(from: "Q4, q4 , Strategie"), ["q4", "strategie"])
    XCTAssertEqual(MeetingDetailViewModel.tags(from: " , ,"), [])
    let environment = try await TestSupport.environment()
    let model = await makeModel(environment)
    await model.setTags(text: "Ops, ops, Q4")
    let stored = try await environment.store.meeting(id: SampleData.meetingID)
    XCTAssertEqual(stored?.tags, ["ops", "q4"])
  }
}
