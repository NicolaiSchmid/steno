import Foundation
import GRDB
import StenoCore
import Testing

@testable import StenoAdapters

/// A destination that records the `previous` receipt it was handed.
struct RecordingDestination: Destination, Sendable {
  let id: String
  let failure: (any Error & Sendable)?
  let previousReceipts = CallLog<DeliveryReceipt?>()

  init(id: String, failure: (any Error & Sendable)? = nil) {
    self.id = id
    self.failure = failure
  }

  func validate() async throws {}

  func deliver(_ meeting: MeetingExport, previous: DeliveryReceipt?) async throws
    -> DeliveryReceipt
  {
    await previousReceipts.record(previous)
    if let failure { throw failure }
    return DeliveryReceipt(
      root: "/vault-\(id)", folder: "Meetings/\(meeting.meeting.title)",
      files: [
        DeliveredFile(
          relativePath: "meeting.json", ownership: .owned,
          sha256: Data(repeating: UInt8(previous == nil ? 1 : 2), count: 32))
      ],
      rendererVersion: 7)
  }
}

@Suite struct DeliveryCoordinatorTests {
  static let now = FixtureMeeting.updatedAt

  static func store() async throws -> (MeetingStore, SettingsStore) {
    let store = try MeetingStore.inMemory()
    let settings = SettingsStore(writer: store.writer)
    try await settings.save(Settings())
    let export = FixtureMeeting.export()
    try await store.save(export.meeting)
    for person in export.persons { try await store.save(person) }
    return (store, settings)
  }

