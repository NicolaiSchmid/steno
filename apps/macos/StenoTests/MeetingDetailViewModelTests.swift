import StenoAdapters
import StenoCore
import XCTest

@MainActor
final class MeetingDetailViewModelTests: XCTestCase {
  func testExportAndSummaryRender() async throws {
    let environment = try await TestSupport.environment()
    let model = MeetingDetailViewModel(meetingID: SampleData.meetingID, environment: environment)
    await TestSupport.waitUntil("export loaded") { model.export != nil }
    XCTAssertEqual(model.meeting?.title, "Produktstrategie 90/10")
    XCTAssertTrue(model.summaryMarkdown.hasPrefix("## "), model.summaryMarkdown)
    XCTAssertTrue(model.summaryMarkdown.contains("**Nicolai**"), "confirmed speaker is bolded")
    XCTAssertEqual(model.unconfirmedSpeakers.map(\.clusterLabel), ["Speaker 2"])
    XCTAssertEqual(model.displayName(forSpeaker: SampleData.speakerOneID), "Nicolai")
    XCTAssertEqual(model.displayName(forSpeaker: nil), "Unknown")
  }

  func testScratchpadSavesOnceAfterTheDebounce() async throws {
    let clock = ManualClock()
    let environment = try await TestSupport.environment(clock: clock)
    let model = MeetingDetailViewModel(meetingID: SampleData.meetingID, environment: environment)
    await TestSupport.waitUntil("export loaded") { model.export != nil }

    model.saveScratchpad("a")
    model.saveScratchpad("ab")
    model.saveScratchpad("abc")
    XCTAssertEqual(model.scratchpadSaves, 0)
    _ = await clock.waitForSleepers(1)
    clock.advance(by: MeetingDetailViewModel.scratchpadDebounce)
    await TestSupport.waitUntil("one save") { model.scratchpadSaves == 1 }
    XCTAssertEqual(model.scratchpadSaves, 1)
    let stored = try await environment.store.meeting(id: SampleData.meetingID)
    XCTAssertEqual(stored?.scratchpad, "abc")
    XCTAssertEqual(stored?.updatedAt, TestSupport.now)
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

    let model = MeetingDetailViewModel(meetingID: SampleData.meetingID, environment: environment)
    await TestSupport.waitUntil("export loaded") { model.export != nil }
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
    let model = MeetingDetailViewModel(meetingID: SampleData.meetingID, environment: environment)
    await TestSupport.waitUntil("export loaded") { model.export != nil }
    XCTAssertFalse(model.keepsAudio)

    await model.setKeepAudio(true, defaultRetention: .keepDays(30))
    var asset = try XCTUnwrap(try await environment.store.asset(meetingID: SampleData.meetingID))
    XCTAssertEqual(asset.retention, .keepForever)
    XCTAssertNil(asset.expiresAt)

    await model.setKeepAudio(false, defaultRetention: .keepDays(7))
    asset = try XCTUnwrap(try await environment.store.asset(meetingID: SampleData.meetingID))
    XCTAssertEqual(asset.retention, .keepDays(7))
    XCTAssertEqual(asset.expiresAt, TestSupport.now.addingTimeInterval(7 * 86_400))
  }

  func testTagsAndTitleEdit() async throws {
    let environment = try await TestSupport.environment()
    let model = MeetingDetailViewModel(meetingID: SampleData.meetingID, environment: environment)
    await TestSupport.waitUntil("export loaded") { model.export != nil }
    await model.setTags(["a", "b"])
    await model.setTitle("  New title ")
    await model.setTitle("   ")
    let stored = try await environment.store.meeting(id: SampleData.meetingID)
    XCTAssertEqual(stored?.tags, ["a", "b"])
    XCTAssertEqual(stored?.title, "New title", "blank titles are ignored")
  }
}
