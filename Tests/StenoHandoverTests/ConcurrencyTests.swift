import Foundation
import StenoCore
import Testing

@testable import StenoHandover

/// The engine is one actor, but every store write, file write and hash is a
/// suspension point at which the next request runs. These tests drive
/// `HandoverEngine.handle` directly (`EngineClient`), so two requests enter
/// the actor in a known order and the races the loopback clients can only
/// make likely are certain.
@Suite struct ConcurrencyTests {
  static let meetingID = UUID(uuidString: "C0C0C0C0-0000-4000-8000-000000000001")!

  @Test func aPairingSecretPairsExactlyOnceUnderConcurrentUse() async throws {
    // Both requests passed the gate (the session was open at both heads);
    // the second must find the session gone, not a save still in flight.
    try await TestService.run(start: false) { test in
      let client = EngineClient(test)
      _ = await test.service.engine.beginPairing()
      let (legitimate, intruder) = (UUID(), UUID())

      async let first = client.pair(deviceID: legitimate, deviceName: "Nicolai's iPhone")
      async let second = client.pair(deviceID: intruder, deviceName: "Photographed QR")
      let statuses = try await [first.code, second.code].sorted()

      #expect(statuses == [200, 403])
      let devices = try await test.service.pairedDevices()
      #expect(devices.count == 1, "one phone paired, not two")
      #expect(await test.service.engine.pairingIsOpen == false, "the secret is spent")
    }
  }

  @Test func concurrentCompletesAdmitOnceAndKeepTheCompleteReceipt() async throws {
    // The phone retries `complete` after its own timeout while the Mac is
    // still copying a large file. A second admission would create a second
    // meeting; with the real intake it can also fail on the moved source and,
    // before the fix, overwrite the `.complete` receipt with `.failed`.
    let intake = ScriptedIntake(
      meetingID: Self.meetingID, failures: 0, delay: .milliseconds(300), admitOnce: true)
    try await TestService.run(chunkSize: 64 * 1024, customIntake: intake, start: false) { test in
      let phone = try await EngineClient.paired(test)
      let bytes = Phone.seededBytes(count: 2 * 64 * 1024, seed: 61)
      let metadata = phone.metadata(for: bytes, chunkSize: 64 * 1024)
      try await phone.uploadAll(metadata, bytes)
      let id = metadata.recordingID

      async let first = phone.complete(id)
      try await Task.sleep(for: .milliseconds(100))
      async let second = phone.complete(id)
      let (a, b) = await (first, second)

      #expect([a.code, b.code].sorted() == [200, 409], "one admits, the retry is told to wait")
      let conflict = try #require([a, b].first { $0.code == 409 })
      #expect(
        try conflict.json(Wire.RecordingStatus.self)
          == Wire.RecordingStatus(state: .verifying, receivedChunks: [0, 1]),
        "the 409 status lists every chunk, so the phone backs off instead of re-uploading")
      #expect(await intake.admissions.count == 1)
      #expect(
        try await test.store.handoverReceipt(recordingID: id)?.state
          == .complete(meetingID: Self.meetingID))

