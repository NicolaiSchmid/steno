import Foundation
import StenoCore
import Testing

@testable import StenoHandover

/// The engine is one actor, but every store read and write, file write and
/// hash is a suspension point at which the next request runs. These tests
/// drive `HandoverEngine.handle` directly (`EngineClient`), so two requests
/// enter the actor in a known order and the races the loopback clients can
/// only make likely are certain. Tests that need one request stopped at a
/// chosen suspension point hold it there: a receipt save on its way to the
/// store (`HeldSave`), a chunk write after its bytes landed (`HeldWrite`), a
/// store read (`StoreGate`), a whole-file hash after it ran (`HeldHash`)
/// and an admission in the intake (`HeldIntake`).
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
      let engine = Self.engine(test, saveReceipt: held.save)
      let phone = try await EngineClient.paired(test, engine: engine)
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
    // the chunk would put the receipt back to `.receiving`, the phone's
    // next `complete` would start over, and the upload after it would
    // become a second meeting.
    let chunkSize = 64 * 1024
    try await TestService.run(chunkSize: chunkSize, start: false) { test in
      let held = HeldWrite()
      let engine = Self.engine(test, writeChunk: held.write)
      let phone = try await EngineClient.paired(test, engine: engine)
      let bytes = Phone.seededBytes(count: 2 * chunkSize, seed: 63)
      let metadata = phone.metadata(for: bytes, chunkSize: chunkSize)
      let chunks = Phone.chunks(of: bytes, size: chunkSize)
      let id = metadata.recordingID
      #expect(try await phone.announce(metadata).code == 201)
      #expect(await phone.upload(id, chunk: 0, chunks[0]).code == 204)

      held.arm()
      defer { held.release() }
      let first = Task { await phone.upload(id, chunk: 1, chunks[1]) }
      await held.held()
      #expect(await phone.upload(id, chunk: 1, chunks[1]).code == 204)
      let meetingID = try await Self.completed(phone, id)
      held.release()
      #expect(await first.value.code == 204, "the chunk counts as received")

      try await Self.staysComplete(phone, id, meetingID: meetingID, test: test)
    }
  }

  /// A phone paired over an on-disk `StoreGate` store, so a test can hold a
  /// request's receipt read, with a two-chunk recording not yet announced.
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

    /// Starts `request` and returns once its receipt read is held: the read
    /// ran and found what the store held then. Only that read is held; the
    /// request goes on at `gate.receiptRead.release()`.
    func heldAtItsRead(
      _ request: @escaping @Sendable () async throws -> HandoverResponse
    ) async -> Task<HandoverResponse, any Error> {
      gate.receiptRead.arm()
      let requesting = Task { try await request() }
      await gate.receiptRead.held()
      return requesting
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

    let second = await gated.heldAtItsRead { [metadata = gated.metadata] in
      try await phone.announce(metadata)
    }
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
    // receipt would make the phone's next `complete` start over, and
    // the upload after it would become a second meeting.
    let gated = try await Gated(seed: 65)
    defer { gated.remove() }
    let (phone, id) = (gated.phone, gated.id)

    let second = await gated.heldAtItsRead { [metadata = gated.metadata] in
      try await phone.announce(metadata)
    }
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
    try await Self.staysComplete(phone, id, meetingID: meetingID, test: gated.test)
    #expect(!gated.gate.timedOut, "nothing waited on the held read")
  }

  @Test(.timeLimit(.minutes(1)))
  func aRequestThatReadAnOlderReceiptKeepsTheOneAdvancedMeanwhile() async throws {
    // Memory holds no receipt (as after a restart), so a status request
    // reads chunk 0's receipt from the store and is held there while chunk
    // 1 lands. The held request then finds both chunks in memory and
    // answers with them. Had it remembered the row it read, memory would
    // lose chunk 1 and the next save would drop it from the store; over a
    // `.complete` receipt, the row would put back `.receiving`, and the
    // upload after it would become a second meeting.
    let gated = try await Gated(seed: 66)
    defer { gated.remove() }
    let (phone, id) = (gated.phone, gated.id)
    #expect(try await phone.announce(gated.metadata).code == 201)
    #expect(await phone.upload(id, chunk: 0, gated.chunks[0]).code == 204)
    await phone.engine.forget(id)

    let status = await gated.heldAtItsRead { await phone.status(id) }
    #expect(await phone.upload(id, chunk: 1, gated.chunks[1]).code == 204)
    gated.gate.receiptRead.release()
    let answered = try await status.value

    #expect(answered.code == 200)
    #expect(
      try answered.json(Wire.RecordingStatus.self)
        == Wire.RecordingStatus(state: .receiving, receivedChunks: [0, 1]))
    #expect(await phone.engine.activeReceipts[id]?.receivedChunks == [0, 1])
    #expect(try await gated.test.store.handoverReceipt(recordingID: id)?.receivedChunks == [0, 1])
    #expect(!gated.gate.timedOut, "nothing waited on the held read")
  }

  @Test(.timeLimit(.minutes(1)))
  func aCompleteOfARevokedPhoneStaysOutOfAnotherPhonesReceipt() async throws {
    // The phone is revoked while the intake holds its `complete`, another
    // phone pairs, announces the same recording id and sends chunk 0, and
    // the revoked phone pairs again under its device id, so its write would
    // reach memory and the store. When the intake answers, the revoked
    // phone's `complete` still answers 200 with the meeting the intake
    // admitted, but its `.complete` write finds the other phone's receipt in
    // memory and changes nothing: a device's write never changes a receipt
    // another device announced. The phone paired again holds no receipt of
    // that id, so its status request answers 404.
    let chunkSize = 64 * 1024
    let intake = HeldIntake(FakeHandoverIntake(meetingID: Self.meetingID))
    defer { intake.release() }
    try await TestService.run(
      chunkSize: chunkSize, intake: intake.fake, customIntake: intake, start: false
    ) { test in
      let phone = try await EngineClient.paired(test)
      let bytes = Phone.seededBytes(count: 2 * chunkSize, seed: 67)
      let metadata = phone.metadata(for: bytes, chunkSize: chunkSize)
      let chunks = Phone.chunks(of: bytes, size: chunkSize)
      let id = metadata.recordingID
      try await phone.uploadAll(metadata, bytes)

      let completion = Task { await phone.complete(id) }
      await intake.held()
      try await test.service.revoke(phone.device.id)
      let other = try await EngineClient.paired(test, deviceName: "Other iPhone")
      #expect(try await other.announce(metadata).code == 201)
      #expect(await other.upload(id, chunk: 0, chunks[0]).code == 204)
      _ = await test.service.engine.beginPairing()
      #expect(try await EngineClient(test).pair(deviceID: phone.device.id).code == 200)
      intake.release()
      let completed = await completion.value

      #expect(completed.code == 200, "the intake admitted the revoked phone's recording")
      #expect(try completed.json(Wire.CompleteResponse.self).meetingID == Self.meetingID)
      #expect(await test.intake.admissions.count == 1)
      let inMemory = try #require(await test.service.engine.activeReceipts[id])
      let stored = try #require(try await test.store.handoverReceipt(recordingID: id))
      for receipt in [inMemory, stored] {
        #expect(receipt.deviceID == other.device.id)
        #expect(receipt.state == .receiving, "not `.complete` with the revoked phone's meeting")
        #expect(receipt.receivedChunks == [0])
      }
      #expect(await phone.status(id).code == 404, "the other phone owns the recording id")
    }
  }

  @Test(.timeLimit(.minutes(1)))
  func anAdmissionOfARevokedPhoneLeavesAnotherPhonesFilesAlone() async throws {
    // The phone is revoked while the intake holds its `complete`, and
    // another phone pairs, announces the same recording id and sends chunk
    // 0. When the intake answers, the revoked phone's admission finds the
    // other phone's receipt in memory and leaves the inbox as it is: that
    // phone's partial and sidecar stay, and its upload completes. Without
    // the sidecar, its `complete` would answer 404 and the phone would
    // announce and send every chunk again.
    let chunkSize = 64 * 1024
    let intake = HeldIntake(FakeHandoverIntake(meetingID: Self.meetingID))
    defer { intake.release() }
    try await TestService.run(
      chunkSize: chunkSize, intake: intake.fake, customIntake: intake, start: false
    ) { test in
      let phone = try await EngineClient.paired(test)
      let bytes = Phone.seededBytes(count: 2 * chunkSize, seed: 69)
      let metadata = phone.metadata(for: bytes, chunkSize: chunkSize)
      let chunks = Phone.chunks(of: bytes, size: chunkSize)
      let id = metadata.recordingID
      let inbox = test.service.engine.inbox
      try await phone.uploadAll(metadata, bytes)

      let completion = Task { await phone.complete(id) }
      await intake.held()
      try await test.service.revoke(phone.device.id)
      let other = try await EngineClient.paired(test, deviceName: "Other iPhone")
      #expect(try await other.announce(metadata).code == 201)
      #expect(await other.upload(id, chunk: 0, chunks[0]).code == 204)
      intake.release()
      #expect(await completion.value.code == 200, "the intake admitted the revoked phone's file")

      #expect(inbox.hasPartial(id), "the other phone's partial stays")
      #expect(inbox.loadMetadata(id) == metadata, "and so does its sidecar")
      #expect(await other.upload(id, chunk: 1, chunks[1]).code == 204)
      #expect(await other.complete(id).code == 200)
      let admissions = await test.intake.admissions.entries
      #expect(admissions.map(\.device.id) == [phone.device.id, other.device.id])
      #expect(try Data(contentsOf: try #require(admissions.last?.file)) == bytes)
    }
  }

  @Test(.timeLimit(.minutes(1)))
  func aReAnnounceDuringTheIntakeLeavesNoFileOfTheAdmittedRecording() async throws {
    // The phone announces again while the intake holds its `complete`,
    // after the intake took the verified file. The announce finds no file,
    // opens a new partial and sidecar and answers 200 with no chunk listed.
    // Once the intake answers, the recording is admitted and no file of it
    // stays in the inbox; the empty partial would otherwise wait there for
    // the next start's sweep.
    let chunkSize = 64 * 1024
    let intake = HeldIntake(FakeHandoverIntake(meetingID: Self.meetingID))
    defer { intake.release() }
    try await TestService.run(
      chunkSize: chunkSize, intake: intake.fake, customIntake: intake, start: false
    ) { test in
      let phone = try await EngineClient.paired(test)
      let bytes = Phone.seededBytes(count: 2 * chunkSize, seed: 70)
      let metadata = phone.metadata(for: bytes, chunkSize: chunkSize)
      let id = metadata.recordingID
      let inbox = test.service.engine.inbox
      try await phone.uploadAll(metadata, bytes)

      let completion = Task { await phone.complete(id) }
      await intake.held()
      #expect(!inbox.hasVerified(id, format: metadata.format), "the intake took the file")
      let announced = try await phone.announce(metadata)
      #expect(announced.code == 200)
      #expect(
        try announced.json(Wire.RecordingStatus.self)
          == Wire.RecordingStatus(state: .receiving, receivedChunks: []))
      #expect(inbox.hasPartial(id) && inbox.loadMetadata(id) == metadata)
      intake.release()
      #expect(await completion.value.code == 200)

      #expect(!inbox.hasPartial(id), "the re-announce's partial goes")
      #expect(inbox.loadMetadata(id) == nil, "and so does its sidecar")
      #expect(inbox.recordingIDs().isEmpty, "no file of the recording is left")
      try await Self.staysComplete(phone, id, meetingID: Self.meetingID, test: test)
    }
  }

  @Test(.timeLimit(.minutes(1)))
  func aRefusedCompleteOfARevokedPhoneLeavesAnotherPhonesUploadAlone() async throws {
    // The phone's `complete` is held after its hash while the phone is
    // revoked (its receipt and files go) and another phone pairs, announces
    // the same recording id and sends chunk 0. The verify then answers 401
    // and finds the other phone's receipt in memory, so its refusal keeps
    // that receipt, its partial and its sidecar, and the other phone's
    // upload completes. Gone, they would be answered 404 "announce again",
    // and the other phone would send every chunk again.
    let chunkSize = 64 * 1024
    try await TestService.run(chunkSize: chunkSize, start: false) { test in
      let held = HeldHash()
      let engine = Self.engine(test, hashMatches: held.hashMatches)
      let phone = try await EngineClient.paired(test, engine: engine)
      let bytes = Phone.seededBytes(count: 2 * chunkSize, seed: 71)
      let metadata = phone.metadata(for: bytes, chunkSize: chunkSize)
      let chunks = Phone.chunks(of: bytes, size: chunkSize)
      let id = metadata.recordingID
      try await phone.uploadAll(metadata, bytes)

      held.arm()
      defer { held.release() }
      let completion = Task { await phone.complete(id) }
      await held.held()
      try await engine.revoke(phone.device.id)
      let other = try await EngineClient.paired(test, engine: engine, deviceName: "Other iPhone")
      #expect(try await other.announce(metadata).code == 201)
      #expect(await other.upload(id, chunk: 0, chunks[0]).code == 204)
      held.release()
      #expect(await completion.value.code == 401, "the phone learns it was unpaired")

      #expect(await engine.activeReceipts[id]?.deviceID == other.device.id)
      #expect(engine.inbox.hasPartial(id), "the other phone's partial stays")
      #expect(engine.inbox.loadMetadata(id) == metadata, "and so does its sidecar")
      #expect(await other.upload(id, chunk: 1, chunks[1]).code == 204)
      #expect(await other.complete(id).code == 200)
      let admissions = await test.intake.admissions.entries
      #expect(admissions.map(\.device.id) == [other.device.id])
      #expect(try Data(contentsOf: try #require(admissions.first?.file)) == bytes)
    }
  }

  @Test(.timeLimit(.minutes(1)))
  func aLateChunkOfARevokedPhoneStaysOutOfAnotherPhonesReceipt() async throws {
    // Chunk 0 is held after its file write while the phone is revoked (its
    // receipt and partial go) and another phone pairs and announces the
    // same recording id. The fold then finds the other phone's receipt in
    // memory and changes nothing: 404, and that receipt lists no chunk.
    // Folded in, the chunk would be listed though the other phone's partial
    // lacks it, so that phone would never send it and its `complete` would
    // fail the hash and start over.
    let chunkSize = 64 * 1024
    try await TestService.run(chunkSize: chunkSize, start: false) { test in
      let held = HeldWrite()
      let engine = Self.engine(test, writeChunk: held.write)
      let phone = try await EngineClient.paired(test, engine: engine)
      let bytes = Phone.seededBytes(count: 2 * chunkSize, seed: 68)
      let metadata = phone.metadata(for: bytes, chunkSize: chunkSize)
      let chunks = Phone.chunks(of: bytes, size: chunkSize)
      let id = metadata.recordingID
      #expect(try await phone.announce(metadata).code == 201)

      held.arm()
      defer { held.release() }
      let late = Task { await phone.upload(id, chunk: 0, chunks[0]) }
      await held.held()
      try await engine.revoke(phone.device.id)
      let other = try await EngineClient.paired(test, engine: engine, deviceName: "Other iPhone")
      #expect(try await other.announce(metadata).code == 201)
      held.release()

      #expect(await late.value.code == 404)
      let inMemory = try #require(await engine.activeReceipts[id])
      let stored = try #require(try await test.store.handoverReceipt(recordingID: id))
      for receipt in [inMemory, stored] {
        #expect(receipt.deviceID == other.device.id)
        #expect(receipt.receivedChunks == [], "not the revoked phone's chunk 0")
      }
    }
  }

  /// An engine beside `test`'s service, over its store and intake, that
  /// saves receipts, writes chunks and hashes the partial through the given
  /// seams.
  private static func engine(
    _ test: TestService,
    saveReceipt: (@Sendable (HandoverReceipt) async throws -> Void)? = nil,
    writeChunk: (@Sendable (Data, UInt64, URL) async throws -> Void)? = nil,
    hashMatches: (@Sendable (URL, Data) async throws -> Bool)? = nil
  ) -> HandoverEngine {
    let now = test.now
    return HandoverEngine(
      configuration: test.service.configuration, identity: test.service.identity,
      store: test.store, intake: test.moving(test.intake), receipts: Broadcast(initial: []),
      now: { now }, saveReceipt: saveReceipt, writeChunk: writeChunk, hashMatches: hashMatches)
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
    _ phone: EngineDevice, _ recordingID: UUID, meetingID: UUID, test: TestService
  ) async throws {
    let complete = HandoverState.complete(meetingID: meetingID)
    #expect(await phone.engine.activeReceipts[recordingID]?.state == complete)
    #expect(try await test.store.handoverReceipt(recordingID: recordingID)?.state == complete)
    #expect(await test.intake.admissions.count == 1)
    #expect(try await completed(phone, recordingID) == meetingID)
    #expect(await test.intake.admissions.count == 1, "the next complete admits nothing new")
  }
}
