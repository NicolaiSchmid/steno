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
    try await TestService.run(chunkSize: Self.chunkSize, intake: intake) { test in
      let phone = try await Phone.pair(test.service)
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
      let raw = try test.rawClient()
      let interrupted = try await raw.exchange(
        .PUT, "/v1/recordings/\(metadata.recordingID.uuidString)/chunks/1",
        headers: [
          ("Authorization", "Bearer \(phone.token)"),
          ("Content-Type", "application/octet-stream"),
          (Wire.chunkHashHeader, ContentHash.sha256(chunks[1]).base64EncodedString()),
          ("Content-Length", String(chunks[1].count)),
        ],
        body: chunks[1].prefix(100 * 1024), closeGrace: .milliseconds(50),
        timeout: .milliseconds(300))
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
  }

  @Test func hashMismatchIs422AndThePartialIsGone() async throws {
    try await TestService.run(chunkSize: Self.chunkSize) { test in
      let phone = try await Phone.pair(test.service)
      let bytes = Phone.seededBytes(count: 2 * Self.chunkSize, seed: 7)
      let wrongHash = ContentHash.sha256(Data("something else".utf8))
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
      let chunks = Phone.chunks(of: bytes, size: Self.chunkSize)
      let late = try await phone.upload(metadata.recordingID, chunk: 0, chunks[0])
      #expect(late.status == 404, "a chunk for the discarded partial asks for a new announce")
      #expect(try late.json(Wire.Problem.self).error.contains("announce again"))

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
  }

  @Test func completeWithMissingChunksIs409WithTheStatus() async throws {
    try await TestService.run(chunkSize: Self.chunkSize) { test in
      let phone = try await Phone.pair(test.service)
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
  }

  @Test func chunkAndMetadataErrorsAreAnsweredWithoutSideEffects() async throws {
    try await TestService.run(chunkSize: Self.chunkSize) { test in
      let phone = try await Phone.pair(test.service)
      let other = try await Phone.pair(test.service, deviceName: "Other phone")
      let bytes = Phone.seededBytes(count: Self.chunkSize + 10, seed: 3)
      let metadata = phone.metadata(for: bytes)
      let chunks = Phone.chunks(of: bytes, size: Self.chunkSize)

      // Announce problems.
      #expect(try await phone.status(metadata.recordingID).status == 404, "status before announce")
      let mismatch = try await phone.client.json(
        "PUT", "/v1/recordings/\(UUID().uuidString)", headers: phone.bearer, body: metadata)
      #expect(mismatch.status == 400, "recordingID differs from the path")
      #expect(
        try await phone.announce(phone.metadata(for: bytes, chunkSize: Self.chunkSize * 2)).status
          == 400, "chunkSize above the Mac's limit")
      #expect(
        try await phone.announce(phone.metadata(for: bytes, format: .caf48kFloat32)).status == 400)
      let garbage = try await phone.client.request(
        "PUT", "/v1/recordings/\(metadata.recordingID.uuidString)", headers: phone.bearer,
        body: Data("{".utf8))
      #expect(garbage.status == 400, "unparseable metadata")
      #expect(try await phone.announce(metadata).status == 201)
      #expect(try await phone.announce(metadata).status == 200, "announcing twice is fine")
      #expect(
        try await phone.announce(
          phone.metadata(for: bytes + Data([1]), recordingID: metadata.recordingID)
        )
        .status == 409, "different metadata under the same id")
      #expect(try await other.announce(metadata).status == 409, "another device's id")
      #expect(try await other.status(metadata.recordingID).status == 404, "another device's status")
      #expect(
        try await other.upload(metadata.recordingID, chunk: 0, chunks[0]).status == 404,
        "another device's chunk")
      #expect(
        try await other.complete(metadata.recordingID).status == 404, "another device's complete")

      // Chunk problems.
      #expect(
        try await phone.upload(UUID(), chunk: 0, chunks[0]).status == 404, "unknown recording")
      #expect(
        try await phone.upload(metadata.recordingID, chunk: 2, chunks[1]).status == 400,
        "index beyond the chunk count")
      #expect(
        try await phone.upload(metadata.recordingID, chunk: 0, chunks[1]).status == 400,
        "the short last chunk sent as chunk 0")
      #expect(
        try await phone.upload(metadata.recordingID, chunk: 1, chunks[0]).status == 400,
        "a full chunk sent as the short last one")
      #expect(
        try await phone.upload(metadata.recordingID, chunk: 0, chunks[0], hashHeader: false).status
          == 400, "missing hash header")
      #expect(
        try await phone.upload(
          metadata.recordingID, chunk: 0, chunks[0], declaredHash: Data(repeating: 1, count: 32)
        ).status == 422, "hash header disagrees with the body")
      #expect(
        try await phone.upload(
          metadata.recordingID, chunk: 0, chunks[0], declaredHash: Data(repeating: 1, count: 31)
        ).status == 400, "hash header of the wrong length")
      #expect(
        try await phone.status(metadata.recordingID).json(Wire.RecordingStatus.self).receivedChunks
          .isEmpty, "nothing was recorded")

      #expect(try await phone.upload(metadata.recordingID, chunk: 0, chunks[0]).status == 204)
      #expect(try await phone.upload(metadata.recordingID, chunk: 1, chunks[1]).status == 204)
      let completed = try await phone.complete(metadata.recordingID)
      #expect(completed.status == 200)
      let repeated = try await phone.complete(metadata.recordingID)
      #expect(repeated.status == 200)
      #expect(
        try repeated.json(Wire.CompleteResponse.self).meetingID
          == completed.json(Wire.CompleteResponse.self).meetingID,
        "completing twice returns the same meeting id")
      #expect(
        try await phone.upload(metadata.recordingID, chunk: 1, chunks[1]).status == 204,
        "a late duplicate after completion is harmless")
      #expect(await test.intake.admissions.count == 1)
    }
  }

  @Test func chunksInFlightAtOnceAllLandInTheReceipt() async throws {
    // The phone keeps two background tasks going, and after a relaunch the
    // background session may deliver several at once. The engine is an actor
    // that suspends while it saves a receipt, so every concurrent chunk must
    // survive into the same receipt: no lost update, in memory or in the store.
    let intake = FakeHandoverIntake(meetingID: Self.meetingID)
    let chunkSize = 64 * 1024
    try await TestService.run(chunkSize: chunkSize, intake: intake) { test in
      let phone = try await Phone.pair(test.service)
      let bytes = Phone.seededBytes(count: 8 * chunkSize - 77, seed: 5)
      let metadata = phone.metadata(for: bytes, chunkSize: chunkSize)
      let chunks = Phone.chunks(of: bytes, size: chunkSize)
      #expect(chunks.count == 8)
      #expect(try await phone.announce(metadata).status == 201)

      let statuses = try await withThrowingTaskGroup(of: Int.self) { group in
        for (index, chunk) in chunks.enumerated().reversed() {
          group.addTask { try await phone.upload(metadata.recordingID, chunk: index, chunk).status }
        }
        return try await group.reduce(into: [Int]()) { $0.append($1) }
      }
      #expect(statuses == Array(repeating: 204, count: 8))
      #expect(
        try await phone.status(metadata.recordingID).json(Wire.RecordingStatus.self)
          == Wire.RecordingStatus(state: .receiving, receivedChunks: Array(0..<8)))
      let receipt = try #require(
        try await test.store.handoverReceipt(recordingID: metadata.recordingID))
      #expect(receipt.receivedChunks == Array(0..<8), "the stored copy lost nothing either")
      #expect(try await phone.complete(metadata.recordingID).status == 200)
      let admission = try #require(await intake.admissions.entries.first)
      #expect(try Data(contentsOf: admission.file) == bytes, "out-of-order chunks land in place")
    }
  }

  @Test func announceAfterCompleteReportsCompleteWithEveryChunk() async throws {
    // The phone's retry after a lost 200 re-announces (`upload-executor.test.ts`,
    // "retry re-announces and completes without re-uploading anything").
    let intake = FakeHandoverIntake(meetingID: Self.meetingID)
    try await TestService.run(chunkSize: Self.chunkSize, intake: intake) { test in
      let phone = try await Phone.pair(test.service)
      let bytes = Phone.seededBytes(count: 2 * Self.chunkSize + 1, seed: 6)
      let metadata = phone.metadata(for: bytes)
      try await phone.uploadAll(metadata, bytes)
      #expect(try await phone.complete(metadata.recordingID).status == 200)

      let again = try await phone.announce(metadata)
      #expect(again.status == 200)
      #expect(
        try again.json(Wire.RecordingStatus.self)
          == Wire.RecordingStatus(state: .complete, receivedChunks: [0, 1, 2]))
      let repeated = try await phone.complete(metadata.recordingID)
      #expect(repeated.status == 200)
      #expect(try repeated.json(Wire.CompleteResponse.self).meetingID == Self.meetingID)
      #expect(await intake.admissions.count == 1, "no second admission")
      #expect(
        !test.service.engine.inbox.hasPartial(metadata.recordingID), "no partial is reopened")
    }
  }

  @Test func aVanishedPartialIs404OnChunkAndAReAnnounceStartsOver() async throws {
    // The phone's executor answers a 404 on a chunk by re-announcing with an
    // empty chunk set ("The Mac forgot the upload; starting over").
    try await TestService.run(chunkSize: Self.chunkSize) { test in
      let phone = try await Phone.pair(test.service)
      let bytes = Phone.seededBytes(count: 2 * Self.chunkSize, seed: 8)
      let metadata = phone.metadata(for: bytes)
      let chunks = Phone.chunks(of: bytes, size: Self.chunkSize)
      let inbox = test.service.engine.inbox
      #expect(try await phone.announce(metadata).status == 201)
      #expect(try await phone.upload(metadata.recordingID, chunk: 0, chunks[0]).status == 204)

      try FileManager.default.removeItem(at: inbox.partial(metadata.recordingID))
      let lost = try await phone.upload(metadata.recordingID, chunk: 1, chunks[1])
      #expect(lost.status == 404)
      #expect(try lost.json(Wire.Problem.self).error.contains("announce again"))
      #expect(
        try await phone.upload(metadata.recordingID, chunk: 0, chunks[0]).status == 204,
        "a chunk already recorded is still a harmless duplicate")

      let again = try await phone.announce(metadata)
      #expect(again.status == 200)
      #expect(
        try again.json(Wire.RecordingStatus.self)
          == Wire.RecordingStatus(state: .receiving, receivedChunks: []))
      #expect(inbox.hasPartial(metadata.recordingID))
      try await phone.uploadAll(metadata, bytes)
      #expect(try await phone.complete(metadata.recordingID).status == 200)
      let admission = try #require(await test.intake.admissions.entries.first)
      #expect(try Data(contentsOf: admission.file) == bytes)
    }
  }

  @Test func aFailedWriteIs500WithoutTheInboxPath() async throws {
    // A `FileHandle` error names the file, and the inbox lives under the
    // user's home; the phone gets the step that failed and nothing else.
    try await TestService.run(chunkSize: Self.chunkSize) { test in
      let phone = try await Phone.pair(test.service)
      let bytes = Phone.seededBytes(count: Self.chunkSize, seed: 10)
      let metadata = phone.metadata(for: bytes)
      let inbox = test.service.engine.inbox
      #expect(try await phone.announce(metadata).status == 201)
      // The partial becomes a directory: every write to it fails.
      try FileManager.default.removeItem(at: inbox.partial(metadata.recordingID))
      try FileManager.default.createDirectory(
        at: inbox.partial(metadata.recordingID), withIntermediateDirectories: false)

      let failed = try await phone.upload(metadata.recordingID, chunk: 0, bytes)
      #expect(failed.status == 500)
      let problem = try failed.json(Wire.Problem.self).error
      #expect(problem == "writing the chunk failed on the Mac")
      #expect(!problem.contains(inbox.directory.path))
      #expect(
        try await phone.status(metadata.recordingID).json(Wire.RecordingStatus.self).receivedChunks
          .isEmpty, "the failed chunk is not recorded")
    }
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