      // The phone's next try after the backoff: same id, no new admission.
      let third = await phone.complete(id)
      #expect(third.code == 200)
      #expect(try third.json(Wire.CompleteResponse.self).meetingID == Self.meetingID)
      #expect(await intake.admissions.count == 1)
    }
  }

  @Test(.timeLimit(.minutes(1)))
  func aReceiptSaveThatReachesTheStoreLateCannotUndoALaterOne() async throws {
    // `store.save` leaves the actor before GRDB's writer queue takes the
    // save, so a save asked for first can reach the store second. The save
    // for chunk 1 is held on its way there while chunk 0 lands; the store
    // must end with both chunks, as memory does.
    let chunkSize = 64 * 1024
    try await TestService.run(chunkSize: chunkSize, start: false) { test in
      let held = HeldSave(store: test.store)
      let now = test.now
      let engine = HandoverEngine(
        configuration: test.service.configuration, identity: test.service.identity,
        store: test.store, intake: test.intake, receipts: Broadcast(initial: []),
        now: { now }, saveReceipt: held.save)
      _ = await engine.beginPairing()
      let deviceID = UUID()
      #expect(try await EngineClient(engine: engine).pair(deviceID: deviceID).code == 200)
      let phone = EngineDevice(
        engine: engine, device: try #require(try await test.store.pairedDevice(id: deviceID)))
      let bytes = Phone.seededBytes(count: 2 * chunkSize, seed: 62)
      let metadata = phone.metadata(for: bytes, chunkSize: chunkSize)
      let chunks = Phone.chunks(of: bytes, size: chunkSize)
      let id = metadata.recordingID
      #expect(try await phone.announce(metadata).code == 201)

      let first = Task { await phone.upload(id, chunk: 1, chunks[1]) }
      try await until { held.isHolding }
      let second = Task { await phone.upload(id, chunk: 0, chunks[0]) }
      try await until { await engine.activeReceipts[id]?.receivedChunks == [0, 1] }
      // Give chunk 0's save the time an in-memory write takes to overtake.
      try await Task.sleep(for: .milliseconds(200))
      #expect(held.reachedStore.isEmpty, "no receipt save overtakes the held one")
      held.release()

      #expect(await first.value.code == 204)
      #expect(await second.value.code == 204)
      #expect(held.reachedStore == [[1], [0, 1]], "the saves reach the store in order")
      #expect(try await test.store.handoverReceipt(recordingID: id)?.receivedChunks == [0, 1])
    }
  }

  @Test func theGateAnswersWhileAWholeFileHashRuns() async throws {
    // Verifying a 4 GiB upload takes seconds; `/v1/hello` and every other
    // connection's auth gate must not queue behind it on the actor.
    let chunkSize = 16 * 1024 * 1024
    try await TestService.run(chunkSize: chunkSize, start: false) { test in
      let client = EngineClient(test)
      let phone = try await EngineClient.paired(test)
      let bytes = Data(repeating: 0x5A, count: 8 * chunkSize)
      let metadata = phone.metadata(for: bytes, chunkSize: chunkSize)
      try await phone.uploadAll(metadata, bytes)
      let id = metadata.recordingID
      let inbox = test.service.engine.inbox
      let receipts = test.service.receipts

      async let completion = phone.complete(id)
      // `.verifying` is written just before the hash starts.
      for await batch in receipts
      where batch.contains(where: { $0.recordingID == id && $0.state.kind == .verifying }) {
        break
      }
      let hello = await client.hello()
      #expect(hello.code == 200)
      #expect(
        inbox.hasPartial(id) && !inbox.hasVerified(id, format: .m4aAAC),
        "hello was answered while the hash was still running, before the promote")

      let completed = await completion
      #expect(completed.code == 200)
      #expect(await test.intake.admissions.count == 1)
    }
  }

  @Test(.timeLimit(.minutes(1)))
  func aChunkThatLandsDuringACompleteLeavesTheReceiptComplete() async throws {
    // The phone sent chunk 1 again after its own timeout while the first
    // attempt was still in flight. That attempt is held after its file
    // write while the second one lands and the phone's `complete` runs to
    // the end. Its fold then finds the receipt `.complete` in memory and
    // changes nothing, and the chunk is answered as received. Folded in,
    // `.receiving` would send the phone's next `complete` back to the
    // start, and the upload after it would become a second meeting.
    let chunkSize = 64 * 1024
    try await TestService.run(chunkSize: chunkSize, start: false) { test in
      let held = HeldWrite()
      let now = test.now
      let engine = HandoverEngine(
        configuration: test.service.configuration, identity: test.service.identity,
        store: test.store, intake: test.intake, receipts: Broadcast(initial: []),
        now: { now }, writeChunk: held.write)
      _ = await engine.beginPairing()
      let deviceID = UUID()
      #expect(try await EngineClient(engine: engine).pair(deviceID: deviceID).code == 200)
      let phone = EngineDevice(
        engine: engine, device: try #require(try await test.store.pairedDevice(id: deviceID)))
      let bytes = Phone.seededBytes(count: 2 * chunkSize, seed: 63)
      let metadata = phone.metadata(for: bytes, chunkSize: chunkSize)
      let chunks = Phone.chunks(of: bytes, size: chunkSize)
      let id = metadata.recordingID
      #expect(try await phone.announce(metadata).code == 201)
      #expect(await phone.upload(id, chunk: 0, chunks[0]).code == 204)

      held.arm()
      let first = Task { await phone.upload(id, chunk: 1, chunks[1]) }
      await held.held()
      #expect(await phone.upload(id, chunk: 1, chunks[1]).code == 204)
      let meetingID = try await Self.completed(phone, id)
      held.release()
      #expect(await first.value.code == 204, "the chunk counts as received")

      try await Self.staysComplete(
        phone, id, meetingID: meetingID, store: test.store, intake: test.intake)
    }
  }

  /// A phone paired over an on-disk `StoreGate` store, so a test can hold an
  /// announce's receipt read, with a two-chunk recording not yet announced.
  private struct Gated {
    static let chunkSize = 64 * 1024
    let gate: StoreGate
    let test: TestService
    let phone: EngineDevice
    let metadata: RecordingMetadata
    let chunks: [Data]

    init(seed: UInt64) async throws {
      gate = try StoreGate()
      test = try TestService.prepare(chunkSize: Self.chunkSize, store: gate.store)
      phone = try await EngineClient.paired(test)
      let bytes = Phone.seededBytes(count: 2 * Self.chunkSize, seed: seed)
      metadata = phone.metadata(for: bytes, chunkSize: Self.chunkSize)
      chunks = Phone.chunks(of: bytes, size: Self.chunkSize)
    }

    var id: UUID { metadata.recordingID }

    /// Starts an announce whose receipt read is held, and returns once it
    /// is: the read ran, and found no receipt.
    func announceHeldAtItsRead() async -> Task<HandoverResponse, any Error> {
      gate.receiptRead.arm()
      let announcing = Task { [phone, metadata] in try await phone.announce(metadata) }
      await gate.receiptRead.held()
      return announcing
    }

    /// Releases the gate's holds and deletes both directories.
    func remove() {
      gate.remove()
      try? FileManager.default.removeItem(at: test.directory)
    }
  }

  @Test(.timeLimit(.minutes(1)))
  func anAnnounceThatFoundNoReceiptKeepsTheOneMadeMeanwhile() async throws {
    // The phone announces a new recording twice at once (a retry after its
    // own timeout). One announce is held in its receipt read, which found
    // nothing, while the other one answers 201 and chunk 0 lands. The held
    // one then finds that receipt in memory and answers as a re-announce,
    // so chunk 0 stays. A fresh receipt would drop it, in memory and in the
    // store.
    let gated = try await Gated(seed: 64)
    defer { gated.remove() }
    let (phone, id) = (gated.phone, gated.id)

    let second = await gated.announceHeldAtItsRead()
    #expect(try await phone.announce(gated.metadata).code == 201)
    #expect(await phone.upload(id, chunk: 0, gated.chunks[0]).code == 204)
    gated.gate.receiptRead.release()
    let reannounced = try await second.value

    #expect(reannounced.code == 200, "answered as a re-announce")
    #expect(
      try reannounced.json(Wire.RecordingStatus.self)
        == Wire.RecordingStatus(state: .receiving, receivedChunks: [0]))
    #expect(await phone.engine.activeReceipts[id]?.receivedChunks == [0])
    #expect(try await gated.test.store.handoverReceipt(recordingID: id)?.receivedChunks == [0])
    #expect(!gated.gate.timedOut, "nothing waited on the held read")
  }

  @Test(.timeLimit(.minutes(1)))
  func anAnnounceThatFoundNoReceiptLeavesACompleteOneComplete() async throws {
    // As above, but the other announce's upload runs to the end, `complete`
    // and all, while the held one waits in its read. That one then answers
    // with the `.complete` receipt and changes nothing. A fresh `.receiving`
    // receipt would send the phone's next `complete` back to the start, and
    // the upload after it would become a second meeting.
    let gated = try await Gated(seed: 65)
    defer { gated.remove() }
    let (phone, id) = (gated.phone, gated.id)

    let second = await gated.announceHeldAtItsRead()
    #expect(try await phone.announce(gated.metadata).code == 201)
    for (index, chunk) in gated.chunks.enumerated() {
      #expect(await phone.upload(id, chunk: index, chunk).code == 204)
    }
    let meetingID = try await Self.completed(phone, id)
    gated.gate.receiptRead.release()
    let reannounced = try await second.value

    #expect(reannounced.code == 200, "answered as a re-announce")
    #expect(
      try reannounced.json(Wire.RecordingStatus.self)
        == Wire.RecordingStatus(state: .complete, receivedChunks: [0, 1]))
    try await Self.staysComplete(
      phone, id, meetingID: meetingID, store: gated.test.store, intake: gated.test.intake)
    #expect(!gated.gate.timedOut, "nothing waited on the held read")
  }

  /// The meeting a `complete` of `recordingID` admitted.
  private static func completed(_ phone: EngineDevice, _ recordingID: UUID) async throws -> UUID {
    let completed = await phone.complete(recordingID)
    try #require(completed.code == 200)
    return try completed.json(Wire.CompleteResponse.self).meetingID
  }

  /// Memory and the store hold `recordingID` as `.complete` with
  /// `meetingID`, the intake admitted it once, and the phone's next
  /// `complete` answers that meeting again.
  private static func staysComplete(
    _ phone: EngineDevice, _ recordingID: UUID, meetingID: UUID, store: MeetingStore,
    intake: FakeHandoverIntake
  ) async throws {
    let complete = HandoverState.complete(meetingID: meetingID)
    #expect(await phone.engine.activeReceipts[recordingID]?.state == complete)
    #expect(try await store.handoverReceipt(recordingID: recordingID)?.state == complete)
    #expect(await intake.admissions.count == 1)
    #expect(try await completed(phone, recordingID) == meetingID)
    #expect(await intake.admissions.count == 1, "the next complete admits nothing new")
  }
}
