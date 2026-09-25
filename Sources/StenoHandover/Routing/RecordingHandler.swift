import Foundation
import NIOHTTP1
import StenoCore

/// The four recording routes: announce, status, chunk, complete. Every path
/// is idempotent on the recording id, so a phone that lost the answer can
/// simply repeat the call.
extension HandoverEngine {
  /// `PUT /v1/recordings/{id}` with `RecordingMetadata`: 201 for a new
  /// recording, 200 for a known one, both with `RecordingStatus`.
  func announce(_ recordingID: UUID, _ request: HandoverRequest) async -> HandoverResponse {
    guard let device = request.device else { return .problem(.unauthorized, "no device") }
    let metadata: RecordingMetadata
    do {
      metadata = try StenoJSON.decode(RecordingMetadata.self, from: request.body)
    } catch {
      return .problem(.badRequest, "RecordingMetadata: \(error)")
    }
    guard metadata.recordingID == recordingID else {
      return .problem(.badRequest, "recordingID does not match the path")
    }
    if let problem = MetadataValidation.problem(with: metadata, configuration: configuration) {
      return .problem(.badRequest, problem)
    }

    if let existing = await receipt(recordingID) {
      guard existing.deviceID == device.id else {
        return .problem(.conflict, "another device owns this recording")
      }
      if existing.state.kind == .complete {
        return .json(.ok, Self.status(of: existing))
      }
      guard existing.byteCount == metadata.byteCount, existing.sha256 == metadata.sha256,
        existing.chunkSize == metadata.chunkSize
      else {
        return .problem(.conflict, "metadata differs from the first announcement")
      }
      var receipt = existing
      if !inbox.hasPartial(recordingID) || inbox.loadMetadata(recordingID) == nil {
        // The partial is gone (a sweep, a crash before the first chunk):
        // start over with the same receipt.
        do {
          try inbox.begin(metadata)
        } catch {
          return .problem(.internalServerError, "inbox: \(error)")
        }
        receipt.receivedChunks = []
      }
      receipt.state = .receiving
      receipt.updatedAt = now()
      do {
        try await persist(receipt)
      } catch {
        return .problem(.internalServerError, "receipt: \(error)")
      }
      return .json(.ok, Self.status(of: receipt))
    }

    do {
      try inbox.begin(metadata)
    } catch {
      return .problem(.internalServerError, "inbox: \(error)")
    }
    let timestamp = now()
    let receipt = HandoverReceipt(
      recordingID: recordingID, deviceID: device.id, state: .receiving,
      byteCount: metadata.byteCount, sha256: metadata.sha256, chunkSize: metadata.chunkSize,
      receivedChunks: [], createdAt: timestamp, updatedAt: timestamp)
    do {
      try await persist(receipt)
    } catch {
      inbox.discard(recordingID, format: metadata.format)
      return .problem(.internalServerError, "receipt: \(error)")
    }
    return .json(.created, Self.status(of: receipt))
  }

  /// `GET /v1/recordings/{id}`: the resume point, 404 for an unknown id.
  func status(_ recordingID: UUID, _ request: HandoverRequest) async -> HandoverResponse {
    guard let receipt = await ownedReceipt(recordingID, request) else {
      return .problem(.notFound, "no such recording")
    }
    return .json(.ok, Self.status(of: receipt))
  }

  /// `PUT /v1/recordings/{id}/chunks/{n}` with raw bytes and
  /// `X-Steno-Chunk-SHA256`: 204, also for a chunk already received.
  func receiveChunk(_ recordingID: UUID, index: Int, _ request: HandoverRequest) async
    -> HandoverResponse
  {
    guard var receipt = await ownedReceipt(recordingID, request) else {
      return .problem(.notFound, "no such recording")
    }
    if receipt.state.kind == .complete {
      return .empty(.noContent)
    }
    let count = MetadataValidation.chunkCount(
      byteCount: receipt.byteCount, chunkSize: receipt.chunkSize)
    guard index < count else {
      return .problem(.badRequest, "chunk index must be below \(count)")
    }
    let expected = MetadataValidation.chunkLength(
      index: index, byteCount: receipt.byteCount, chunkSize: receipt.chunkSize)
    guard request.body.count == expected else {
      return .problem(.badRequest, "chunk \(index) must be \(expected) bytes")
    }
    guard let declared = request.headers[Wire.chunkHashHeader.lowercased()],
      let digest = Data(base64Encoded: declared), digest.count == 32
    else {
      return .problem(.badRequest, "\(Wire.chunkHashHeader) must be the base64 SHA-256 of the body")
    }
    guard ConstantTime.equals(ReceivingFile.sha256(request.body), digest) else {
      return .problem(.unprocessableEntity, "chunk \(index) hash mismatch")
    }
    if receipt.receivedChunks.contains(index) {
      return .empty(.noContent)
    }
    guard inbox.hasPartial(recordingID) else {
      // Announce again: the partial is gone.
      return .problem(.notFound, "no partial file; announce again")
    }
    do {
      try ReceivingFile.write(
        request.body, at: UInt64(index) * UInt64(receipt.chunkSize), to: inbox.partial(recordingID))
    } catch {
      return .problem(.internalServerError, "write: \(error)")
    }
    receipt.receivedChunks = (receipt.receivedChunks + [index]).sorted()
    receipt.state = .receiving
    receipt.updatedAt = now()
    do {
      try await persist(receipt)
    } catch {
      return .problem(.internalServerError, "receipt: \(error)")
    }
    return .empty(.noContent)
  }

