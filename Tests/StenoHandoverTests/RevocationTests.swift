import Foundation
import StenoCore
import Testing

@testable import StenoHandover

/// Revoking a phone mid-upload: its partial and receipt are gone, every
/// bearer route answers 401 at the gate (the phone's "unpaired" signal), and
/// another phone's upload is untouched. Both ways in: `revoke(_:)` from the
/// Mac and `DELETE /v1/pairing` from the phone. A `complete` that read its
/// receipt before the revoke admits nothing, also when the phone paired again
/// meanwhile.
@Suite struct RevocationTests {
  static let chunkSize = 256 * 1024

  struct Upload {
    let phone: Phone
    let metadata: RecordingMetadata
    let chunks: [Data]
    let bytes: Data

    static func begin(_ test: TestService, seed: UInt64, name: String) async throws -> Upload {
      let phone = try await Phone.pair(test.service, deviceName: name)
      let bytes = Phone.seededBytes(count: 2 * RevocationTests.chunkSize, seed: seed)
      let metadata = phone.metadata(for: bytes, chunkSize: RevocationTests.chunkSize)
      let chunks = Phone.chunks(of: bytes, size: RevocationTests.chunkSize)
      #expect(try await phone.announce(metadata).status == 201)
      #expect(try await phone.upload(metadata.recordingID, chunk: 0, chunks[0]).status == 204)
      return Upload(phone: phone, metadata: metadata, chunks: chunks, bytes: bytes)
    }

    var id: UUID { metadata.recordingID }
  }

