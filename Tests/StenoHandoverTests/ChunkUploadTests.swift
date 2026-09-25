import Foundation
import StenoCore
import Testing

@testable import StenoHandover

/// M4: 1 MiB chunks, a little over 3 MiB of seeded random bytes, a mid-chunk
/// disconnect, resume from `GET` status, a duplicate chunk, then complete;
/// bytes on disk equal the source. A hash mismatch is 422 and the partial is
/// gone.
@Suite struct ChunkUploadTests {
  static let chunkSize = 1024 * 1024
  static let meetingID = UUID(uuidString: "0EE71E00-0000-4000-8000-00000000C0DE")!

  @Test func uploadSurvivesDisconnectResumesSkipsDuplicatesAndCompletes() async throws {
    let intake = FakeHandoverIntake(meetingID: Self.meetingID)
    let test = try await TestService.start(chunkSize: Self.chunkSize, intake: intake)
    defer { Task { await test.stop() } }
    let phone = try await Phone.pair(test)
    let bytes = Phone.seededBytes(count: 3 * Self.chunkSize + 12345, seed: 42)
    let metadata = phone.metadata(for: bytes)
    let chunks = Phone.chunks(of: bytes, size: Self.chunkSize)
    #expect(chunks.count == 4)
    #expect(chunks.last?.count == 12345)

    let announced = try await phone.announce(metadata)
    #expect(announced.status == 201)
    #expect(
      try announced.json(Wire.RecordingStatus.self)
        == Wire.RecordingStatus(state: .receiving, receivedChunks: []))

    #expect(try await phone.upload(metadata.recordingID, chunk: 0, chunks[0]).status == 204)

    // Chunk 1 breaks off after 100 KiB: head with the full length, part of
    // the body, then the connection goes away.
    let raw = try await test.rawClient()
    let torn = RawClient.request(
      "PUT", "/v1/recordings/\(metadata.recordingID.uuidString)/chunks/1",
      headers: [
        ("Authorization", "Bearer \(phone.token)"),
        ("Content-Type", "application/octet-stream"),
        (Wire.chunkHashHeader, ReceivingFile.sha256(chunks[1]).base64EncodedString()),
        ("Content-Length", String(chunks[1].count)),
      ],
      body: chunks[1].prefix(100 * 1024))
    let interrupted = try await raw.exchange(
      torn, closeGrace: .milliseconds(50), timeout: .milliseconds(300))
    #expect(interrupted.status == nil, "nothing is answered for a torn chunk")

