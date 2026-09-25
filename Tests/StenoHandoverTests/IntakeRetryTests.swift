import Foundation
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
/// receipt accounts for.
@Suite struct IntakeRetryTests {
  static let chunkSize = 256 * 1024
  static let meetingID = UUID(uuidString: "1ABE1000-0000-4000-8000-0000000000AD")!

  @Test func intakeFailureIs500AndTheNextCompleteAdmitsTheSameVerifiedFile() async throws {
    let intake = ScriptedIntake(meetingID: Self.meetingID, failures: 1)
    let test = try await TestService.start(chunkSize: Self.chunkSize, customIntake: intake)
    defer { Task { await test.stop() } }
    let phone = try await Phone.pair(test.service)
    let bytes = Phone.seededBytes(count: 2 * Self.chunkSize, seed: 77)
    let metadata = phone.metadata(for: bytes, chunkSize: Self.chunkSize)
    try await phone.uploadAll(metadata, bytes)
    let inbox = test.service.engine.inbox

    let failed = try await phone.complete(metadata.recordingID)
    #expect(failed.status == 500)
    #expect(try failed.json(Wire.Problem.self).error.contains("refused"))
    #expect(inbox.hasVerified(metadata.recordingID, format: .m4aAAC), "the verified file waits")
    #expect(!inbox.hasPartial(metadata.recordingID))
    #expect(inbox.loadMetadata(metadata.recordingID) == metadata, "the sidecar waits with it")
    let receipt = try #require(
      try await test.store.handoverReceipt(recordingID: metadata.recordingID))
    #expect(receipt.state.kind == .failed)
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

  @Test func reAnnounceAfterAnIntakeFailureKeepsTheChunkSet() async throws {
    // The phone's executor answers a 5xx at complete with a backoff and then
    // starts the recording over at announce (`upload-executor.test.ts`).
    let intake = ScriptedIntake(meetingID: Self.meetingID, failures: 1)
    let test = try await TestService.start(chunkSize: Self.chunkSize, customIntake: intake)
    defer { Task { await test.stop() } }
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

  @Test func aRestartedMacResumesFromTheStoredReceiptAndSweepsOnlyOrphans() async throws {
    let first = try await TestService.start(chunkSize: Self.chunkSize)
    defer { Task { await first.stop() } }
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
    try await first.store.save(receipt(completed, .complete(meetingID: UUID())))
    let refused = UUID()
    _ = try seed(refused)
    _ = try inbox.promote(refused, format: .m4aAAC)
    try await first.store.save(receipt(refused, .failed("admit: refused")))
    await first.service.stop()

    // The Mac comes back over the same store and inbox.
    let intake = FakeHandoverIntake(meetingID: Self.meetingID)
    let now = first.now
    let second = HandoverService(
      configuration: first.service.configuration, store: first.store, intake: intake,
      identity: try TestIdentity.load(), clock: ManualClock(), now: { now })
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
      client: try await LoopbackClient.forService(second), token: phone.token,
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
