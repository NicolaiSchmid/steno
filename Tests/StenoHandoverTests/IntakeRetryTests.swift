import Foundation
import GRDB
import StenoCore
import Synchronization
import Testing

@testable import StenoHandover

/// A `HandoverIntake` that throws for the first `failures` admissions and
/// then returns `meetingID`: the pipeline refusing a file once, as the plan's
/// "intake failure leaves a retryable state" needs. `delay` holds each
/// admission open, the way the real intake's copy of a large file does;
/// `admitOnce` refuses every admission after the first successful one, the
/// way the real intake fails when a second copy races the first one's
/// removal of the source.
final class ScriptedIntake: HandoverIntake, Sendable {
  /// Carries the file path, as a `CocoaError` from the real intake's copy
  /// would; the Mac must not echo it to the phone or into the receipt.
  struct Refused: Error, CustomStringConvertible {
    var file: URL
    var description: String { "the pipeline refused \(file.path)" }
  }

  let meetingID: UUID
  let delay: Duration
  let admitOnce: Bool
  /// The file of every admission, in order.
  let admissions = CallLog<URL>()
  private let failuresLeft: Mutex<Int>
  private let admitted = Mutex(false)

  init(meetingID: UUID, failures: Int, delay: Duration = .zero, admitOnce: Bool = false) {
    self.meetingID = meetingID
    self.delay = delay
    self.admitOnce = admitOnce
    self.failuresLeft = Mutex(failures)
  }

  func admit(file: URL, metadata: RecordingMetadata, device: PairedDevice) async throws -> UUID {
    await admissions.record(file)
    if delay > .zero { try await Task.sleep(for: delay) }
    let refuse = failuresLeft.withLock { left -> Bool in
      guard left > 0 else { return false }
      left -= 1
      return true
    }
    if refuse { throw Refused(file: file) }
    let repeated = admitted.withLock { done -> Bool in
      defer { done = true }
      return done
    }
    if admitOnce, repeated { throw Refused(file: file) }
    return meetingID
  }
}

/// The recovery paths after the Mac verified a file: the intake failing once,
/// the phone retrying by `complete` or by announcing again, and a Mac restart
/// resuming from the stored receipt while the sweep removes only what no
/// receipt accounts for; after a restart and a revoke, another phone's first
/// announce of the same recording id starts from no file, and after a
/// restart a failed receipt read keeps the files.
@Suite struct IntakeRetryTests {
  static let chunkSize = 256 * 1024
  static let meetingID = UUID(uuidString: "1ABE1000-0000-4000-8000-0000000000AD")!