    // Resume from the status the Mac reports.
    let status = try await phone.status(metadata.recordingID)
    #expect(status.status == 200)
    let resume = try status.json(Wire.RecordingStatus.self)
    #expect(resume == Wire.RecordingStatus(state: .receiving, receivedChunks: [0]))
    for index in (0..<chunks.count) where !resume.receivedChunks.contains(index) {
      #expect(
        try await phone.upload(metadata.recordingID, chunk: index, chunks[index]).status == 204)
    }
    #expect(
      try await phone.upload(metadata.recordingID, chunk: 1, chunks[1]).status == 204,
      "a duplicate chunk is 204")
    #expect(
      try await phone.status(metadata.recordingID).json(Wire.RecordingStatus.self).receivedChunks
        == [0, 1, 2, 3])

    let completed = try await phone.complete(metadata.recordingID)
    #expect(completed.status == 200)
    #expect(try completed.json(Wire.CompleteResponse.self).meetingID == Self.meetingID)

    let admissions = await intake.admissions.entries
    #expect(admissions.count == 1)
    let admission = try #require(admissions.first)
    #expect(admission.metadata == metadata)
    #expect(admission.device.id == phone.deviceID)
    #expect(admission.file.pathExtension == "m4a")
    #expect(try Data(contentsOf: admission.file) == bytes, "bytes on disk equal the source")

    let receipt = try #require(
      try await test.store.handoverReceipt(recordingID: metadata.recordingID))
    #expect(receipt.state == .complete(meetingID: Self.meetingID))
    #expect(receipt.deviceID == phone.deviceID)
    #expect(receipt.byteCount == Int64(bytes.count))
    #expect(
      !FileManager.default.fileExists(
        atPath: test.service.engine.inbox.partial(metadata.recordingID).path))
    #expect(
      !FileManager.default.fileExists(
        atPath: test.service.engine.inbox.metadata(metadata.recordingID).path))
  }

  @Test func hashMismatchIs422AndThePartialIsGone() async throws {
    let test = try await TestService.start(chunkSize: Self.chunkSize)
    defer { Task { await test.stop() } }
    let phone = try await Phone.pair(test)
    let bytes = Phone.seededBytes(count: 2 * Self.chunkSize, seed: 7)
    let wrongHash = ReceivingFile.sha256(Data("something else".utf8))
    let metadata = phone.metadata(for: bytes, sha256: wrongHash)

    try await phone.uploadAll(metadata, bytes)
    let inbox = test.service.engine.inbox
    #expect(FileManager.default.fileExists(atPath: inbox.partial(metadata.recordingID).path))

    let completed = try await phone.complete(metadata.recordingID)
    #expect(completed.status == 422)
    #expect(!FileManager.default.fileExists(atPath: inbox.partial(metadata.recordingID).path))
    #expect(await test.intake.admissions.count == 0)

    let status = try await phone.status(metadata.recordingID).json(Wire.RecordingStatus.self)
    #expect(status == Wire.RecordingStatus(state: .failed, receivedChunks: []))
    let receipt = try #require(
      try await test.store.handoverReceipt(recordingID: metadata.recordingID))
    #expect(receipt.state == .failed("sha256 mismatch"))

    // The phone starts over: re-announcing the same id (same metadata) after
    // a failure is a resume, 200 with no chunks.
    let again = try await phone.announce(
      phone.metadata(for: bytes, recordingID: metadata.recordingID, sha256: wrongHash))
    #expect(again.status == 200)
    #expect(try again.json(Wire.RecordingStatus.self).receivedChunks.isEmpty)
    let fresh = phone.metadata(for: bytes)
    try await phone.uploadAll(fresh, bytes)
    #expect(try await phone.complete(fresh.recordingID).status == 200)
  }

  @Test func completeWithMissingChunksIs409WithTheStatus() async throws {
    let test = try await TestService.start(chunkSize: Self.chunkSize)
    defer { Task { await test.stop() } }
    let phone = try await Phone.pair(test)
    let bytes = Phone.seededBytes(count: 2 * Self.chunkSize + 1, seed: 9)
    let metadata = phone.metadata(for: bytes)
    let chunks = Phone.chunks(of: bytes, size: Self.chunkSize)

    #expect(try await phone.announce(metadata).status == 201)
    #expect(try await phone.upload(metadata.recordingID, chunk: 2, chunks[2]).status == 204)
    let early = try await phone.complete(metadata.recordingID)
    #expect(early.status == 409)
    #expect(
      try early.json(Wire.RecordingStatus.self)
        == Wire.RecordingStatus(state: .receiving, receivedChunks: [2]))
    #expect(await test.intake.admissions.count == 0)
  }

  @Test func chunkAndMetadataErrorsAreAnsweredWithoutSideEffects() async throws {
    let test = try await TestService.start(chunkSize: Self.chunkSize)
    defer { Task { await test.stop() } }
    let phone = try await Phone.pair(test)
    let other = try await Phone.pair(test, deviceName: "Other phone")
    let bytes = Phone.seededBytes(count: Self.chunkSize + 10, seed: 3)
    let metadata = phone.metadata(for: bytes)
    let chunks = Phone.chunks(of: bytes, size: Self.chunkSize)

    // Announce problems.
    #expect(try await phone.status(metadata.recordingID).status == 404)
    let mismatch = try await phone.client.json(
      "PUT", "/v1/recordings/\(UUID().uuidString)", headers: phone.bearer, body: metadata)
    #expect(mismatch.status == 400)
    #expect(
      try await phone.announce(phone.metadata(for: bytes, chunkSize: Self.chunkSize * 2)).status
        == 400, "chunkSize above the Mac's limit")
    #expect(
      try await phone.announce(phone.metadata(for: bytes, format: .caf48kFloat32)).status == 400)
    let garbage = try await phone.client.request(
      "PUT", "/v1/recordings/\(metadata.recordingID.uuidString)", headers: phone.bearer,
      body: Data("{".utf8))
    #expect(garbage.status == 400)
    #expect(try await phone.announce(metadata).status == 201)
    #expect(try await phone.announce(metadata).status == 200, "announcing twice is fine")
    #expect(
      try await phone.announce(
        phone.metadata(for: bytes + Data([1]), recordingID: metadata.recordingID)
      )
      .status == 409, "different metadata under the same id")
    #expect(try await other.announce(metadata).status == 409, "another device's id")
    #expect(try await other.status(metadata.recordingID).status == 404)
    #expect(try await other.upload(metadata.recordingID, chunk: 0, chunks[0]).status == 404)
    #expect(try await other.complete(metadata.recordingID).status == 404)

    // Chunk problems.
    #expect(try await phone.upload(UUID(), chunk: 0, chunks[0]).status == 404)
    #expect(try await phone.upload(metadata.recordingID, chunk: 2, chunks[1]).status == 400)
    #expect(try await phone.upload(metadata.recordingID, chunk: 0, chunks[1]).status == 400)
    #expect(try await phone.upload(metadata.recordingID, chunk: 1, chunks[0]).status == 400)
    #expect(
      try await phone.upload(metadata.recordingID, chunk: 0, chunks[0], hashHeader: false).status
        == 400)
    #expect(
      try await phone.upload(
        metadata.recordingID, chunk: 0, chunks[0], declaredHash: Data(repeating: 1, count: 32)
      ).status == 422)
    #expect(
      try await phone.upload(
        metadata.recordingID, chunk: 0, chunks[0], declaredHash: Data(repeating: 1, count: 31)
      ).status == 400)
    #expect(
      try await phone.status(metadata.recordingID).json(Wire.RecordingStatus.self).receivedChunks
        .isEmpty, "nothing was recorded")

    #expect(try await phone.upload(metadata.recordingID, chunk: 0, chunks[0]).status == 204)
    #expect(try await phone.upload(metadata.recordingID, chunk: 1, chunks[1]).status == 204)
    #expect(try await phone.complete(metadata.recordingID).status == 200)
    #expect(try await phone.complete(metadata.recordingID).status == 200, "idempotent")
    #expect(
      try await phone.upload(metadata.recordingID, chunk: 1, chunks[1]).status == 204,
      "a late duplicate after completion is harmless")
    #expect(await test.intake.admissions.count == 1)
  }

  @Test func chunkArithmeticCoversTheShortLastChunk() {
    #expect(MetadataValidation.chunkCount(byteCount: 1, chunkSize: 10) == 1)
    #expect(MetadataValidation.chunkCount(byteCount: 10, chunkSize: 10) == 1)
    #expect(MetadataValidation.chunkCount(byteCount: 11, chunkSize: 10) == 2)
    #expect(MetadataValidation.chunkLength(index: 0, byteCount: 11, chunkSize: 10) == 10)
    #expect(MetadataValidation.chunkLength(index: 1, byteCount: 11, chunkSize: 10) == 1)
    #expect(MetadataValidation.chunkLength(index: 0, byteCount: 10, chunkSize: 10) == 10)
  }
}