  @Test func oneRowPerDestinationFailuresDoNotBlockTheNext() async throws {
    let (store, settings) = try await Self.store()
    let failing = RecordingDestination(id: "a-fails", failure: ObsidianError.audioUnavailable)
    let working = RecordingDestination(id: "b-works")
    let coordinator = DeliveryCoordinator(
      store: store, settings: settings, destinations: { _ in [failing, working] },
      now: { Self.now })

    let results = await coordinator.deliverAll(meetingID: FixtureMeeting.meetingID)

    #expect(results.map(\.destinationID) == ["a-fails", "b-works"])
    #expect(results[0].status == .failed(ObsidianError.audioUnavailable.description))
    #expect(results[0].receipt == nil)
    #expect(results[1].status == .delivered)
    #expect(results[1].receipt?.root == "/vault-b-works")
    #expect(results.allSatisfy { $0.lastAttemptAt == Self.now })
    #expect(
      results.map(\.id) == [
        Delivery.id(meetingID: FixtureMeeting.meetingID, destinationID: "a-fails"),
        Delivery.id(meetingID: FixtureMeeting.meetingID, destinationID: "b-works"),
      ])
    let stored = try await store.deliveries(meetingID: FixtureMeeting.meetingID)
    #expect(stored == results, "the rows are what deliverAll returned")
    #expect(await failing.previousReceipts.entries == [nil])
    #expect(await working.previousReceipts.entries == [nil])
  }

  @Test func aSecondRunPassesTheStoredReceiptBackAsPrevious() async throws {
    let (store, settings) = try await Self.store()
    let working = RecordingDestination(id: "b-works")
    let coordinator = DeliveryCoordinator(
      store: store, settings: settings, destinations: { _ in [working] }, now: { Self.now })
    let first = await coordinator.deliverAll(meetingID: FixtureMeeting.meetingID)
    let later = Self.now.addingTimeInterval(60)
    let again = DeliveryCoordinator(
      store: store, settings: settings, destinations: { _ in [working] }, now: { later })
    let second = await again.deliverAll(meetingID: FixtureMeeting.meetingID)

    #expect(await working.previousReceipts.entries == [nil, first[0].receipt])
    #expect(second[0].receipt?.files.first?.sha256 == Data(repeating: 2, count: 32))
    #expect(second[0].lastAttemptAt == later)
    #expect(try await store.deliveries(meetingID: FixtureMeeting.meetingID).count == 1)
  }

  @Test func aFailedRunKeepsThePreviousReceiptForTheNextAttempt() async throws {
    let (store, settings) = try await Self.store()
    let working = RecordingDestination(id: "x")
    let first = await DeliveryCoordinator(
      store: store, settings: settings, destinations: { _ in [working] }, now: { Self.now }
    ).deliverAll(meetingID: FixtureMeeting.meetingID)
    let broken = RecordingDestination(id: "x", failure: ObsidianError.audioUnavailable)
    let second = await DeliveryCoordinator(
      store: store, settings: settings, destinations: { _ in [broken] }, now: { Self.now }
    ).deliverAll(meetingID: FixtureMeeting.meetingID)
    #expect(second[0].status.kind == .failed)
    #expect(second[0].receipt == first[0].receipt, "the folder stays pinned across a failure")
    let third = await DeliveryCoordinator(
      store: store, settings: settings, destinations: { _ in [working] }, now: { Self.now }
    ).deliverAll(meetingID: FixtureMeeting.meetingID)
    #expect(await working.previousReceipts.entries == [nil, first[0].receipt])
    #expect(third[0].status == .delivered)
  }

  @Test func noObsidianSettingsMeansNoDestinationsAndNoRows() async throws {
    let (store, settings) = try await Self.store()
    #expect(destinations(for: Settings()).isEmpty)
    let coordinator = DeliveryCoordinator(store: store, settings: settings, now: { Self.now })
    #expect(await coordinator.deliverAll(meetingID: FixtureMeeting.meetingID).isEmpty)
    #expect(try await store.deliveries(meetingID: FixtureMeeting.meetingID).isEmpty)

    var configured = Settings()
    configured.obsidian = ObsidianSettings(vaultPath: "/tmp/vault", peopleFolder: "People")
    let built = destinations(for: configured)
    #expect(built.map(\.id) == [ObsidianFolderDestination.destinationID])
    #expect((built.first as? ObsidianFolderDestination)?.settings == configured.obsidian)
  }

  @Test func settingsThatDoNotLoadFailEveryStoredRowInsteadOfSilence() async throws {
    let (store, settings) = try await Self.store()
    let working = RecordingDestination(id: "b-works")
    let first = await DeliveryCoordinator(
      store: store, settings: settings, destinations: { _ in [working] }, now: { Self.now }
    ).deliverAll(meetingID: FixtureMeeting.meetingID)
    #expect(first[0].status == .delivered)

    // A settings row that is not JSON makes `SettingsStore.load` throw.
    try await store.writer.write { db in
      try db.execute(
        sql: "INSERT OR REPLACE INTO setting (key, value) VALUES ('obsidian', '{not json')")
    }
    await #expect(throws: (any Error).self) { try await settings.load() }

    let later = Self.now.addingTimeInterval(60)
    let results = await DeliveryCoordinator(
      store: store, settings: settings, destinations: { _ in [working] }, now: { later }
    ).deliverAll(meetingID: FixtureMeeting.meetingID)

    #expect(results.count == 1)
    let row = try #require(results.first, "one failed row per stored delivery, not silence")
    #expect(row.destinationID == "b-works")
    #expect(row.status.kind == .failed)
    if case .failed(let reason) = row.status {
      #expect(reason.hasPrefix("settings failed: "))
    }
    #expect(row.lastAttemptAt == later)
    #expect(row.receipt == first[0].receipt, "the receipt is kept for the next attempt")
    #expect(try await store.deliveries(meetingID: FixtureMeeting.meetingID) == results)
    #expect(await working.previousReceipts.count == 1, "no destination ran")
  }

  @Test func anUnknownMeetingYieldsFailedRowsNotAThrow() async throws {
    let (store, settings) = try await Self.store()
    let working = RecordingDestination(id: "b-works")
    let coordinator = DeliveryCoordinator(
      store: store, settings: settings, destinations: { _ in [working] }, now: { Self.now })
    let results = await coordinator.deliverAll(meetingID: SampleData.uuid(404))
    #expect(results.count == 1)
    #expect(results[0].status.kind == .failed)
    if case .failed(let reason) = results[0].status {
      #expect(reason.hasPrefix("export failed: meeting "))
    }
    #expect(await working.previousReceipts.count == 0)
    #expect(
      try await store.deliveries(meetingID: SampleData.uuid(404)).isEmpty,
      "no meeting row, so no delivery row can hang off it; the result still reports the failure")
  }

  @Test func theStoredReceiptRoundTripsIntoTheRealDestination() async throws {
    let (store, settings) = try await Self.store()
    let directory = try Fixtures.temporaryDirectory("coordinator-vault")
    defer { try? FileManager.default.removeItem(at: directory) }
    var configured = try await settings.load()
    configured.obsidian = ObsidianSettings(vaultPath: directory.path, peopleFolder: "People")
    try await settings.save(configured)
    let coordinator = DeliveryCoordinator(
      store: store, settings: settings,
      destinations: { settings in
        settings.obsidian.map {
          [ObsidianFolderDestination(settings: $0, timeZone: FixtureMeeting.berlin)]
        } ?? []
      }, now: { Self.now })

    let first = await coordinator.deliverAll(meetingID: FixtureMeeting.meetingID)
    #expect(first.count == 1)
    #expect(first[0].status == .delivered)
    let receipt = try #require(first[0].receipt)
    #expect(receipt.folder == FixtureMeeting.folder)
    #expect(receipt.files.count == 5, "no audio, no persons in the store's export")
    let stored = try #require(
      try await store.deliveries(meetingID: FixtureMeeting.meetingID).first?.receipt)
    #expect(stored == receipt, "the receipt survives the JSON column")

    // The user drops a note in; the second run through the stored receipt
    // changes nothing Steno wrote and leaves the note alone.
    let notes = directory.appendingPathComponent("\(receipt.folder)/notes.md")
    try Data("mine\n".utf8).write(to: notes)
    let second = await coordinator.deliverAll(meetingID: FixtureMeeting.meetingID)
    #expect(second[0].status == .delivered)
    #expect(second[0].receipt == receipt, "byte-identical files, identical hashes")
    #expect(try Data(contentsOf: notes) == Data("mine\n".utf8))
    #expect(try await store.deliveries(meetingID: FixtureMeeting.meetingID).count == 1)
  }
}
