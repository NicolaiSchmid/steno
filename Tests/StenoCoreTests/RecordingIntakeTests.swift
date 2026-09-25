import Foundation
import Testing

@testable import StenoCore

@Suite struct RecordingIntakeTests {
  actor Enqueued {
    var calls: [(Meeting, AudioAsset)] = []
    func record(_ meeting: Meeting, _ asset: AudioAsset) { calls.append((meeting, asset)) }
  }

  @Test func admitPlacesTheFileEnqueuesOnceAndIsIdempotent() async throws {
    let directory = try Fixtures.temporaryDirectory()
    defer { try? FileManager.default.removeItem(at: directory) }
    let store = try MeetingStore.inMemory()
    let settingsStore = SettingsStore(writer: store.writer)
    var settings = Settings()
    settings.audioFolder = directory.appendingPathComponent("audio", isDirectory: true)
    settings.defaultRetention = .keepDays(3)
    settings.defaultTemplateID = "interview"
    try await settingsStore.save(settings)
    try await store.save(SampleData.pairedDevice(), tokenHash: Data(repeating: 1, count: 32))

    let enqueued = Enqueued()
    let intake = RecordingIntake(
      store: store, settings: settingsStore,
      enqueue: { meeting, asset in
        // Like the pipeline's enqueue: the meeting row exists afterwards.
        try await store.save(meeting, asset: asset)
        await enqueued.record(meeting, asset)
      },
      now: { SampleData.createdAt })
    let upload = directory.appendingPathComponent("upload.bin")
    try Data(repeating: 0xAA, count: 4096).write(to: upload)

    let meetingID = try await intake.admit(
      file: upload, metadata: SampleData.recordingMetadata(), device: SampleData.pairedDevice())

    let calls = await enqueued.calls
    #expect(calls.count == 1)
    let (meeting, asset) = try #require(calls.first)
    #expect(meeting.id == meetingID)
    #expect(meeting.source == .phone)
    #expect(meeting.state == .queued)
    #expect(meeting.duration == 6)
    #expect(meeting.startedAt == SampleData.startedAt)
    #expect(meeting.templateID == "interview")
    #expect(meeting.title.hasPrefix("Phone recording 2026-09-24"))
    #expect(meeting.createdAt == SampleData.createdAt)
    #expect(asset.meetingID == meetingID)
    #expect(asset.format == .m4aAAC)
    #expect(asset.lanes == [.mixed])
    #expect(asset.retention == .keepDays(3))
    #expect(asset.expiresAt == nil)
    #expect(
      asset.url
        == settings.audioFolder.appendingPathComponent("\(meetingID.uuidString)/recording.m4a"))
    #expect(FileManager.default.fileExists(atPath: asset.url.path))
    #expect(!FileManager.default.fileExists(atPath: upload.path))

    let receipt = try #require(try await store.handoverReceipt(recordingID: SampleData.uuid(91)))
    #expect(receipt.state == .complete(meetingID: meetingID))
    #expect(receipt.deviceID == SampleData.uuid(90))
    #expect(receipt.byteCount == 4096)

