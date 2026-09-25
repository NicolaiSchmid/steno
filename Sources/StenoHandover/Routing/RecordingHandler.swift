import Crypto
import Foundation
import NIOHTTP1
import StenoCore

/// The four recording routes: announce, status, chunk, complete. Every path
/// is idempotent on the recording id, so a phone that lost the answer can
/// simply repeat the call. `device` is the bearer principal the gate
/// established; a recording belongs to the device that announced it.
extension HandoverEngine {
  /// `PUT /v1/recordings/{id}` with `RecordingMetadata`: 201 for a new
  /// recording, 200 for a known one, both with `RecordingStatus`.
  func announce(_ recordingID: UUID, device: PairedDevice, body: Data) async -> HandoverResponse {
    let metadata: RecordingMetadata
    do {
      metadata = try StenoJSON.decode(RecordingMetadata.self, from: body)
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
      var receivedChunks: [Int]?
      if !inbox.hasVerified(recordingID, format: metadata.format),
        !inbox.hasPartial(recordingID) || inbox.loadMetadata(recordingID) == nil
      {
        // The partial is gone (a sweep, a crash before the first chunk):
        // start over with the same receipt. A verified file waiting for a
        // second intake attempt keeps its chunk set instead, so the phone's
        // retry (announce, then complete) sends no chunk twice.
        do {
          try inbox.begin(metadata)
        } catch {
          return .internalError("opening the partial file", error)
        }
        receivedChunks = []
      }
      do {
        try await transition(&receipt, to: .receiving, receivedChunks: receivedChunks)
      } catch {
        return .internalError("saving the receipt", error)
      }
      return .json(.ok, Self.status(of: receipt))
    }

    do {
      try inbox.begin(metadata)
    } catch {
      return .internalError("opening the partial file", error)
    }
    let timestamp = now()
    let receipt = HandoverReceipt(
      recordingID: recordingID, deviceID: device.id, state: .receiving,
      byteCount: metadata.byteCount, sha256: metadata.sha256, chunkSize: metadata.chunkSize,
      receivedChunks: [], createdAt: timestamp, updatedAt: timestamp)
    do {
      try await persist(receipt)
    } catch {
      inbox.discard(recordingID)
      return .internalError("saving the receipt", error)
    }
    return .json(.created, Self.status(of: receipt))
  }

  /// `GET /v1/recordings/{id}`: the resume point, 404 for an unknown id.
  func status(_ recordingID: UUID, device: PairedDevice) async -> HandoverResponse {
    guard let receipt = await ownedReceipt(recordingID, device: device) else {
      return .problem(.notFound, "no such recording")
    }
    return .json(.ok, Self.status(of: receipt))
  }

