import Foundation
import GRDB
import Testing

@testable import StenoCore

@Suite struct SettingsStoreTests {
  @Test func defaultsAreTheProgramsDefaults() {
    let settings = Settings()
    #expect(settings.defaultTemplateID == "default")
    #expect(settings.defaultRetention == .keepDays(30))
    #expect(settings.speakerMatchThreshold == 0.60)
    #expect(settings.llmContextTokens == 32_000)
    #expect(settings.obsidian == nil)
    #expect(settings.launchAtLogin)
    #expect(settings.meetingDetectionEnabled)
    #expect(settings.speechEngineID == "parakeet-v3")
    #expect(settings.audioFolder.lastPathComponent == "Audio")
  }

  @Test func emptyTableLoadsDefaults() async throws {
    let store = try MeetingStore.inMemory()
    let settings = SettingsStore(writer: store.writer)
    #expect(try await settings.load() == Settings())
  }

  @Test func saveThenLoadRoundTrips() async throws {
    let store = try MeetingStore.inMemory()
    let settings = SettingsStore(writer: store.writer)
    try await settings.save(SampleData.settings())
    #expect(try await settings.load() == SampleData.settings())
    let rows = try await store.writer.read { db in
      try SettingRow.order(SettingRow.Columns.key).fetchAll(db)
    }
    #expect(rows.first { $0.key == "llmContextTokens" }?.value == "16000")
    #expect(rows.first { $0.key == "defaultRetention" }?.value == #"{"keepDays":7}"#)
    #expect(rows.first { $0.key == "launchAtLogin" }?.value == "false")

    var cleared = SampleData.settings()
    cleared.obsidian = nil
    cleared.llmBaseURL = nil
    try await settings.save(cleared)
    #expect(try await settings.load() == cleared)
    let keys = try await store.writer.read { db in try SettingRow.fetchAll(db).map(\.key) }
    #expect(!keys.contains("obsidian"))
  }

  @Test func unknownAndMissingRowsAreIgnored() async throws {
    let store = try MeetingStore.inMemory()
    try await store.writer.write { db in
      try SettingRow(key: "llmModel", value: "\"gpt\"").insert(db)
      try SettingRow(key: "futureKnob", value: "42").insert(db)
    }
    let settings = try await SettingsStore(writer: store.writer).load()
    #expect(settings.llmModel == "gpt")
    #expect(settings.launchAtLogin == true)
    #expect(settings.defaultTemplateID == "default")
    #expect(settings.defaultRetention == .keepDays(30))
    #expect(settings.speakerMatchThreshold == 0.60)
    #expect(settings.llmContextTokens == 32_000)
    #expect(settings.obsidian == nil)
  }

  @Test func observeYieldsAgainAfterASave() async throws {
    let store = try MeetingStore.inMemory()
    let settings = SettingsStore(writer: store.writer)
    var iterator = settings.observe().makeAsyncIterator()
    #expect(try await iterator.next() == Settings())
    try await settings.save(SampleData.settings())
    #expect(try await iterator.next() == SampleData.settings())
  }
}
