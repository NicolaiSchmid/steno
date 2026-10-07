import Foundation
import Synchronization
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
      enqueue: { meeting, asset in await enqueued.record(meeting, asset) },
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
    #expect(try await store.meeting(id: meetingID) != nil, "the intake saved the meeting")

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

  /// Makes every meeting insert fail, as a full disk or a busy store would
  /// fail the admission's commit; with `failedReceiptsToo`, the save of a
  /// `.failed` receipt fails as well.
  static func refuseWrites(_ store: MeetingStore, failedReceiptsToo: Bool = false) async throws {
    var sql = """
      CREATE TRIGGER refuseMeetings BEFORE INSERT ON meeting
      BEGIN SELECT RAISE(ABORT, 'disk full'); END;
      """
    if failedReceiptsToo {
      for event in ["INSERT", "UPDATE"] {
        sql += """
          CREATE TRIGGER refuseFailed\(event) BEFORE \(event) ON handoverReceipt
          WHEN NEW.state = 'failed' BEGIN SELECT RAISE(ABORT, 'disk full'); END;
          """
      }
    }
    try await store.writer.write { [sql] db in try db.execute(sql: sql) }
  }

  @Test func aFailedAdmissionCommitLeavesAFailedReceiptTheUploadAndNoMeeting() async throws {
    let directory = try Fixtures.temporaryDirectory()
    defer { try? FileManager.default.removeItem(at: directory) }
    let store = try MeetingStore.inMemory()
    let settingsStore = SettingsStore(writer: store.writer)
    var settings = Settings()
    settings.audioFolder = directory.appendingPathComponent("audio", isDirectory: true)
    try await settingsStore.save(settings)
    try await store.save(SampleData.pairedDevice(), tokenHash: Data(repeating: 1, count: 32))
    let enqueued = Enqueued()
    let intake = RecordingIntake(
      store: store, settings: settingsStore,
      enqueue: { meeting, asset in await enqueued.record(meeting, asset) })
    let upload = directory.appendingPathComponent("upload.bin")
    try Data([1]).write(to: upload)
    try await Self.refuseWrites(store)

    await #expect(throws: (any Error).self) {
      _ = try await intake.admit(
        file: upload, metadata: SampleData.recordingMetadata(), device: SampleData.pairedDevice())
    }
    let receipt = try #require(try await store.handoverReceipt(recordingID: SampleData.uuid(91)))
    #expect(receipt.state.meetingID == nil, "never .complete for a meeting that does not exist")
    guard case .failed(let reason) = receipt.state else {
      Issue.record("expected .failed, got \(receipt.state)")
      return
    }
    #expect(reason.contains("disk full"))
    #expect(try await store.meetings().isEmpty)
    #expect(await enqueued.calls.isEmpty)
    #expect(FileManager.default.fileExists(atPath: upload.path), "the retry finds its file")
    let copies = try FileManager.default.contentsOfDirectory(atPath: settings.audioFolder.path)
      .flatMap { folder in
        try FileManager.default.contentsOfDirectory(
          atPath: settings.audioFolder.appendingPathComponent(folder).path)
      }
    #expect(copies.isEmpty, "the copy is removed with the failed admission")

    // The retry succeeds and completes the same receipt.
    try await store.writer.write { db in try db.execute(sql: "DROP TRIGGER refuseMeetings") }
    let meetingID = try await intake.admit(
      file: upload, metadata: SampleData.recordingMetadata(), device: SampleData.pairedDevice())
    #expect(
      try await store.handoverReceipt(recordingID: SampleData.uuid(91))?.state
        == .complete(meetingID: meetingID))
    #expect(try await store.meeting(id: meetingID) != nil)
    #expect(!FileManager.default.fileExists(atPath: upload.path))
  }

  /// A failed admission commit leaves no `.complete` receipt behind, also
  /// when the save of the `.failed` one fails too (a full disk): the receipt
  /// stays as the listener left it and the upload stays for the retry.
  /// Two separate commits would leave a `.complete` receipt without its
  /// meeting, and the phone's retried `complete` would answer 200 for a
  /// meeting that never existed.
  @Test func aFailedAdmissionLeavesNoCompleteReceiptEvenWhenTheFailedSaveFails() async throws {
    let directory = try Fixtures.temporaryDirectory()
    defer { try? FileManager.default.removeItem(at: directory) }
    let store = try MeetingStore.inMemory()
    let settingsStore = SettingsStore(writer: store.writer)
    var settings = Settings()
    settings.audioFolder = directory.appendingPathComponent("audio", isDirectory: true)
    try await settingsStore.save(settings)
    try await store.save(SampleData.pairedDevice(), tokenHash: Data(repeating: 1, count: 32))
    var verifying = SampleData.handoverReceipt()
    verifying.state = .verifying
    try await store.save(verifying)
    let intake = RecordingIntake(store: store, settings: settingsStore, enqueue: { _, _ in })
    let upload = directory.appendingPathComponent("upload.bin")
    try Data([1]).write(to: upload)
    try await Self.refuseWrites(store, failedReceiptsToo: true)

    await #expect(throws: (any Error).self) {
      _ = try await intake.admit(
        file: upload, metadata: SampleData.recordingMetadata(), device: SampleData.pairedDevice())
    }
    #expect(
      try await store.handoverReceipt(recordingID: SampleData.uuid(91))?.state == .verifying,
      "never .complete without its meeting")
    #expect(try await store.meetings().isEmpty)
    #expect(FileManager.default.fileExists(atPath: upload.path), "the upload stays for the retry")
  }

  /// A receipt of another phone under the same recording id is never
  /// completed. The admitting phone was revoked and the other one announced
  /// the id, before the intake read the receipt or between its read and its
  /// commit (where the admitting phone also paired again). The intake
  /// refuses, and the other phone's receipt stays as it was: completed, it
  /// would answer that phone's `complete` with this meeting, and that phone
  /// would delete a recording never admitted.
  @Test(arguments: [false, true])
  func aReceiptOfAnotherPhoneIsNeverCompleted(afterTheRead: Bool) async throws {
    let directory = try Fixtures.temporaryDirectory()
    defer { try? FileManager.default.removeItem(at: directory) }
    let store = try MeetingStore.inMemory()
    let settingsStore = SettingsStore(writer: store.writer)
    var settings = Settings()
    settings.audioFolder = directory.appendingPathComponent("audio", isDirectory: true)
    try await settingsStore.save(settings)
    let device = SampleData.pairedDevice()
    let other = PairedDevice(
      id: SampleData.uuid(92), name: "Other phone", pairedAt: device.pairedAt,
      lastSeenAt: nil)
    try await store.save(device, tokenHash: Data(repeating: 1, count: 32))
    var theirs = SampleData.handoverReceipt()
    theirs.deviceID = other.id
    theirs.state = .receiving
    // The revoke, the other phone's announce, and this phone pairing again
    // under the same device id, on the writer's own queue.
    let takeover: @Sendable () throws -> Void = { [theirs] in
      try store.writer.write { db in
        _ = try PairedDeviceRow.deleteOne(db, key: device.id.uuidString)
        try PairedDeviceRow(other, tokenHash: Data(repeating: 2, count: 32)).save(db)
        try HandoverReceiptRow(theirs).save(db)
        try PairedDeviceRow(device, tokenHash: Data(repeating: 1, count: 32)).save(db)
      }
    }
    let enqueued = Enqueued()
    var intake = RecordingIntake(
      store: store, settings: settingsStore,
      enqueue: { meeting, asset in await enqueued.record(meeting, asset) })
    if afterTheRead {
      var mine = SampleData.handoverReceipt()
      mine.state = .verifying
      try await store.save(mine)
      // The copy's sync runs after the receipt read and before the commit.
      let ran = Mutex(false)
      intake.syncs = RecordingIntake.Syncs(
        file: { _ in
          let first = ran.withLock { done in
            defer { done = true }
            return !done
          }
          if first { try takeover() }
        },
        directory: { _ in })
    } else {
      try takeover()
    }
    let upload = directory.appendingPathComponent("upload.bin")
    try Data([1]).write(to: upload)

    await #expect(throws: MeetingStoreError.receiptOfAnotherDevice(SampleData.uuid(91))) {
      _ = try await intake.admit(
        file: upload, metadata: SampleData.recordingMetadata(), device: device)
    }
    let receipt = try #require(try await store.handoverReceipt(recordingID: SampleData.uuid(91)))
    #expect(receipt.deviceID == other.id, "the other phone's receipt is untouched")
    #expect(receipt.state == .receiving)
    #expect(try await store.meetings().isEmpty)
    #expect(await enqueued.calls.isEmpty)
    #expect(FileManager.default.fileExists(atPath: upload.path))
  }

  /// Once the rows committed, the recording is admitted: an enqueue that
  /// fails then (the app is shutting down) leaves the meeting `.queued` for
  /// the next launch, and the phone is told `complete`.
  @Test func anEnqueueThatFailsAfterTheCommitStillAdmits() async throws {
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

    let meetingID = try await intake.admit(
      file: upload, metadata: SampleData.recordingMetadata(), device: SampleData.pairedDevice())

    #expect(try await store.meeting(id: meetingID)?.state == .queued)
    #expect(
      try await store.handoverReceipt(recordingID: SampleData.uuid(91))?.state
        == .complete(meetingID: meetingID))
    #expect(!FileManager.default.fileExists(atPath: upload.path))
  }

  /// The syncs `admit` makes and the enqueue, in order, as
  /// `file <path>`, `folder <path>` and `enqueue`.
  final class SyncLog: Sendable {
    private let events = Mutex<[String]>([])
    var all: [String] { events.withLock { $0 } }
    func record(_ event: String) { events.withLock { $0.append(event) } }

    func syncs(failingFile: Bool = false) -> RecordingIntake.Syncs {
      struct SyncFailed: Error {}
      return RecordingIntake.Syncs(
        file: { url in
          self.record("file \(url.standardizedFileURL.path)")
          if failingFile { throw SyncFailed() }
        },
        directory: { url in self.record("folder \(url.standardizedFileURL.path)") })
    }
  }

  /// Before the receipt and the meeting are written, the parent of every
  /// folder `admit` created is synced (outermost first), then the copy,
  /// then its meeting folder, as Rust's `create_dir_all_durably` and
  /// `copy_durably` do: `copyItem` alone syncs nothing.
  @Test func theCopyAndEveryFolderItCreatedAreSyncedBeforeTheReceipt() async throws {
    let directory = try Fixtures.temporaryDirectory().standardizedFileURL
    defer { try? FileManager.default.removeItem(at: directory) }
    let store = try MeetingStore.inMemory()
    let settingsStore = SettingsStore(writer: store.writer)
    var settings = Settings()
    let audio = directory.appendingPathComponent("audio", isDirectory: true)
    settings.audioFolder = audio
    try await settingsStore.save(settings)
    try await store.save(SampleData.pairedDevice(), tokenHash: Data(repeating: 1, count: 32))
    let log = SyncLog()
    var intake = RecordingIntake(
      store: store, settings: settingsStore,
      enqueue: { _, _ in log.record("enqueue") })
    intake.syncs = log.syncs()
    let upload = directory.appendingPathComponent("upload.bin")
    try Data([1]).write(to: upload)

    let meetingID = try await intake.admit(
      file: upload, metadata: SampleData.recordingMetadata(), device: SampleData.pairedDevice())

    let meetingFolder = audio.appendingPathComponent(meetingID.uuidString).path
    #expect(
      log.all == [
        "folder \(directory.path)",
        "folder \(audio.path)",
        "file \(meetingFolder)/recording.m4a",
        "folder \(meetingFolder)",
        "enqueue",
      ])
  }

  /// A copy whose sync fails is removed and nothing is written: the upload
  /// stays for the retry.
  @Test func aCopyWhoseSyncFailsIsRemovedAndNoReceiptIsWritten() async throws {
    let directory = try Fixtures.temporaryDirectory()
    defer { try? FileManager.default.removeItem(at: directory) }
    let store = try MeetingStore.inMemory()
    let settingsStore = SettingsStore(writer: store.writer)
    var settings = Settings()
    settings.audioFolder = directory.appendingPathComponent("audio", isDirectory: true)
    try await settingsStore.save(settings)
    try await store.save(SampleData.pairedDevice(), tokenHash: Data(repeating: 1, count: 32))
    let log = SyncLog()
    var intake = RecordingIntake(
      store: store, settings: settingsStore,
      enqueue: { _, _ in log.record("enqueue") })
    intake.syncs = log.syncs(failingFile: true)
    let upload = directory.appendingPathComponent("upload.bin")
    try Data([1]).write(to: upload)

    await #expect(throws: (any Error).self) {
      _ = try await intake.admit(
        file: upload, metadata: SampleData.recordingMetadata(), device: SampleData.pairedDevice())
    }
    #expect(!log.all.contains("enqueue"))
    #expect(try await store.handoverReceipt(recordingID: SampleData.uuid(91)) == nil)
    let copies = try FileManager.default.contentsOfDirectory(atPath: settings.audioFolder.path)
      .flatMap { folder in
        try FileManager.default.contentsOfDirectory(
          atPath: settings.audioFolder.appendingPathComponent(folder).path)
      }
    #expect(copies.isEmpty, "the unsynced copy is removed")
    #expect(FileManager.default.fileExists(atPath: upload.path))
  }

  /// The production intake over the real pipeline commits the `.complete`
  /// receipt, the meeting and its asset in one transaction under
  /// `synchronous = FULL`, and leaves the writer at `NORMAL`. Every commit
  /// is a point a crash could stop at, and none holds the receipt without
  /// the meeting. A power loss after the commit cannot be tested; that it
  /// ran under `FULL` can.
  @Test func theProductionIntakeCommitsItsReceiptAndMeetingDurably() async throws {
    let directory = try Fixtures.temporaryDirectory()
    defer { try? FileManager.default.removeItem(at: directory) }
    let store = try MeetingStore.onDisk(at: directory.appendingPathComponent("steno.sqlite"))
    let harness = try await PipelineHarness(sharedStore: store)
    defer { harness.cleanUp() }
    try await store.save(SampleData.pairedDevice(), tokenHash: Data(repeating: 1, count: 32))
    let upload = harness.directory.appendingPathComponent("upload.wav")
    try FileManager.default.copyItem(
      at: Fixtures.url("audio/conversation-two-lane-6s.wav"), to: upload)
    var metadata = SampleData.recordingMetadata()
    metadata.format = .wav16kInt16
    // The app's wiring (`AppEnvironment.makeIntake`).
    let pipeline = harness.pipeline
    let intake = RecordingIntake(
      store: store, settings: harness.settingsStore, currentPipeline: { pipeline },
      now: { PipelineHarness.now })
    #expect(try await CommitLog.synchronous(of: store) == 1)
    let log = try await CommitLog.install(on: store)

    _ = try await intake.admit(file: upload, metadata: metadata, device: SampleData.pairedDevice())
    await harness.pipeline.waitUntilIdle()

    let commits = log.commits
    #expect(
      commits.first
        == CommitLog.Commit(synchronous: 2, tables: ["handoverReceipt", "meeting", "audioAsset"]),
      "the first commit holds the receipt, the meeting and the asset, under FULL")
    let receiptWithoutMeeting = commits.filter {
      $0.tables.contains("handoverReceipt") && !$0.tables.contains("meeting")
    }
    #expect(receiptWithoutMeeting.isEmpty, "no commit holds the receipt without the meeting")
    #expect(try await CommitLog.synchronous(of: store) == 1, "the writer is back at NORMAL")
  }

  /// A refused admission commits nothing under `FULL`: its durable
  /// transaction rolls back, the `.failed` receipt commits as usual, and
  /// the writer is back at `NORMAL`.
  @Test func aRefusedAdmissionLeavesTheWriterAtNormal() async throws {
    let directory = try Fixtures.temporaryDirectory()
    defer { try? FileManager.default.removeItem(at: directory) }
    let store = try MeetingStore.onDisk(at: directory.appendingPathComponent("steno.sqlite"))
    let settingsStore = SettingsStore(writer: store.writer)
    var settings = Settings()
    settings.audioFolder = directory.appendingPathComponent("audio", isDirectory: true)
    try await settingsStore.save(settings)
    try await store.save(SampleData.pairedDevice(), tokenHash: Data(repeating: 1, count: 32))
    let intake = RecordingIntake(store: store, settings: settingsStore, enqueue: { _, _ in })
    let upload = directory.appendingPathComponent("upload.bin")
    try Data([1]).write(to: upload)
    try await Self.refuseWrites(store)
    let log = try await CommitLog.install(on: store)

    await #expect(throws: (any Error).self) {
      _ = try await intake.admit(
        file: upload, metadata: SampleData.recordingMetadata(), device: SampleData.pairedDevice())
    }

    let receipts = log.commits.filter { $0.tables.contains("handoverReceipt") }
    #expect(receipts.map(\.synchronous) == [1])
    #expect(try await CommitLog.synchronous(of: store) == 1)
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