  @Test func intakeFailureIs500AndTheNextCompleteAdmitsTheSameVerifiedFile() async throws {
    let intake = ScriptedIntake(meetingID: Self.meetingID, failures: 1)
    try await TestService.run(chunkSize: Self.chunkSize, customIntake: intake) { test in
      let phone = try await Phone.pair(test.service)
      let bytes = Phone.seededBytes(count: 2 * Self.chunkSize, seed: 77)
      let metadata = phone.metadata(for: bytes, chunkSize: Self.chunkSize)
      try await phone.uploadAll(metadata, bytes)
      let inbox = test.service.engine.inbox

      let failed = try await phone.complete(metadata.recordingID)
      #expect(failed.status == 500)
      let problem = try failed.json(Wire.Problem.self).error
      #expect(problem.contains("intake"))
      #expect(
        !problem.contains(inbox.directory.path),
        "the intake's error names the file; the phone must not learn the inbox path")
      #expect(inbox.hasVerified(metadata.recordingID, format: .m4aAAC), "the verified file waits")
      #expect(!inbox.hasPartial(metadata.recordingID))
      #expect(inbox.loadMetadata(metadata.recordingID) == metadata, "the sidecar waits with it")
      let receipt = try #require(
        try await test.store.handoverReceipt(recordingID: metadata.recordingID))
      #expect(
        receipt.state == .failed(HandoverEngine.intakeRefused), "no path in the receipt either")
      #expect(receipt.receivedChunks == [0, 1], "the chunk set survives the failure")
      #expect(
        try await phone.status(metadata.recordingID).json(Wire.RecordingStatus.self)
          == Wire.RecordingStatus(state: .failed, receivedChunks: [0, 1]))

      // The phone repeats the call; no chunk travels again.
      let retried = try await phone.complete(metadata.recordingID)
      #expect(retried.status == 200)
      #expect(try retried.json(Wire.CompleteResponse.self).meetingID == Self.meetingID)
      let files = await intake.admissions.entries
      #expect(files.count == 2)
      #expect(files.first == files.last, "the same verified file both times")
      #expect(try Data(contentsOf: try #require(files.last)) == bytes)
      #expect(
        try await test.store.handoverReceipt(recordingID: metadata.recordingID)?.state
          == .complete(meetingID: Self.meetingID))
      #expect(inbox.loadMetadata(metadata.recordingID) == nil, "the sidecar goes with the success")

      // And once more, for the phone that lost the 200: same id, no new admission.
      let again = try await phone.complete(metadata.recordingID)
      #expect(try again.json(Wire.CompleteResponse.self).meetingID == Self.meetingID)
      #expect(await intake.admissions.count == 2)
    }
  }

  @Test func reAnnounceAfterAnIntakeFailureKeepsTheChunkSet() async throws {
    // The phone's executor answers a 5xx at complete with a backoff and then
    // starts the recording over at announce (`upload-executor.test.ts`).
    let intake = ScriptedIntake(meetingID: Self.meetingID, failures: 1)
    try await TestService.run(chunkSize: Self.chunkSize, customIntake: intake) { test in
      let phone = try await Phone.pair(test.service)
      let bytes = Phone.seededBytes(count: 2 * Self.chunkSize + 99, seed: 78)
      let metadata = phone.metadata(for: bytes, chunkSize: Self.chunkSize)
      let chunks = Phone.chunks(of: bytes, size: Self.chunkSize)
      try await phone.uploadAll(metadata, bytes)
      #expect(try await phone.complete(metadata.recordingID).status == 500)
      let inbox = test.service.engine.inbox

      let announced = try await phone.announce(metadata)
      #expect(announced.status == 200)
      #expect(
        try announced.json(Wire.RecordingStatus.self)
          == Wire.RecordingStatus(state: .receiving, receivedChunks: [0, 1, 2]),
        "every chunk is still there; the phone goes straight to complete")
      #expect(!inbox.hasPartial(metadata.recordingID), "no fresh partial beside the verified file")
      #expect(inbox.hasVerified(metadata.recordingID, format: .m4aAAC))

      // A chunk the phone sends anyway is a harmless duplicate.
      #expect(try await phone.upload(metadata.recordingID, chunk: 1, chunks[1]).status == 204)
      let done = try await phone.complete(metadata.recordingID)
      #expect(done.status == 200)
      #expect(try done.json(Wire.CompleteResponse.self).meetingID == Self.meetingID)
      let files = await intake.admissions.entries
      #expect(files.count == 2)
      #expect(try Data(contentsOf: try #require(files.last)) == bytes)
    }
  }

  @Test func aRestartedMacResumesFromTheStoredReceiptAndSweepsOnlyOrphans() async throws {
    try await TestService.run(chunkSize: Self.chunkSize) { first in
      let phone = try await Phone.pair(first.service)
      let bytes = Phone.seededBytes(count: 3 * Self.chunkSize, seed: 79)
      let metadata = phone.metadata(for: bytes, chunkSize: Self.chunkSize)
      let chunks = Phone.chunks(of: bytes, size: Self.chunkSize)
      #expect(try await phone.announce(metadata).status == 201)
      #expect(try await phone.upload(metadata.recordingID, chunk: 0, chunks[0]).status == 204)
      let inbox = first.service.engine.inbox

      // Beside the live upload: an orphan nobody announced to the store, the
      // leftover of a completed handover, and a verified file whose intake
      // failed (its `.failed` receipt keeps it for the retry).
      func seed(_ id: UUID) throws -> RecordingMetadata {
        let metadata = RecordingMetadata(
          recordingID: id, startedAt: first.now, durationSeconds: 1, byteCount: 10,
          sha256: Data(repeating: 0, count: 32), chunkSize: Self.chunkSize, format: .m4aAAC,
          deviceName: "Ghost")
        try inbox.begin(metadata)
        return metadata
      }
      func receipt(_ id: UUID, _ state: HandoverState) -> HandoverReceipt {
        HandoverReceipt(
          recordingID: id, deviceID: phone.deviceID, state: state, byteCount: 10,
          sha256: Data(repeating: 0, count: 32), chunkSize: Self.chunkSize, receivedChunks: [0],
          createdAt: first.now, updatedAt: first.now)
      }
      let orphan = UUID()
      _ = try seed(orphan)
      let completed = UUID()
      _ = try seed(completed)
      _ = try inbox.promote(completed, format: .m4aAAC)
      let completedMeeting = UUID()
      try await first.store.saveAdmittedMeeting(completedMeeting)
      try await first.store.save(receipt(completed, .complete(meetingID: completedMeeting)))
      let refused = UUID()
      _ = try seed(refused)
      _ = try inbox.promote(refused, format: .m4aAAC)
      try await first.store.save(receipt(refused, .failed("admit: refused")))
      await first.service.stop()

      // The Mac comes back over the same store and inbox.
      let intake = FakeHandoverIntake(meetingID: Self.meetingID)
      let now = first.now
      let second = HandoverService(
        configuration: first.service.configuration, store: first.store,
        intake: first.moving(intake),
        identity: try TestIdentity.load(), now: { now })
      try await second.start()
      defer { Task { await second.stop() } }

      #expect(inbox.hasPartial(metadata.recordingID), "the live upload is spared")
      #expect(inbox.loadMetadata(metadata.recordingID) == metadata)
      #expect(inbox.hasVerified(refused, format: .m4aAAC), "the retryable verified file is spared")
      #expect(!inbox.hasPartial(orphan) && inbox.loadMetadata(orphan) == nil, "the orphan is gone")
      #expect(
        !inbox.hasVerified(completed, format: .m4aAAC) && inbox.loadMetadata(completed) == nil,
        "the completed leftover is gone")

      // The phone resumes with the token it holds; the receipt comes from the store.
      let resumed = Phone(
        client: try LoopbackClient.forService(second), token: phone.token,
        deviceID: phone.deviceID, deviceName: phone.deviceName)
      let status = try await resumed.status(metadata.recordingID)
      #expect(status.status == 200)
      #expect(
        try status.json(Wire.RecordingStatus.self)
          == Wire.RecordingStatus(state: .receiving, receivedChunks: [0]))
      for index in 1..<chunks.count {
        #expect(
          try await resumed.upload(metadata.recordingID, chunk: index, chunks[index]).status == 204)
      }
      let completedUpload = try await resumed.complete(metadata.recordingID)
      #expect(completedUpload.status == 200)
      #expect(try completedUpload.json(Wire.CompleteResponse.self).meetingID == Self.meetingID)
      let admissions = await intake.admissions.entries
      #expect(admissions.count == 1)
      #expect(try Data(contentsOf: try #require(admissions.first?.file)) == bytes)
      await second.stop()
    }
  }

  /// The intake refuses the verified file once and the Mac restarts, so the
  /// receipt is only in the store and the verified file waits for the
  /// phone's retry. The phone is revoked: the revoke finds no receipt in
  /// memory to discard, and its delete takes the row with the device.
  /// Another phone then announces the same recording id. Its first announce
  /// discards the waiting file, so its `complete` hashes and admits its own
  /// bytes, never the old file unhashed.
  @Test func anotherPhonesFirstAnnounceAfterARestartAndARevokeAdmitsItsOwnBytes() async throws {
    let size = 64 * 1024
    let first = try TestService.prepare(
      chunkSize: size, customIntake: ScriptedIntake(meetingID: Self.meetingID, failures: 1))
    defer { try? FileManager.default.removeItem(at: first.directory) }
    let before = try await EngineClient.paired(first, deviceName: "Old iPhone")
    let oldBytes = Phone.seededBytes(count: 2 * size, seed: 80)
    let old = before.metadata(for: oldBytes, chunkSize: size)
    let id = old.recordingID
    try await before.uploadAll(old, oldBytes)
    #expect(await before.complete(id).code == 500)
    let inbox = first.service.engine.inbox
    #expect(inbox.hasVerified(id, format: old.format), "the verified file waits")

    // The Mac comes back over the same store and inbox.
    let intake = FakeHandoverIntake(meetingID: Self.meetingID)
    let now = first.now
    let second = HandoverService(
      configuration: first.service.configuration, store: first.store,
      intake: first.moving(intake), identity: first.service.identity, now: { now })
    await second.engine.sweepOrphans()
    #expect(inbox.hasVerified(id, format: old.format), "the sweep keeps it")
    try await second.revoke(before.device.id)
    #expect(try await first.store.handoverReceipt(recordingID: id) == nil)
    #expect(
      inbox.hasVerified(id, format: old.format),
      "the revoke found no receipt in memory and left the file")

    let other = try await EngineClient.paired(
      first, engine: second.engine, deviceName: "Other iPhone")
    let bytes = Phone.seededBytes(count: size + 99, seed: 81)
    let metadata = Phone.metadata(
      for: bytes, deviceName: "Other iPhone", recordingID: id, chunkSize: size)
    #expect(try await other.announce(metadata).code == 201)
    #expect(
      !inbox.hasVerified(id, format: old.format), "the first announce discarded the waiting file")
    #expect(inbox.loadMetadata(id) == metadata)
    try await other.uploadAll(metadata, bytes)
    #expect(await other.complete(id).code == 200)
    let admissions = await intake.admissions.entries
    #expect(admissions.count == 1)
    #expect(
      try Data(contentsOf: try #require(admissions.first?.file)) == bytes,
      "the other phone's own bytes, hashed")
  }

  /// After a restart the store cannot read the receipts. An announce answers
  /// 500 and opens nothing, so the verified file of a receipt only in the
  /// store waits for the phone's retry, and no new receipt is saved over it.
  @Test func aFailedReceiptReadAnswersTheAnnounce500AndKeepsTheFiles() async throws {
    let size = 64 * 1024
    let first = try TestService.prepare(
      chunkSize: size, customIntake: ScriptedIntake(meetingID: Self.meetingID, failures: 1))
    defer { try? FileManager.default.removeItem(at: first.directory) }
    let phone = try await EngineClient.paired(first)
    let bytes = Phone.seededBytes(count: 2 * size, seed: 82)
    let metadata = phone.metadata(for: bytes, chunkSize: size)
    let id = metadata.recordingID
    try await phone.uploadAll(metadata, bytes)
    #expect(await phone.complete(id).code == 500)

    // The Mac comes back over the same store and inbox, with nothing in
    // memory, and a temporary table of the same name shadows the receipts.
    let intake = FakeHandoverIntake(meetingID: Self.meetingID)
    let now = first.now
    let second = HandoverService(
      configuration: first.service.configuration, store: first.store,
      intake: first.moving(intake), identity: first.service.identity, now: { now })
    let resumed = EngineDevice(engine: second.engine, device: phone.device)
    try await first.store.writer.write { db in
      try db.execute(sql: "CREATE TEMP TABLE handoverReceipt (unreadable INTEGER)")
    }
    #expect(try await resumed.announce(metadata).code == 500)
    let inbox = second.engine.inbox
    #expect(inbox.hasVerified(id, format: metadata.format), "the verified file waits")
    #expect(inbox.loadMetadata(id) == metadata, "and so does its sidecar")

    // Once the store reads again, the phone's retry admits that file.
    try await first.store.writer.write { db in
      try db.execute(sql: "DROP TABLE temp.handoverReceipt")
    }
    #expect(try await resumed.announce(metadata).code == 200)
    #expect(await resumed.complete(id).code == 200)
    let admissions = await intake.admissions.entries
    #expect(try Data(contentsOf: try #require(admissions.first?.file)) == bytes)
  }

  /// A `.complete` receipt whose meeting never committed (earlier releases committed the two
  /// separately, and a crash or a full disk could land in between) is not admitted. The phone never
  /// got the 200 and holds the recording, so after a restart the sweep keeps the verified file, a
  /// re-announce lists every chunk without saying `.complete`, and `complete`, with or without that
  /// announce, admits the file again instead of answering the missing meeting. Rust:
  /// `a_complete_receipt_without_its_meeting_is_admitted_again_after_a_restart`.
  @Test func aCompleteReceiptWithoutItsMeetingIsAdmittedAgainAfterARestart() async throws {
    try await TestService.run(chunkSize: Self.chunkSize) { first in
      let phone = try await Phone.pair(first.service)
      let inbox = first.service.engine.inbox
      let missing = UUID()
      var uploads: [(RecordingMetadata, Data)] = []
      for seed in [80, 81] {
        let bytes = Phone.seededBytes(count: 2 * Self.chunkSize, seed: UInt64(seed))
        let metadata = phone.metadata(for: bytes, chunkSize: Self.chunkSize)
        try await phone.uploadAll(metadata, bytes)
        // Verified and handed to the intake, whose receipt committed and
        // whose meeting did not.
        _ = try inbox.promote(metadata.recordingID, format: metadata.format)
        var receipt = try #require(
          try await first.store.handoverReceipt(recordingID: metadata.recordingID))
        receipt.state = .complete(meetingID: missing)
        try await first.store.save(receipt)
        uploads.append((metadata, bytes))
      }
      await first.service.stop()

      // The Mac comes back over the same store and inbox.
      let intake = FakeHandoverIntake(meetingID: Self.meetingID)
      let now = first.now
      let second = HandoverService(
        configuration: first.service.configuration, store: first.store,
        intake: first.moving(intake),
        identity: try TestIdentity.load(), now: { now })
      try await second.start()
      defer { Task { await second.stop() } }

      for (metadata, _) in uploads {
        #expect(
          inbox.hasVerified(metadata.recordingID, format: metadata.format),
          "the sweep keeps the verified file")
      }
      let resumed = Phone(
        client: try LoopbackClient.forService(second), token: phone.token,
        deviceID: phone.deviceID, deviceName: phone.deviceName)
      let announced = try await resumed.announce(uploads[0].0)
      #expect(announced.status == 200)
      #expect(
        try announced.json(Wire.RecordingStatus.self)
          == Wire.RecordingStatus(state: .receiving, receivedChunks: [0, 1]),
        "not complete: the phone goes on to complete")
      for (metadata, _) in uploads {
        let completed = try await resumed.complete(metadata.recordingID)
        #expect(completed.status == 200)
        #expect(
          try completed.json(Wire.CompleteResponse.self).meetingID == Self.meetingID,
          "the new admission's meeting, not the missing one")
      }
      let admissions = await intake.admissions.entries
      #expect(admissions.count == 2, "both files are admitted again")
      for ((_, bytes), admission) in zip(uploads, admissions) {
        #expect(try Data(contentsOf: admission.file) == bytes)
      }
      await second.stop()
    }
  }

  /// A receipt the store cannot read is an error, not a missing receipt:
  /// the sweep keeps the upload's files, which a later start sweeps once
  /// the store reads again. Taken for a missing receipt, the failed read
  /// would delete a resumable upload. The read fails because a temporary
  /// table of the same name shadows `handoverReceipt` on the in-memory
  /// store's one connection. Rust:
  /// `a_failed_receipt_read_keeps_the_upload_and_answers_500`.
  @Test func aReceiptTheStoreCannotReadKeepsTheUploadThroughTheSweep() async throws {
    try await TestService.run(chunkSize: Self.chunkSize) { test in
      let phone = try await Phone.pair(test.service)
      let bytes = Phone.seededBytes(count: 2 * Self.chunkSize, seed: 83)
      let metadata = phone.metadata(for: bytes, chunkSize: Self.chunkSize)
      let chunks = Phone.chunks(of: bytes, size: Self.chunkSize)
      #expect(try await phone.announce(metadata).status == 201)
      #expect(try await phone.upload(metadata.recordingID, chunk: 0, chunks[0]).status == 204)
      let engine = test.service.engine

      try await test.store.writer.write { db in
        try db.execute(sql: "CREATE TEMP TABLE handoverReceipt (unreadable INTEGER)")
      }
      await engine.sweepOrphans()
      #expect(engine.inbox.hasPartial(metadata.recordingID), "the resumable upload is kept")
      #expect(engine.inbox.loadMetadata(metadata.recordingID) == metadata)

      // Once the store reads again, the upload resumes where it stood.
      try await test.store.writer.write { db in
        try db.execute(sql: "DROP TABLE temp.handoverReceipt")
      }
      await engine.sweepOrphans()
      #expect(engine.inbox.hasPartial(metadata.recordingID))
      #expect(try await phone.upload(metadata.recordingID, chunk: 1, chunks[1]).status == 204)
      #expect(try await phone.complete(metadata.recordingID).status == 200)
    }
  }
}