  /// `POST /v1/recordings/{id}/complete`: 200 `{meetingID}` once every chunk
  /// is present and the whole file hashes to the announced value; 409 with
  /// the status while chunks are missing; 422 on a hash mismatch, after
  /// which the partial is gone and the phone starts over.
  func complete(_ recordingID: UUID, _ request: HandoverRequest) async -> HandoverResponse {
    guard let device = request.device, var receipt = await ownedReceipt(recordingID, request)
    else {
      return .problem(.notFound, "no such recording")
    }
    if let meetingID = receipt.state.meetingID {
      return .json(.ok, Wire.CompleteResponse(meetingID: meetingID))
    }
    guard let metadata = inbox.loadMetadata(recordingID) else {
      return .problem(.notFound, "no metadata; announce again")
    }

    let file: URL
    if inbox.hasVerified(recordingID, format: metadata.format) {
      // Verified earlier; the intake failed then. Admit again.
      file = inbox.verified(recordingID, format: metadata.format)
    } else {
      let count = MetadataValidation.chunkCount(
        byteCount: receipt.byteCount, chunkSize: receipt.chunkSize)
      guard receipt.receivedChunks == Array(0..<count), inbox.hasPartial(recordingID) else {
        if !inbox.hasPartial(recordingID) {
          receipt.receivedChunks = []
          receipt.updatedAt = now()
          try? await persist(receipt)
        }
        return .json(.conflict, Self.status(of: receipt))
      }
      receipt.state = .verifying
      receipt.updatedAt = now()
      try? await persist(receipt)

      let partial = inbox.partial(recordingID)
      let verified: Bool
      do {
        verified =
          try ReceivingFile.size(of: partial) == receipt.byteCount
          && ConstantTime.equals(try ReceivingFile.sha256(of: partial), receipt.sha256)
      } catch {
        return .problem(.internalServerError, "verify: \(error)")
      }
      guard verified else {
        inbox.discard(recordingID, format: metadata.format)
        receipt.state = .failed("sha256 mismatch")
        receipt.receivedChunks = []
        receipt.updatedAt = now()
        try? await persist(receipt)
        return .problem(.unprocessableEntity, "sha256 mismatch; the partial was discarded")
      }
      do {
        file = try inbox.promote(recordingID, format: metadata.format)
      } catch {
        return .problem(.internalServerError, "promote: \(error)")
      }
    }

    do {
      let meetingID = try await intake.admit(file: file, metadata: metadata, device: device)
      // The intake deletes the verified file and writes `.complete`; mirror
      // that receipt for the stream, or write `.complete` ourselves when a
      // test intake did not. Keep the metadata so a replayed complete still
      // returns the same meeting id through the early `.complete` check.
      if let stored = try? await store.handoverReceipt(recordingID: recordingID),
        stored.state.kind == .complete
      {
        receipt = stored
      } else {
        receipt.state = .complete(meetingID: meetingID)
      }
      receipt.updatedAt = now()
      try await persist(receipt)
      // The intake owns the verified file (it copies then deletes it); the
      // metadata sidecar is ours to remove. A replayed complete returns the
      // same id through the early `.complete` check, no metadata needed.
      try? FileManager.default.removeItem(at: inbox.metadata(recordingID))
      return .json(.ok, Wire.CompleteResponse(meetingID: meetingID))
    } catch {
      // The intake left `.failed(reason)` and the file; the phone retries.
      if let stored = try? await store.handoverReceipt(recordingID: recordingID) {
        activeReceipts[recordingID] = stored
      } else {
        receipt.state = .failed("admit: \(error)")
        activeReceipts[recordingID] = receipt
      }
      onReceiptsChange?(receiptsSnapshot)
      return .problem(.internalServerError, "admit: \(error)")
    }
  }

  // MARK: - Helpers

  static func status(of receipt: HandoverReceipt) -> Wire.RecordingStatus {
    if receipt.state.kind == .complete {
      let count = MetadataValidation.chunkCount(
        byteCount: receipt.byteCount, chunkSize: receipt.chunkSize)
      return Wire.RecordingStatus(state: .complete, receivedChunks: Array(0..<count))
    }
    return Wire.RecordingStatus(receipt)
  }

  /// The receipt from memory or the store.
  func receipt(_ recordingID: UUID) async -> HandoverReceipt? {
    if let active = activeReceipts[recordingID] { return active }
    guard let stored = try? await store.handoverReceipt(recordingID: recordingID) else {
      return nil
    }
    activeReceipts[recordingID] = stored
    return stored
  }

  /// The receipt when it belongs to the requesting device.
  private func ownedReceipt(_ recordingID: UUID, _ request: HandoverRequest) async
    -> HandoverReceipt?
  {
    guard let device = request.device, let receipt = await receipt(recordingID),
      receipt.deviceID == device.id
    else {
      return nil
    }
    return receipt
  }
}
