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

  /// A settings store over `store` whose audio folder is `audio` in
  /// `directory`, with `SampleData.pairedDevice()` paired, and that folder.
  static func audioFolder(in directory: URL, for store: MeetingStore) async throws
    -> (SettingsStore, URL)
  {
    let settingsStore = SettingsStore(writer: store.writer)
    var settings = Settings()
    settings.audioFolder = directory.appendingPathComponent("audio", isDirectory: true)
    try await settingsStore.save(settings)
    try await store.save(SampleData.pairedDevice(), tokenHash: Data(repeating: 1, count: 32))
    return (settingsStore, settings.audioFolder)
  }

  /// A one-byte upload in `directory`.
  static func upload(in directory: URL) throws -> URL {
    let upload = directory.appendingPathComponent("upload.bin")
    try Data([1]).write(to: upload)
    return upload
  }

  /// The files in the meeting folders under `audio`.
  static func copies(in audio: URL) throws -> [URL] {
    guard FileManager.default.fileExists(atPath: audio.path) else { return [] }
    return try FileManager.default.contentsOfDirectory(at: audio, includingPropertiesForKeys: nil)
      .flatMap { folder in
        try FileManager.default.contentsOfDirectory(at: folder, includingPropertiesForKeys: nil)
      }
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

  /// A failed admission commit (a full disk) leaves a `.failed` receipt, the
  /// upload for the retry and no meeting, and removes the copy once that
  /// receipt is saved; the retry then completes the same receipt. Rust: the
  /// refused half of
  /// `a_phone_recording_lands_in_the_audio_folder_and_a_refused_one_leaves_no_copy`.
  @Test func aFailedAdmissionCommitLeavesAFailedReceiptTheUploadAndNoMeeting() async throws {
    let directory = try Fixtures.temporaryDirectory()
    defer { try? FileManager.default.removeItem(at: directory) }
    let store = try MeetingStore.inMemory()
    let (settingsStore, audio) = try await Self.audioFolder(in: directory, for: store)
    let enqueued = Enqueued()
    let intake = RecordingIntake(
      store: store, settings: settingsStore,
      enqueue: { meeting, asset in await enqueued.record(meeting, asset) })
    let upload = try Self.upload(in: directory)
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
    #expect(try Self.copies(in: audio).isEmpty, "the copy is removed with the failed admission")

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
  /// meeting that never existed. The copy stays too: without a durable
  /// `.failed` receipt over it, a failed commit whose frames reached the WAL
  /// can be replayed after a crash, and its meeting then needs the copy.
  /// Rust: `a_failed_admission_whose_failed_save_fails_keeps_the_copy_and_no_complete_receipt`.
  @Test func aFailedAdmissionWhoseFailedSaveFailsKeepsTheCopyAndNoCompleteReceipt() async throws {
    let directory = try Fixtures.temporaryDirectory()
    defer { try? FileManager.default.removeItem(at: directory) }
    let store = try MeetingStore.inMemory()
    let (settingsStore, audio) = try await Self.audioFolder(in: directory, for: store)
    var verifying = SampleData.handoverReceipt()
    verifying.state = .verifying
    try await store.save(verifying)
    let intake = RecordingIntake(store: store, settings: settingsStore, enqueue: { _, _ in })
    let upload = try Self.upload(in: directory)
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
    let copies = try Self.copies(in: audio)
    #expect(copies.count == 1, "the copy stays")
    #expect(try copies.first.map { try Data(contentsOf: $0) } == Data([1]))
  }

  /// A receipt of another upload under the same recording id is never
  /// completed: another phone's (the admitting phone was revoked and the
  /// other one announced the id; the admitting phone then paired again), or
  /// the admitting phone's own of other bytes (it announced another file
  /// under the id), made before the intake read the receipt or between its
  /// read and its commit. The intake refuses, and that receipt stays as it
  /// was: completed, it would answer that upload's `complete` with this
  /// meeting, and the phone would delete a recording never admitted; no
  /// ledger row says those bytes were admitted. Rust:
  /// `a_receipt_of_another_upload_is_never_completed`.
  @Test(arguments: [(false, false), (true, false), (false, true), (true, true)])
  func aReceiptOfAnotherUploadIsNeverCompleted(afterTheRead: Bool, otherBytes: Bool)
    async throws
  {
    let directory = try Fixtures.temporaryDirectory()
    defer { try? FileManager.default.removeItem(at: directory) }
    let store = try MeetingStore.inMemory()
    let (settingsStore, _) = try await Self.audioFolder(in: directory, for: store)
    let device = SampleData.pairedDevice()
    let other = PairedDevice(
      id: SampleData.uuid(92), name: "Other phone", pairedAt: device.pairedAt,
      lastSeenAt: nil)
    var theirs = SampleData.handoverReceipt()
    theirs.deviceID = otherBytes ? device.id : other.id
    if otherBytes { theirs.sha256 = Data(repeating: 2, count: 32) }
    theirs.state = .receiving
    // The revoke, the other phone's announce, and this phone pairing again
    // under the same device id; or this phone's announce of another file
    // under the id; on the writer's own queue.
    let takeover: @Sendable () throws -> Void = { [theirs] in
      try store.writer.write { db in
        if !otherBytes {
          _ = try PairedDeviceRow.deleteOne(db, key: device.id.uuidString)
          try PairedDeviceRow(other, tokenHash: Data(repeating: 2, count: 32)).save(db)
        }
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
    let upload = try Self.upload(in: directory)
    let metadata = SampleData.recordingMetadata()

    await #expect(throws: MeetingStoreError.receiptOfAnotherUpload(SampleData.uuid(91))) {
      _ = try await intake.admit(file: upload, metadata: metadata, device: device)
    }
    let receipt = try #require(try await store.handoverReceipt(recordingID: SampleData.uuid(91)))
    #expect(receipt.deviceID == theirs.deviceID, "the other upload's receipt is untouched")
    #expect(receipt.sha256 == theirs.sha256)
    #expect(receipt.state == .receiving)
    #expect(try await store.meetings().isEmpty)
    #expect(
      try await store.admittedMeeting(
        recordingID: metadata.recordingID, byteCount: metadata.byteCount,
        sha256: metadata.sha256) == nil)
    #expect(await enqueued.calls.isEmpty)
    #expect(FileManager.default.fileExists(atPath: upload.path))
  }

  /// Once the rows committed, the recording is admitted: an enqueue that fails
  /// then (the app is shutting down) leaves the meeting `.queued` for the next
  /// launch, and the phone is told `complete`. Rust:
  /// `an_enqueue_that_fails_after_the_commit_still_admits`.
  @Test func anEnqueueThatFailsAfterTheCommitStillAdmits() async throws {
    struct Boom: Error {}
    let directory = try Fixtures.temporaryDirectory()
    defer { try? FileManager.default.removeItem(at: directory) }
    let store = try MeetingStore.inMemory()
    let (settingsStore, _) = try await Self.audioFolder(in: directory, for: store)
    let intake = RecordingIntake(
      store: store, settings: settingsStore, enqueue: { _, _ in throw Boom() })
    let upload = try Self.upload(in: directory)

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
    let (settingsStore, audio) = try await Self.audioFolder(in: directory, for: store)
    let log = SyncLog()
    var intake = RecordingIntake(
      store: store, settings: settingsStore,
      enqueue: { _, _ in log.record("enqueue") })
    intake.syncs = log.syncs()
    let upload = try Self.upload(in: directory)

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
    let (settingsStore, audio) = try await Self.audioFolder(in: directory, for: store)
    let log = SyncLog()
    var intake = RecordingIntake(
      store: store, settings: settingsStore,
      enqueue: { _, _ in log.record("enqueue") })
    intake.syncs = log.syncs(failingFile: true)
    let upload = try Self.upload(in: directory)

    await #expect(throws: (any Error).self) {
      _ = try await intake.admit(
        file: upload, metadata: SampleData.recordingMetadata(), device: SampleData.pairedDevice())
    }
    #expect(!log.all.contains("enqueue"))
    #expect(try await store.handoverReceipt(recordingID: SampleData.uuid(91)) == nil)
    #expect(try Self.copies(in: audio).isEmpty, "the unsynced copy is removed")
    #expect(FileManager.default.fileExists(atPath: upload.path))
  }

  /// The production intake over the real pipeline commits the `.complete`
  /// receipt, the meeting and its asset in one transaction under
  /// `synchronous = FULL`, and leaves the writer at `NORMAL`. Every commit is a
  /// point a crash could stop at, and none holds the receipt without the
  /// meeting. A power loss after the commit cannot be tested; that it ran under
  /// `FULL` can. The enqueue (`ProcessingPipeline.enqueueSaved`) writes
  /// nothing: no other commit saves the meeting with its asset. Rust:
  /// `the_production_intake_commits_its_receipt_and_meeting_durably`.
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
    let meetingWithAsset = commits.filter { $0.tables.isSuperset(of: ["meeting", "audioAsset"]) }
    #expect(meetingWithAsset.count == 1, "the enqueue writes nothing: \(commits)")
    #expect(try await CommitLog.synchronous(of: store) == 1, "the writer is back at NORMAL")
  }

  /// A failed admission commit leaves one commit, its `.failed` receipt,
  /// under `FULL`, while the copy is still there: a failed commit is not
  /// proof that nothing committed, and the durable `.failed` commit is what
  /// writes over a commit a crash could replay. Only then is the copy
  /// removed, and the writer is back at `NORMAL`. Rust:
  /// `a_failed_admission_commit_saves_failed_durably_before_it_removes_the_copy`.
  @Test func aFailedAdmissionCommitSavesFailedDurablyBeforeItRemovesTheCopy() async throws {
    let directory = try Fixtures.temporaryDirectory()
    defer { try? FileManager.default.removeItem(at: directory) }
    let store = try MeetingStore.onDisk(at: directory.appendingPathComponent("steno.sqlite"))
    let (settingsStore, audio) = try await Self.audioFolder(in: directory, for: store)
    let intake = RecordingIntake(store: store, settings: settingsStore, enqueue: { _, _ in })
    let upload = try Self.upload(in: directory)
    try await Self.refuseWrites(store)
    // The copies on the disk as each commit starts.
    let copiesAtCommit = Mutex<[Int]>([])
    let log = try await CommitLog.install(on: store) {
      let count = (try? Self.copies(in: audio).count) ?? -1
      copiesAtCommit.withLock { $0.append(count) }
    }

    await #expect(throws: (any Error).self) {
      _ = try await intake.admit(
        file: upload, metadata: SampleData.recordingMetadata(), device: SampleData.pairedDevice())
    }

    #expect(log.commits == [CommitLog.Commit(synchronous: 2, tables: ["handoverReceipt"])])
    #expect(copiesAtCommit.withLock { $0 } == [1], "the copy is there when the receipt commits")
    #expect(try Self.copies(in: audio).isEmpty, "and removed after")
    #expect(try await CommitLog.synchronous(of: store) == 1)
  }

  /// A `.complete` receipt whose meeting is gone (the separate receipt and
  /// meeting commits of earlier releases, with a crash or a full disk
  /// between them) is not an idempotent return: the intake admits the file
  /// again into a new meeting, since the phone never got its 200 and still
  /// holds the recording. Rust: `a_complete_receipt_whose_meeting_is_gone_is_admitted_again`.
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
