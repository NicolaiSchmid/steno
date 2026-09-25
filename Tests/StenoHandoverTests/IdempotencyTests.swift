import Foundation
import StenoCore
import Testing

@testable import StenoHandover

/// M5: the receipt stream reaches `.complete`, and a start sweeps orphaned
/// inbox files. Completing twice is covered by `ChunkUploadTests` (fake
/// intake) and the end-to-end test (real intake).
@Suite struct IdempotencyTests {
  static let chunkSize = 1024 * 1024

  @Test func receiptsStreamReachesComplete() async throws {
    let meetingID = UUID(uuidString: "1DEA0000-0000-4000-8000-000000000002")!
    let test = try await TestService.start(
      chunkSize: Self.chunkSize, intake: FakeHandoverIntake(meetingID: meetingID))
    defer { Task { await test.stop() } }
    let receipts = await test.service.receipts
    let phone = try await Phone.pair(test.service)
    let bytes = Phone.seededBytes(count: 2 * Self.chunkSize, seed: 21)
    let metadata = phone.metadata(for: bytes)

    let collector = Task { () -> HandoverReceipt? in
      for await batch in receipts {
        if let receipt = batch.first(where: { $0.recordingID == metadata.recordingID }),
          receipt.state.kind == .complete
        {
          return receipt
        }
      }
      return nil
    }
    try await phone.uploadAll(metadata, bytes)
    #expect(try await phone.complete(metadata.recordingID).status == 200)

    let receipt = await collector.value
    #expect(receipt?.state == .complete(meetingID: meetingID))
    #expect(receipt?.receivedChunks == [0, 1])
  }

  @Test func startSweepsOrphanedInboxFiles() async throws {
    let test = try TestService.prepare(chunkSize: Self.chunkSize)
    defer { Task { await test.stop() } }
    // An announced-but-abandoned recording with no receipt in the store.
    let inbox = test.service.engine.inbox
    let orphan = UUID()
    try inbox.begin(
      RecordingMetadata(
        recordingID: orphan, startedAt: Date(timeIntervalSince1970: 1_789_000_000),
        durationSeconds: 10, byteCount: 1000, sha256: Data(repeating: 0, count: 32),
        chunkSize: Self.chunkSize, format: .m4aAAC, deviceName: "Ghost"))
    #expect(inbox.hasPartial(orphan))

    try await test.service.start()
    #expect(!inbox.hasPartial(orphan), "the orphan partial is swept")
    #expect(inbox.loadMetadata(orphan) == nil)
  }
}
