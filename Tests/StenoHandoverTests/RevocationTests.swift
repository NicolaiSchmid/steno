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

  /// The Mac comes back over the store and inbox of a phone that uploaded
  /// every chunk: after a restart its receipt is only in the store.
  private struct Restarted {
    let gate: StoreGate
    let test: TestService
    /// The phone's view before the restart.
    let phone: EngineDevice
    let bytes: Data
    let metadata: RecordingMetadata
    let service: HandoverService
    let intake: FakeHandoverIntake
    /// The same phone's view of the restarted engine.
    let device: EngineDevice

    init() async throws {
      let chunkSize = 64 * 1024
      gate = try StoreGate()
      test = try TestService.prepare(chunkSize: chunkSize, store: gate.store)
      phone = try await EngineClient.paired(test)
      bytes = Phone.seededBytes(count: 2 * chunkSize, seed: 99)
      metadata = phone.metadata(for: bytes, chunkSize: chunkSize)
      try await phone.uploadAll(metadata, bytes)
      intake = FakeHandoverIntake()
      let now = test.now
      service = HandoverService(
        configuration: test.service.configuration, store: test.store, intake: intake,
        identity: test.service.identity, now: { now })
      device = EngineDevice(engine: service.engine, device: phone.device)
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
    let phone = restarted.phone.device

    gate.receiptRead.arm()
    let completing = Task { await restarted.device.complete(restarted.id) }
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
    let paired = try await restarted.test.store.pairedDevice(id: phone.id)
    #expect((paired != nil) == pairsAgain, "only pairing again brings the device back")
    if let paired {
      // The new pairing uploads the recording again, and it goes through.
      let again = EngineDevice(engine: restarted.service.engine, device: paired)
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
    let readBefore = Task { await restarted.device.complete(restarted.id) }
    await gate.receiptRead.held()
    gate.deviceDelete.arm()
    let revoking = Task { try await restarted.service.revoke(restarted.phone.device.id) }
    await gate.deviceDelete.held()

    let startedDuring = await restarted.device.complete(restarted.id)
    #expect(startedDuring.code == 401, "a complete during the delete is refused")
    gate.receiptRead.release()
    #expect(await readBefore.value.code == 401, "a complete that read before it is refused")
    gate.deviceDelete.release()
    try await revoking.value

    try await restarted.expectNothingAdmitted()
    #expect(!gate.timedOut, "nothing waited on the held delete")
  }
}
