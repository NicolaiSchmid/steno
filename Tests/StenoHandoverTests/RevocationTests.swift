import Foundation
import StenoCore
import Testing

@testable import StenoHandover

/// Revoking a phone mid-upload: its partial and receipt are gone, every
/// bearer route answers 401 at the gate (the phone's "unpaired" signal), and
/// another phone's upload is untouched. Both ways in: `revoke(_:)` from the
/// Mac and `DELETE /v1/pairing` from the phone.
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
}
