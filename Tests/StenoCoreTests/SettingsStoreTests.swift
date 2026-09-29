import Foundation
import GRDB
import Testing

@testable import StenoCore

@Suite struct SettingsStoreTests {
  @Test func defaultsAreTheProgramsDefaults() {
    let settings = Settings()
    #expect(settings.defaultTemplateID == "default")
    #expect(settings.defaultRetention == .keepForever, "a new install keeps every recording")
    #expect(settings.speakerMatchThreshold == 0.60)
    #expect(settings.llmContextTokens == 32_000)
    #expect(settings.llmProvider == .endpoint, "a new install summarises over an endpoint")
    #expect(settings.codexModel == nil)
    #expect(settings.codexContextTokens == 128_000)
    #expect(settings.codexConfirmedAt == nil, "nothing reads the Codex sign-in until confirmed")
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

  /// The row an install wrote under the old default, byte for byte: no
  /// migration touches it and the new default does not apply.
  @Test func aStoredThirtyDayRowFromAnOldInstallWins() async throws {
    let store = try MeetingStore.inMemory()
    let settings = SettingsStore(writer: store.writer)
    try await store.writer.write { db in
      try SettingRow(key: "defaultRetention", value: #"{"keepDays":30}"#).insert(db)
    }
    #expect(try await settings.load().defaultRetention == .keepDays(30))
  }

  /// Each of the three rules survives save and load; an install that stored
  /// `{"keepDays":30}` under the old default keeps it.
  @Test func everyRetentionRuleRoundTrips() async throws {
    let store = try MeetingStore.inMemory()
    let settings = SettingsStore(writer: store.writer)
    for (rule, encoded) in [
      (AudioRetention.keepForever, #""keepForever""#),
      (.keepDays(30), #"{"keepDays":30}"#),
      (.deleteAfterProcessing, #""deleteAfterProcessing""#),
    ] {
      var current = Settings()
      current.defaultRetention = rule
      try await settings.save(current)
      #expect(try await settings.load().defaultRetention == rule)
      let row = try await store.writer.read { db in
        try SettingRow.filter(SettingRow.Columns.key == "defaultRetention").fetchOne(db)
      }
      #expect(row?.value == encoded, "\(rule)")
    }
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
    #expect(settings.defaultRetention == .keepForever)
    #expect(settings.speakerMatchThreshold == 0.60)
    #expect(settings.llmContextTokens == 32_000)
    #expect(settings.obsidian == nil)
  }

  /// An install from before the provider existed has none of the Codex
  /// rows: it loads as the endpoint provider with its own rows intact.
  /// Switching to Codex stores the four new rows (the confirmation as an
  /// ISO 8601 instant) beside the endpoint's, and "Stop using ChatGPT"
  /// removes only the confirmation row.
  @Test func codexRowsRoundTripAndAnOldInstallLoadsTheirDefaults() async throws {
    let store = try MeetingStore.inMemory()
    let settings = SettingsStore(writer: store.writer)
    try await store.writer.write { db in
      try SettingRow(key: "llmModel", value: "\"local\"").insert(db)
      try SettingRow(key: "llmContextTokens", value: "16000").insert(db)
    }
    let old = try await settings.load()
    #expect(old.llmProvider == .endpoint)
    #expect(old.codexModel == nil)
    #expect(old.codexContextTokens == 128_000)
    #expect(old.codexConfirmedAt == nil)
    #expect(old.llmModel == "local")
    #expect(old.llmContextTokens == 16_000)

    var confirmed = old
    confirmed.llmProvider = .codex
    confirmed.codexModel = "gpt-5.6-terra"
    confirmed.codexContextTokens = 272_000
    confirmed.codexConfirmedAt = Date(timeIntervalSince1970: 1_790_000_000.25)
    try await settings.save(confirmed)
    #expect(try await settings.load() == confirmed)
    let rows = try await store.writer.read { db in
      Dictionary(uniqueKeysWithValues: try SettingRow.fetchAll(db).map { ($0.key, $0.value) })
    }
    #expect(rows["llmProvider"] == "\"codex\"")
    #expect(rows["codexModel"] == "\"gpt-5.6-terra\"")
    #expect(rows["codexContextTokens"] == "272000")
    #expect(rows["codexConfirmedAt"] == "\"2026-09-21T14:13:20.250Z\"")
    #expect(rows["llmModel"] == "\"local\"", "the endpoint's rows survive the switch")
    #expect(rows["llmContextTokens"] == "16000")

    var stopped = confirmed
    stopped.codexConfirmedAt = nil
    try await settings.save(stopped)
    #expect(try await settings.load() == stopped)
    let keys = try await store.writer.read { db in try SettingRow.fetchAll(db).map(\.key) }
    #expect(!keys.contains("codexConfirmedAt"))
    #expect(keys.contains("codexModel"), "the model is kept for the next confirmation")
    #expect(keys.contains("llmProvider"))
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