  /// `PUT /v1/recordings/{id}/chunks/{n}` with raw bytes and
  /// `X-Steno-Chunk-SHA256`: 204, also for a chunk already received.
  func receiveChunk(
    _ recordingID: UUID, index: Int, device: PairedDevice, _ request: HandoverRequest
  ) async -> HandoverResponse {
    guard var receipt = await ownedReceipt(recordingID, device: device) else {
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
    guard let declared = request.headers.first(name: Wire.chunkHashHeader),
      let digest = Data(base64Encoded: declared), digest.count == 32
    else {
      return .problem(.badRequest, "\(Wire.chunkHashHeader) must be the base64 SHA-256 of the body")
    }
    // swift-crypto compares digests in constant time.
    guard SHA256.hash(data: request.body) == digest else {
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
      try await ReceivingFile.write(
        request.body, at: UInt64(index) * UInt64(receipt.chunkSize), to: inbox.partial(recordingID))
    } catch {
      return .internalError("writing the chunk", error)
    }
    // The write suspended the actor: another chunk may have landed, or the
    // device may have been revoked. Fold this chunk into the receipt as it
    // stands now, never into the copy from before the write.
    guard let current = activeReceipts[recordingID], current.deviceID == device.id else {
      return .problem(.notFound, "no such recording")
    }
    receipt = current
    do {
      try await transition(
        &receipt, to: .receiving, receivedChunks: Set(receipt.receivedChunks + [index]).sorted())
    } catch {
      return .internalError("saving the receipt", error)
    }
    return .empty(.noContent)
  }

  /// `POST /v1/recordings/{id}/complete`: 200 `{meetingID}` once every chunk
  /// is present and the whole file hashes to the announced value; 409 with
  /// the status while chunks are missing or while an earlier `complete` is
  /// still verifying or admitting; 422 on a hash mismatch, after which the
  /// partial is gone and the phone starts over.
  func complete(_ recordingID: UUID, device: PairedDevice) async -> HandoverResponse {
    guard var receipt = await ownedReceipt(recordingID, device: device) else {
      return .problem(.notFound, "no such recording")
    }
    if let meetingID = receipt.state.meetingID {
      return .json(.ok, Wire.CompleteResponse(meetingID: meetingID))
    }
    // One `complete` per recording at a time: the phone retries after its
    // own timeout, and a second verify or admission of the same file must
    // not start while the first is suspended. The phone answers a 409 whose
    // status lists every chunk by backing off.
    guard !completing.contains(recordingID) else {
      return .json(.conflict, Self.status(of: receipt))
    }
    completing.insert(recordingID)
    defer { completing.remove(recordingID) }
    guard let metadata = inbox.loadMetadata(recordingID) else {
      return .problem(.notFound, "no metadata; announce again")
    }
    switch await verifiedFile(for: &receipt, metadata: metadata) {
    case .answered(let response):
      return response
    case .file(let file):
      return await admit(file, metadata: metadata, device: device, receipt: &receipt)
    }
  }

  private enum Verification {
    case file(URL)
    case answered(HandoverResponse)
  }

  /// The verified file: the one already waiting after an earlier intake
  /// failure, else the partial once every chunk is present (409 with the
  /// status otherwise) and the whole file hashes to the announced value (422
  /// and the partial is discarded otherwise), promoted to its final name.
  private func verifiedFile(for receipt: inout HandoverReceipt, metadata: RecordingMetadata)
    async -> Verification
  {
    let recordingID = receipt.recordingID
    if inbox.hasVerified(recordingID, format: metadata.format) {
      return .file(inbox.verified(recordingID, format: metadata.format))
    }
    let count = MetadataValidation.chunkCount(
      byteCount: receipt.byteCount, chunkSize: receipt.chunkSize)
    guard receipt.receivedChunks == Array(0..<count), inbox.hasPartial(recordingID) else {
      if !inbox.hasPartial(recordingID) {
        try? await transition(&receipt, to: receipt.state, receivedChunks: [])
      }
      return .answered(.json(.conflict, Self.status(of: receipt)))
    }
    try? await transition(&receipt, to: .verifying)

    let partial = inbox.partial(recordingID)
    let verified: Bool
    do {
      verified =
        try ReceivingFile.size(of: partial) == receipt.byteCount
        ? try await ReceivingFile.hashMatches(partial, expected: receipt.sha256) : false
    } catch {
      return .answered(.internalError("verifying the file", error))
    }
    receipt = activeReceipts[recordingID] ?? receipt
    guard verified else {
      inbox.discard(recordingID)
      try? await transition(&receipt, to: .failed("sha256 mismatch"), receivedChunks: [])
      return .answered(
        .problem(.unprocessableEntity, "sha256 mismatch; the partial was discarded"))
    }
    do {
      return .file(try inbox.promote(recordingID, format: metadata.format))
    } catch {
      return .answered(.internalError("moving the verified file", error))
    }
  }

  /// Hands the verified file to the intake. On success the receipt is
  /// `.complete` and the answer is 200 whatever the receipt write did: the
  /// real intake wrote this same receipt and deleted the file, a test intake
  /// did neither. The metadata sidecar is ours to remove; a replayed
  /// complete returns the same id through the early `.complete` check. On
  /// failure the verified file stays for the phone's retry and the reason is
  /// fixed text, because the error may name the file's path.
  private func admit(
    _ file: URL, metadata: RecordingMetadata, device: PairedDevice,
    receipt: inout HandoverReceipt
  ) async -> HandoverResponse {
    let recordingID = receipt.recordingID
    let meetingID: UUID
    do {
      meetingID = try await intake.admit(file: file, metadata: metadata, device: device)
    } catch {
      receipt = activeReceipts[recordingID] ?? receipt
      try? await transition(&receipt, to: .failed(Self.intakeRefused))
      return .internalError("the intake", error)
    }
    receipt = activeReceipts[recordingID] ?? receipt
    try? await transition(&receipt, to: .complete(meetingID: meetingID))
    try? FileManager.default.removeItem(at: inbox.metadata(recordingID))
    return .json(.ok, Wire.CompleteResponse(meetingID: meetingID))
  }

  // MARK: - Helpers

  /// The `.failed` reason after the intake threw; the detail is logged.
  static let intakeRefused = "the intake refused the file"

  static func status(of receipt: HandoverReceipt) -> Wire.RecordingStatus {
    if receipt.state.kind == .complete {
      let count = MetadataValidation.chunkCount(
        byteCount: receipt.byteCount, chunkSize: receipt.chunkSize)
      return Wire.RecordingStatus(state: .complete, receivedChunks: Array(0..<count))
    }
    return Wire.RecordingStatus(state: receipt.state.kind, receivedChunks: receipt.receivedChunks)
  }

  /// The receipt from memory or the store. Another request may have loaded
  /// and advanced it while the store read was awaited; memory wins then.
  func receipt(_ recordingID: UUID) async -> HandoverReceipt? {
    if let active = activeReceipts[recordingID] { return active }
    guard let stored = try? await store.handoverReceipt(recordingID: recordingID) else {
      return nil
    }
    if let active = activeReceipts[recordingID] { return active }
    activeReceipts[recordingID] = stored
    return stored
  }

  /// The receipt when it belongs to the requesting device.
  private func ownedReceipt(_ recordingID: UUID, device: PairedDevice) async -> HandoverReceipt? {
    guard let receipt = await receipt(recordingID), receipt.deviceID == device.id else {
      return nil
    }
    return receipt
  }
}