    try Data(repeating: 0xAA, count: 4096).write(to: upload)
    let again = try await intake.admit(
      file: upload, metadata: SampleData.recordingMetadata(), device: SampleData.pairedDevice())
    #expect(again == meetingID)
    #expect(await enqueued.calls.count == 1)
    #expect(FileManager.default.fileExists(atPath: upload.path))
  }

  @Test func admitCompletesAnExistingReceipt() async throws {
    let directory = try Fixtures.temporaryDirectory()
    defer { try? FileManager.default.removeItem(at: directory) }
    let store = try MeetingStore.inMemory()
    let settingsStore = SettingsStore(writer: store.writer)
    var settings = Settings()
    settings.audioFolder = directory
    try await settingsStore.save(settings)
    try await store.save(SampleData.pairedDevice(), tokenHash: Data(repeating: 1, count: 32))
    var receiving = SampleData.handoverReceipt()
    receiving.state = .receiving
    receiving.receivedChunks = [0, 1, 2, 3]
    try await store.save(receiving)

    let intake = RecordingIntake(store: store, settings: settingsStore, enqueue: { _, _ in })
    let upload = directory.appendingPathComponent("upload.bin")
    try Data([1]).write(to: upload)
    let meetingID = try await intake.admit(
      file: upload, metadata: SampleData.recordingMetadata(), device: SampleData.pairedDevice())
    let receipt = try #require(try await store.handoverReceipt(recordingID: SampleData.uuid(91)))
    #expect(receipt.state == .complete(meetingID: meetingID))
    #expect(receipt.receivedChunks == [0, 1, 2, 3])
    #expect(receipt.createdAt == SampleData.createdAt)
  }

  @Test func titleUsesTheGivenTimeZone() {
    let title = RecordingIntake.title(
      for: SampleData.startedAt, timeZone: TimeZone(identifier: "Europe/Berlin")!)
    #expect(title == "Phone recording 2026-09-24 11:00")
    #expect(AudioFormat.wav16kInt16.fileExtension == "wav")
  }

  @Test func aFailedEnqueueLeavesAFailedReceiptTheUploadAndNoMeeting() async throws {
    struct Boom: Error {}
    let directory = try Fixtures.temporaryDirectory()
    defer { try? FileManager.default.removeItem(at: directory) }
    let store = try MeetingStore.inMemory()
    let settingsStore = SettingsStore(writer: store.writer)
    var settings = Settings()
    settings.audioFolder = directory.appendingPathComponent("audio", isDirectory: true)
    try await settingsStore.save(settings)
    try await store.save(SampleData.pairedDevice(), tokenHash: Data(repeating: 1, count: 32))
    let intake = RecordingIntake(
      store: store, settings: settingsStore, enqueue: { _, _ in throw Boom() })
    let upload = directory.appendingPathComponent("upload.bin")
    try Data([1]).write(to: upload)

    await #expect(throws: Boom.self) {
      _ = try await intake.admit(
        file: upload, metadata: SampleData.recordingMetadata(), device: SampleData.pairedDevice())
    }
    let receipt = try #require(try await store.handoverReceipt(recordingID: SampleData.uuid(91)))
    #expect(receipt.state.meetingID == nil, "never .complete for a meeting that does not exist")
    guard case .failed(let reason) = receipt.state else {
      Issue.record("expected .failed, got \(receipt.state)")
      return
    }
    #expect(reason.contains("Boom"))
    #expect(try await store.meetings().isEmpty)
    #expect(FileManager.default.fileExists(atPath: upload.path), "the retry finds its file")
    let copies = try FileManager.default.contentsOfDirectory(atPath: settings.audioFolder.path)
      .flatMap { folder in
        try FileManager.default.contentsOfDirectory(
          atPath: settings.audioFolder.appendingPathComponent(folder).path)
      }
    #expect(copies.isEmpty, "the copy is removed with the failed admission")

    // The retry succeeds and completes the same receipt.
    let retrying = RecordingIntake(store: store, settings: settingsStore, enqueue: { _, _ in })
    let meetingID = try await retrying.admit(
      file: upload, metadata: SampleData.recordingMetadata(), device: SampleData.pairedDevice())
    #expect(
      try await store.handoverReceipt(recordingID: SampleData.uuid(91))?.state
        == .complete(meetingID: meetingID))
    #expect(!FileManager.default.fileExists(atPath: upload.path))
  }

  @Test func aCompleteReceiptWhoseMeetingIsGoneIsAdmittedAgain() async throws {
    let directory = try Fixtures.temporaryDirectory()
    defer { try? FileManager.default.removeItem(at: directory) }
    let store = try MeetingStore.inMemory()
    let settingsStore = SettingsStore(writer: store.writer)
    var settings = Settings()
    settings.audioFolder = directory
    try await settingsStore.save(settings)
    try await store.save(SampleData.pairedDevice(), tokenHash: Data(repeating: 1, count: 32))
    // A .complete receipt whose meeting row does not exist.
    try await store.save(SampleData.handoverReceipt())
    let enqueued = Enqueued()
    let intake = RecordingIntake(
      store: store, settings: settingsStore,
      enqueue: { meeting, asset in await enqueued.record(meeting, asset) })
    let upload = directory.appendingPathComponent("upload.bin")
    try Data([1]).write(to: upload)

    let meetingID = try await intake.admit(
      file: upload, metadata: SampleData.recordingMetadata(), device: SampleData.pairedDevice())
    #expect(meetingID != SampleData.meetingID)
    #expect(await enqueued.calls.count == 1)
    #expect(
      try await store.handoverReceipt(recordingID: SampleData.uuid(91))?.state
        == .complete(meetingID: meetingID))
  }
}
