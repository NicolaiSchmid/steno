import StenoAdapters
import StenoCore
import XCTest

@MainActor
final class MeetingDetailViewModelTests: XCTestCase {
  private var observing: [Task<Void, Never>] = []

  override func tearDown() {
    for task in observing { task.cancel() }
    observing = []
  }

  /// A detail model with its store observations running, as the view's
  /// `.task`s would run them, and its export loaded.
  private func makeModel(_ environment: AppEnvironment) async -> MeetingDetailViewModel {
    let model = MeetingDetailViewModel(meetingID: SampleData.meetingID, environment: environment)
    observing.append(Task { await model.observe() })
    observing.append(Task { await model.observeDeliveries() })
    await TestSupport.waitUntil("export loaded") { model.export != nil }
    return model
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
    let model = await makeModel(environment)
    XCTAssertFalse(model.keepsAudio)

    await model.setKeepAudio(true)
    let assetOptional = try await environment.store.asset(meetingID: SampleData.meetingID)
    var asset = try XCTUnwrap(assetOptional)
    XCTAssertEqual(asset.retention, .keepForever)
    XCTAssertNil(asset.expiresAt)

    try await environment.updateSettings { $0.defaultRetention = .keepDays(7) }
    await model.setKeepAudio(false)
    let assetReloaded = try await environment.store.asset(meetingID: SampleData.meetingID)
    asset = try XCTUnwrap(assetReloaded)
    XCTAssertEqual(asset.retention, .keepDays(7))
    XCTAssertEqual(asset.expiresAt, TestSupport.now.addingTimeInterval(7 * 86_400))
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