  @Test func revokeAndUnpairDropTheUploadAndMakeEveryBearerRoute401() async throws {
    try await TestService.run(chunkSize: Self.chunkSize) { test in
      let inbox = test.service.engine.inbox
      let a = try await Upload.begin(test, seed: 11, name: "Phone A")
      let b = try await Upload.begin(test, seed: 12, name: "Phone B")
      #expect(inbox.hasPartial(a.id) && inbox.hasPartial(b.id))
      let live = await test.service.engine.receiptsSnapshot.map(\.recordingID)
      #expect(Set(live) == [a.id, b.id])

      // The Mac revokes A.
      try await test.service.revoke(a.phone.deviceID)

      #expect(!inbox.hasPartial(a.id), "A's partial is discarded")
      #expect(inbox.loadMetadata(a.id) == nil)
      #expect(
        try await test.store.handoverReceipt(recordingID: a.id) == nil, "the receipt cascades")
      #expect(await test.service.engine.receiptsSnapshot.map(\.recordingID) == [b.id])
      #expect(try await test.service.pairedDevices().map(\.id) == [b.phone.deviceID])
      #expect(inbox.hasPartial(b.id), "B's upload is untouched")

      // Every bearer route rejects A at the gate; the engine never sees them
      // and the chunk body is not read.
      let handled = test.metrics.handledRequests
      #expect(try await a.phone.announce(a.metadata).status == 401)
      #expect(try await a.phone.status(a.id).status == 401)
      #expect(try await a.phone.upload(a.id, chunk: 1, a.chunks[1]).status == 401)
      #expect(try await a.phone.complete(a.id).status == 401)
      #expect(
        try await a.phone.client.request("DELETE", "/v1/pairing", headers: a.phone.bearer).status
          == 401)
      #expect(test.metrics.handledRequests == handled, "rejected before the engine")
      #expect(test.metrics.discardedBodyBytes <= a.chunks[1].count + 4096)
      #expect(
        try await a.phone.client.request("GET", "/v1/hello").status == 200, "hello needs no token")
      #expect(!inbox.hasPartial(a.id), "a rejected announce creates nothing")

      // B continues: a resume from status, the last chunk, complete.
      let resume = try await b.phone.status(b.id)
      #expect(resume.status == 200)
      #expect(try resume.json(Wire.RecordingStatus.self).receivedChunks == [0])
      #expect(try await b.phone.upload(b.id, chunk: 1, b.chunks[1]).status == 204)
      #expect(try await b.phone.complete(b.id).status == 200)
      let admissions = await test.intake.admissions.entries
      #expect(admissions.map(\.device.id) == [b.phone.deviceID])
      #expect(try Data(contentsOf: try #require(admissions.first?.file)) == b.bytes)

      // B unpairs itself from the phone side.
      let unpair = try await b.phone.client.request(
        "DELETE", "/v1/pairing", headers: b.phone.bearer)
      #expect(unpair.status == 204)
      #expect(try await test.service.pairedDevices().isEmpty)
      #expect(await test.service.engine.receiptsSnapshot.isEmpty)
      #expect(try await b.phone.status(b.id).status == 401)
      #expect(try await b.phone.complete(b.id).status == 401)
      #expect(try await b.phone.announce(b.metadata).status == 401)
    }
  }

  @Test func unpairMidUploadDiscardsThePartialAndTheReceipt() async throws {
    try await TestService.run(chunkSize: Self.chunkSize) { test in
      let inbox = test.service.engine.inbox
      let upload = try await Upload.begin(test, seed: 13, name: "Phone C")
      let receipts = test.service.receipts

      let unpair = try await upload.phone.client.request(
        "DELETE", "/v1/pairing", headers: upload.phone.bearer)
      #expect(unpair.status == 204)
      #expect(!inbox.hasPartial(upload.id))
      #expect(inbox.loadMetadata(upload.id) == nil)
      #expect(try await test.store.handoverReceipt(recordingID: upload.id) == nil)
      #expect(try await test.store.pairedDevice(id: upload.phone.deviceID) == nil)

      // The receipt stream saw the drop.
      let observed = Task { () -> Bool in
        for await batch in receipts where batch.isEmpty { return true }
        return false
      }
      #expect(await observed.value)
      #expect(try await upload.phone.upload(upload.id, chunk: 1, upload.chunks[1]).status == 401)
      #expect(await test.intake.admissions.count == 0)
    }
  }

  /// Polls `condition`, set by the engine before a suspension, for at most
  /// five seconds.
  private static func until(_ condition: () async -> Bool) async throws {
    for _ in 0..<5000 {
      if await condition() { return }
      try await Task.sleep(for: .milliseconds(1))
    }
    Issue.record("the condition never held")
  }

  /// The Mac comes back over the store and inbox of a phone that uploaded
  /// every chunk: after a restart its receipt is only in the store.
  private struct Restarted {
    let gate: StoreGate
    let test: TestService
    /// The phone's pairing, made before the restart.
    let paired: PairedDevice
    let bytes: Data
    let metadata: RecordingMetadata
    /// The meeting, when the phone completed the upload before the restart.
    let meetingID: UUID?
    let service: HandoverService
    let intake: FakeHandoverIntake
    /// The phone, talking to the restarted engine.
    let phone: EngineDevice

    init(completed: Bool = false) async throws {
      let chunkSize = 64 * 1024
      gate = try StoreGate()
      test = try TestService.prepare(chunkSize: chunkSize, store: gate.store)
      let before = try await EngineClient.paired(test)
      paired = before.device
      bytes = Phone.seededBytes(count: 2 * chunkSize, seed: 99)
      metadata = before.metadata(for: bytes, chunkSize: chunkSize)
      try await before.uploadAll(metadata, bytes)
      meetingID =
        completed
        ? try await before.complete(metadata.recordingID).json(Wire.CompleteResponse.self).meetingID
        : nil
      intake = FakeHandoverIntake()
      let now = test.now
      service = HandoverService(
        configuration: test.service.configuration, store: test.store, intake: intake,
        identity: test.service.identity, now: { now })
      phone = EngineDevice(engine: service.engine, device: paired)
    }

    var id: UUID { metadata.recordingID }

    func remove() {
      gate.remove()
      try? FileManager.default.removeItem(at: test.directory)
    }

    /// Nothing reached the intake and nothing of the upload is left.
    func expectNothingAdmitted() async throws {
      #expect(await intake.admissions.count == 0, "the intake never sees the file")
      let inbox = service.engine.inbox
      #expect(
        !inbox.hasPartial(id) && !inbox.hasVerified(id, format: metadata.format)
          && inbox.loadMetadata(id) == nil, "its files are gone")
      #expect(await service.engine.receiptsSnapshot.isEmpty)
      #expect(try await test.store.handoverReceipt(recordingID: id) == nil)
    }
  }

  /// A revoke that lands while `complete` reads the store finds nothing in
  /// memory to discard, so the files are still there for the verify; the
  /// engine itself must keep a revoked device's recording from the intake.
  /// With `pairsAgain`, the phone pairs again under the same device id
  /// before the read returns.
  private func completeAfterARevokeDuringItsReceiptRead(pairsAgain: Bool) async throws {
    let restarted = try await Restarted()
    defer { restarted.remove() }
    let gate = restarted.gate
    let phone = restarted.paired

    gate.receiptRead.arm()
    let completing = Task { await restarted.phone.complete(restarted.id) }
    await gate.receiptRead.held()
    try await restarted.service.revoke(phone.id)
    if pairsAgain {
      _ = await restarted.service.engine.beginPairing()
      let pairing = try await EngineClient(engine: restarted.service.engine).pair(
        deviceID: phone.id, deviceName: phone.name)
      #expect(pairing.code == 200)
    }
    gate.receiptRead.release()
    let response = await completing.value

    #expect(response.code == 401, "the phone learns it was unpaired")
    try await restarted.expectNothingAdmitted()
    let stored = try await restarted.test.store.pairedDevice(id: phone.id)
    #expect((stored != nil) == pairsAgain, "only pairing again brings the device back")
    if let stored {
      // The new pairing uploads the recording again, and it goes through.
      let again = EngineDevice(engine: restarted.service.engine, device: stored)
      try await again.uploadAll(restarted.metadata, restarted.bytes)
      #expect(await again.complete(restarted.id).code == 200)
    }
    #expect(!gate.timedOut, "nothing waited on the held read")
  }

  @Test(.timeLimit(.minutes(1)))
  func aCompleteThatReadItsReceiptBeforeARevokeAdmitsNothing() async throws {
    try await completeAfterARevokeDuringItsReceiptRead(pairsAgain: false)
  }

  @Test(.timeLimit(.minutes(1)))
  func aPhoneThatPairedAgainDoesNotLetTheOldCompleteThrough() async throws {
    try await completeAfterARevokeDuringItsReceiptRead(pairsAgain: true)
  }

  /// Until a revoke's store delete commits, a store read still returns the
  /// device's receipt. A `complete` that starts meanwhile is refused before
  /// it reads, and one that read before the revoke is refused when its read
  /// returns, before it writes: either would otherwise wait on the held
  /// delete.
  @Test(.timeLimit(.minutes(1)))
  func aCompleteDuringARevokesStoreDeleteAdmitsNothing() async throws {
    let restarted = try await Restarted()
    defer { restarted.remove() }
    let gate = restarted.gate

    gate.receiptRead.arm()
    let readBefore = Task { await restarted.phone.complete(restarted.id) }
    await gate.receiptRead.held()
    gate.deviceDelete.arm()
    let revoking = Task { try await restarted.service.revoke(restarted.paired.id) }
    await gate.deviceDelete.held()

    let startedDuring = await restarted.phone.complete(restarted.id)
    #expect(startedDuring.code == 401, "a complete during the delete is refused")
    gate.receiptRead.release()
    #expect(await readBefore.value.code == 401, "a complete that read before it is refused")
    gate.deviceDelete.release()
    try await revoking.value

    try await restarted.expectNothingAdmitted()
    #expect(!gate.timedOut, "nothing waited on the held delete")
  }

  /// A status read that returns after a revoke leaves the receipt out of
  /// memory, so a `complete` that passed the gate before the delete
  /// committed does not find it there and verify the files the revoke
  /// missed. An announce like that does not put a receipt in memory either.
  @Test(.timeLimit(.minutes(1)))
  func aRequestThatReadItsReceiptBeforeARevokeLeavesItOutOfMemory() async throws {
    let restarted = try await Restarted()
    defer { restarted.remove() }
    let gate = restarted.gate
    let engine = restarted.service.engine

    gate.receiptRead.arm()
    let reading = Task { await restarted.phone.status(restarted.id) }
    await gate.receiptRead.held()
    try await restarted.service.revoke(restarted.paired.id)
    gate.receiptRead.release()

    #expect(await reading.value.code == 200, "it read before the revoke")
    #expect(await engine.receiptsSnapshot.isEmpty, "the revoked phone's receipt stays out")
    #expect(await restarted.phone.complete(restarted.id).code == 404)
    #expect(await restarted.intake.admissions.count == 0, "the intake never sees the file")
    #expect(try await restarted.phone.announce(restarted.metadata).code == 500)
    #expect(await engine.receiptsSnapshot.isEmpty, "a failed announce leaves nothing")
    #expect(!gate.timedOut, "nothing waited on the held read")
  }

  /// A recording admitted before the restart answers its meeting also when
  /// a revoke lands during the read: a 401 would make the phone keep it, and
  /// its upload after pairing again would become a second meeting.
  @Test(.timeLimit(.minutes(1)))
  func aCompletedRecordingAnswersItsMeetingAfterARevokeDuringTheRead() async throws {
    let restarted = try await Restarted(completed: true)
    defer { restarted.remove() }
    let gate = restarted.gate

    gate.receiptRead.arm()
    let completing = Task { await restarted.phone.complete(restarted.id) }
    await gate.receiptRead.held()
    try await restarted.service.revoke(restarted.paired.id)
    gate.receiptRead.release()
    let response = await completing.value

    #expect(response.code == 200)
    let meetingID = try #require(restarted.meetingID)
    #expect(try response.json(Wire.CompleteResponse.self).meetingID == meetingID)
    #expect(await restarted.intake.admissions.count == 0, "nothing is admitted again")
    #expect(await restarted.service.engine.receiptsSnapshot.isEmpty)
    #expect(!gate.timedOut, "nothing waited on the held read")
  }

  /// A revoke during the `.verifying` save discards the files, but the
  /// phone, still passing the gate before the delete commits, announces and
  /// sends the chunks again. The first chunk's save is still held, so the
  /// store lists no chunk and both are written: a complete partial is back
  /// for the verify. Only the check after the verify keeps it from the
  /// intake.
  @Test(.timeLimit(.minutes(1)))
  func aRevokeDuringTheVerifyingSaveWhoseFilesCameBackAdmitsNothing() async throws {
    let gate = try StoreGate()
    defer { gate.remove() }
    let chunkSize = 64 * 1024
    let test = try TestService.prepare(chunkSize: chunkSize, store: gate.store)
    defer { try? FileManager.default.removeItem(at: test.directory) }
    let engine = test.service.engine
    let phone = try await EngineClient.paired(test)
    let bytes = Phone.seededBytes(count: 2 * chunkSize, seed: 5)
    let metadata = phone.metadata(for: bytes, chunkSize: chunkSize)
    let chunks = Phone.chunks(of: bytes, size: chunkSize)
    let id = metadata.recordingID
    #expect(try await phone.announce(metadata).code == 201)

    // The first chunk's save holds the writer; every later write queues.
    gate.receiptWrite.arm()
    let first = Task { await phone.upload(id, chunk: 0, chunks[0]) }
    await gate.receiptWrite.held()
    let second = Task { await phone.upload(id, chunk: 1, chunks[1]) }
    try await Self.until { await engine.activeReceipts[id]?.receivedChunks == [0, 1] }
    let completing = Task { await phone.complete(id) }
    try await Self.until { await engine.activeReceipts[id]?.state.kind == .verifying }
    let revoking = Task { try await test.service.revoke(phone.device.id) }
    try await Self.until { await engine.revoking[phone.device.id] != nil }
    #expect(!engine.inbox.hasPartial(id), "the revoke discarded the partial")

    let announcing = Task { try await phone.announce(metadata) }
    try await Self.until { engine.inbox.loadMetadata(id) != nil }
    for (index, chunk) in chunks.enumerated() {
      // Written, then answered 404: the receipt stays out of memory.
      #expect(await phone.upload(id, chunk: index, chunk).code == 404)
    }
    #expect(engine.inbox.hasPartial(id), "the partial is back")
    gate.receiptWrite.release()

    #expect(await completing.value.code == 401, "the phone learns it was unpaired")
    try await revoking.value
    #expect(try await announcing.value.code == 500, "its save fails on the deleted device")
    #expect(await first.value.code == 204)
    #expect(await second.value.code == 204)
    #expect(await test.intake.admissions.count == 0, "the intake never sees the file")
    #expect(
      !engine.inbox.hasPartial(id) && !engine.inbox.hasVerified(id, format: metadata.format)
        && engine.inbox.loadMetadata(id) == nil, "its files are gone")
    #expect(await engine.receiptsSnapshot.isEmpty)
    #expect(try await test.store.handoverReceipt(recordingID: id) == nil)
    #expect(!gate.timedOut, "nothing waited on the held save")
  }

  /// A revoke whose store delete throws leaves the device paired and no
  /// trace of the revoke; only the files of its upload are gone, so the
  /// phone announces again and uploads anew.
  @Test(.timeLimit(.minutes(1)))
  func aFailedRevokeLeavesNoRevokeInFlight() async throws {
    let gate = try StoreGate()
    defer { gate.remove() }
    let chunkSize = 64 * 1024
    let test = try TestService.prepare(chunkSize: chunkSize, store: gate.store)
    defer { try? FileManager.default.removeItem(at: test.directory) }
    let engine = test.service.engine
    let phone = try await EngineClient.paired(test)
    let deviceID = phone.device.id
    let bytes = Phone.seededBytes(count: 2 * chunkSize, seed: 6)
    let metadata = phone.metadata(for: bytes, chunkSize: chunkSize)
    let id = metadata.recordingID
    try await phone.uploadAll(metadata, bytes)
    let revocations = await engine.revocations[deviceID, default: 0]

    try await gate.pool.write { db in
      try db.execute(
        sql: """
          CREATE TRIGGER keepDevice BEFORE DELETE ON pairedDevice
          BEGIN SELECT RAISE(ABORT, 'kept'); END
          """)
    }
    await #expect(throws: (any Error).self) { try await test.service.revoke(deviceID) }
    try await gate.pool.write { db in try db.execute(sql: "DROP TRIGGER keepDevice") }

    #expect(try await test.store.pairedDevice(id: deviceID) != nil, "the device is still paired")
    #expect(await engine.revoking.isEmpty, "no revoke is in flight")
    #expect(await !engine.revoked.contains(deviceID))
    #expect(await engine.revocations[deviceID, default: 0] == revocations)
    #expect(await phone.status(id).code == 200)
    #expect(await engine.activeReceipts[id] != nil, "the status read is in memory again")
    try await phone.uploadAll(metadata, bytes)
    #expect(await phone.complete(id).code == 200)
    #expect(await test.intake.admissions.count == 1)
  }
}
