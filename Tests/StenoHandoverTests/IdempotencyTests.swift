import Foundation
import StenoCore
import Testing

@testable import StenoHandover

/// M5: completing twice returns the same meeting id, the receipt stream
/// reaches `.complete`, and a start sweeps orphaned inbox files.
@Suite struct IdempotencyTests {
  static let chunkSize = 1024 * 1024

  @Test func completingTwiceReturnsTheSameMeetingID() async throws {
    let meetingID = UUID(uuidString: "1DEA0000-0000-4000-8000-000000000001")!
    let intake = FakeHandoverIntake(meetingID: meetingID)
    let test = try await TestService.start(chunkSize: Self.chunkSize, intake: intake)
    defer { Task { await test.stop() } }
    let phone = try await Phone.pair(test)
    let bytes = Phone.seededBytes(count: Self.chunkSize + 500, seed: 11)
    let metadata = phone.metadata(for: bytes)

    try await phone.uploadAll(metadata, bytes)
    let first = try await phone.complete(metadata.recordingID)
    let second = try await phone.complete(metadata.recordingID)
    #expect(first.status == 200)
    #expect(second.status == 200)
    #expect(try first.json(Wire.CompleteResponse.self).meetingID == meetingID)
    #expect(try second.json(Wire.CompleteResponse.self).meetingID == meetingID)
    #expect(await intake.admissions.count == 1, "the second complete does not admit again")
  }

  @Test func realIntakeMakesTheSecondCompleteReturnTheStoredMeeting() async throws {
    // The real RecordingIntake, as M6 wires it: complete enqueues a `.queued`
    // `.phone` meeting; a repeat returns that meeting id without a second one.
    let directory = try Fixtures.temporaryDirectory("handover-intake")
    defer { try? FileManager.default.removeItem(at: directory) }
    let store = try MeetingStore.inMemory()
    let settingsStore = SettingsStore(writer: store.writer)
    var settings = Settings()
    settings.audioFolder = directory.appendingPathComponent("audio", isDirectory: true)
    try await settingsStore.save(settings)
    let enqueued = CallLog<UUID>()
    let intake = RecordingIntake(
      store: store, settings: settingsStore,
      enqueue: { meeting, asset in
        // What ProcessingPipeline.enqueue does: persist the meeting and asset,
        // so RecordingIntake's idempotency check finds the meeting next time.
        try await store.save(meeting, asset: asset)
        await enqueued.record(meeting.id)
      })

    let inbox = directory.appendingPathComponent("inbox", isDirectory: true)
    let service = HandoverService(
      configuration: HandoverConfiguration(
        serviceName: "Test Mac", advertise: false, chunkSize: Self.chunkSize, inboxDirectory: inbox),
      store: store, intake: intake, identity: try TestIdentity.load(), clock: ManualClock())
    try await service.start()
    defer { Task { await service.stop() } }
    let test = TestService(
      service: service, store: store, intake: FakeHandoverIntake(), clock: ManualClock(),
      directory: directory, now: Date())
    let phone = try await Phone.pair(test)
    let bytes = Phone.seededBytes(count: Self.chunkSize + 42, seed: 5)
    let metadata = phone.metadata(for: bytes)

    try await phone.uploadAll(metadata, bytes)
    let first = try await phone.complete(metadata.recordingID)
    let meetingID = try first.json(Wire.CompleteResponse.self).meetingID
    let second = try await phone.complete(metadata.recordingID)
    #expect(try second.json(Wire.CompleteResponse.self).meetingID == meetingID)
    #expect(await enqueued.count == 1)

    let meeting = try #require(try await store.meeting(id: meetingID))
    #expect(meeting.source == .phone)
    #expect(meeting.state == .queued)
  }

  @Test func receiptsStreamReachesComplete() async throws {
    let meetingID = UUID(uuidString: "1DEA0000-0000-4000-8000-000000000002")!
    let test = try await TestService.start(
      chunkSize: Self.chunkSize, intake: FakeHandoverIntake(meetingID: meetingID))
    defer { Task { await test.stop() } }
    let receipts = await test.service.receipts
    let phone = try await Phone.pair(test)
    let bytes = Phone.seededBytes(count: 2 * Self.chunkSize, seed: 21)
    let metadata = phone.metadata(for: bytes)

    let collector = Task { () -> HandoverReceipt? in
      for await batch in receipts {
        if let receipt = batch.first(where: { $0.recordingID == metadata.recordingID }),
          receipt.state.kind == .complete
        {
          return receipt
        }
      }
      return nil
    }
    try await phone.uploadAll(metadata, bytes)
    #expect(try await phone.complete(metadata.recordingID).status == 200)

    let receipt = await collector.value
    #expect(receipt?.state == .complete(meetingID: meetingID))
    #expect(receipt?.receivedChunks == [0, 1])
  }

  @Test func startSweepsOrphanedInboxFiles() async throws {
    let directory = try Fixtures.temporaryDirectory("handover-sweep")
    defer { try? FileManager.default.removeItem(at: directory) }
    let inboxURL = directory.appendingPathComponent("inbox", isDirectory: true)
    let inbox = Inbox(directory: inboxURL)
    try inbox.prepare()
    // An announced-but-abandoned recording with no receipt in the store.
    let orphan = UUID()
    let metadata = RecordingMetadata(
      recordingID: orphan, startedAt: Date(timeIntervalSince1970: 1_789_000_000),
      durationSeconds: 10, byteCount: 1000, sha256: Data(repeating: 0, count: 32),
      chunkSize: Self.chunkSize, format: .m4aAAC, deviceName: "Ghost")
    try inbox.begin(metadata)
    #expect(inbox.hasPartial(orphan))

    let service = HandoverService(
      configuration: HandoverConfiguration(
        serviceName: "Test Mac", advertise: false, chunkSize: Self.chunkSize,
        inboxDirectory: inboxURL),
      store: try MeetingStore.inMemory(), intake: FakeHandoverIntake(),
      identity: try TestIdentity.load(), clock: ManualClock())
    try await service.start()
    defer { Task { await service.stop() } }

    #expect(!inbox.hasPartial(orphan), "the orphan partial is swept")
    #expect(inbox.loadMetadata(orphan) == nil)
  }
}
