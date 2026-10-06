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
    guard let receipt = await ownedReceipt(recordingID, device: device) else {
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
      try await writeChunk(
        request.body, UInt64(index) * UInt64(receipt.chunkSize), inbox.partial(recordingID))
    } catch {
      return .internalError("writing the chunk", error)
    }
    // The write suspended the actor: another chunk may have landed, the
    // device may have been revoked, or the phone's `complete` may have
    // admitted the recording (this was an older attempt of a chunk it sent
    // again). Fold this chunk into the receipt as it stands now, never into
    // the copy from before the write; a `.complete` one stays as it is
    // (`transition`) and the chunk counts as received.
    guard var current = activeReceipts[recordingID], current.deviceID == device.id else {
      return .problem(.notFound, "no such recording")
    }
    do {
      try await transition(
        &current, to: .receiving, receivedChunks: Set(current.receivedChunks + [index]).sorted())
    } catch {
      return .internalError("saving the receipt", error)
    }
    return .empty(.noContent)
  }

  /// `POST /v1/recordings/{id}/complete`: 200 `{meetingID}` once every chunk
  /// is present and the whole file hashes to the announced value; 409 with
  /// the status while chunks are missing or while an earlier `complete` is
  /// still verifying or admitting, and with no chunk listed when the partial
  /// was replaced during the verify; 422 on a hash mismatch, after which the
  /// partial is gone and the phone starts over; 401 while a revoke of the
  /// device is in flight, and in place of any of these when one landed
  /// during the store read or the verify of a recording not yet admitted
  /// (its files are then discarded, unless another device announced the
  /// recording id meanwhile).
  func complete(_ recordingID: UUID, device: PairedDevice) async -> HandoverResponse {
    guard revoking[device.id] == nil else { return Self.unauthorized }
    let revocation = revocations[device.id, default: 0]
    guard var receipt = await ownedReceipt(recordingID, device: device) else {
      return .problem(.notFound, "no such recording")
    }
    if let meetingID = receipt.state.meetingID {
      // Admitted before, so a revoke during the read admits nothing new:
      // answer the meeting and drop the stale copy from memory. A 401 would
      // make the phone keep the recording, and its upload after pairing
      // again would become a second meeting.
      if revocations[device.id, default: 0] != revocation { forget(recordingID) }
      return .json(.ok, Wire.CompleteResponse(meetingID: meetingID))
    }
    // A revoke during the store read missed the receipt. Refuse before the
    // `.verifying` write, which would put its row back once the phone paired
    // again.
    if let refused = refusal(recordingID, device: device, revokedSince: revocation) {
      return refused
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
    let verification = await verifiedFile(
      for: &receipt, metadata: metadata, revokedSince: revocation)
    // Again after the verify, which suspends, whatever it answered. A
    // revoke then discards the files itself, but a phone that still passes
    // the gate (before the delete commits, or paired again) can announce
    // and send the chunks again: this check discards the new partial and
    // answers 401. Another phone's announce of the same recording id after
    // the revoke keeps its files and receipt. Nothing suspends between this
    // check and the intake call.
    if let refused = refusal(recordingID, device: device, revokedSince: revocation) {
      return refused
    }
    switch verification {
    case .answered(let response):
      return response
    case .file(let file):
      return await admit(file, metadata: metadata, device: device, receipt: &receipt)
    }
  }

  /// 401 when the device was revoked since `complete` took `revocation`.
  /// That revoke may have missed the receipt, so the files are discarded and
  /// the receipt forgotten here, unless memory holds another device's
  /// receipt of the recording id (`ownedByAnotherDevice`).
  private func refusal(_ recordingID: UUID, device: PairedDevice, revokedSince revocation: Int)
    -> HandoverResponse?
  {
    guard revocations[device.id, default: 0] != revocation else { return nil }
    if !ownedByAnotherDevice(recordingID, device: device) {
      inbox.discard(recordingID)
      forget(recordingID)
    }
    return Self.unauthorized
  }

  /// Whether memory holds the receipt of `recordingID` for a device other
  /// than `device`: another phone announced the same recording id after
  /// `device` was revoked, so the files in the inbox are that phone's upload
  /// and the receipt is its own. The caller checks and removes files in one
  /// actor step, with no suspension in between, so no announce lands between
  /// the two.
  private func ownedByAnotherDevice(_ recordingID: UUID, device: PairedDevice) -> Bool {
    guard let held = activeReceipts[recordingID] else { return false }
    return held.deviceID != device.id
  }

  private enum Verification {
    case file(URL)
    case answered(HandoverResponse)
  }

  /// The verified file: the one already waiting after an earlier intake
  /// failure, else the partial once every chunk is present (409 with the
  /// status otherwise) and the whole file hashes to the announced value (422
  /// and the partial is discarded otherwise), promoted to its final name. A
  /// partial gone or replaced during the hash answers 409 with no chunk
  /// listed, and a revoke since `complete` took `revocation` answers 401.
  private func verifiedFile(
    for receipt: inout HandoverReceipt, metadata: RecordingMetadata, revokedSince revocation: Int
  ) async -> Verification {
    let recordingID = receipt.recordingID
    if inbox.hasVerified(recordingID, format: metadata.format) {
      return .file(inbox.verified(recordingID, format: metadata.format))
    }
    let count = MetadataValidation.chunkCount(
      byteCount: receipt.byteCount, chunkSize: receipt.chunkSize)
    guard receipt.receivedChunks == Array(0..<count), inbox.hasPartial(recordingID) else {
      if !inbox.hasPartial(recordingID) {
        // Nothing suspended since `complete` read the receipt, so this is
        // the state memory holds.
        try? await transition(&receipt, to: receipt.state, receivedChunks: [])
      }
      return .answered(.json(.conflict, Self.status(of: receipt)))
    }
    // Taken before the `.verifying` write, the first suspension: the hash
    // must be of the file `promote` moves. A partial discarded meanwhile
    // and created again (a revoke, then an announce) is another file.
    let partial = inbox.partial(recordingID)
    let identity: ReceivingFile.Identity
    do {
      identity = try ReceivingFile.identity(of: partial)
    } catch {
      return .answered(.internalError("reading the partial", error))
    }
    try? await transition(&receipt, to: .verifying)

    let verified: Bool
    do {
      verified =
        try ReceivingFile.size(of: partial) == receipt.byteCount
        ? try await hashMatches(partial, receipt.sha256) : false
    } catch {
      return .answered(.internalError("verifying the file", error))
    }
    // Before any write: once the phone paired again, one would put back
    // the row the revoke deleted. `complete` discards the files.
    guard revocations[receipt.deviceID, default: 0] == revocation else {
      return .answered(Self.unauthorized)
    }
    guard (try? ReceivingFile.identity(of: partial)) == identity else {
      // Gone or another file: the phone sends every chunk again.
      try? await transition(&receipt, to: .receiving, receivedChunks: [])
      return .answered(.json(.conflict, Self.status(of: receipt)))
    }
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
  /// real intake wrote this same receipt and deleted the file. Every file
  /// of the recording left in the inbox then goes: the metadata sidecar,
  /// and a partial and sidecar that a re-announce opened during the intake,
  /// after the intake took the verified file. A replayed complete returns
  /// the same id through the early `.complete` check. On failure the
  /// verified file stays for the phone's retry and the reason is fixed text,
  /// because the error may name the file's path.
  private func admit(
    _ file: URL, metadata: RecordingMetadata, device: PairedDevice,
    receipt: inout HandoverReceipt
  ) async -> HandoverResponse {
    let recordingID = receipt.recordingID
    let meetingID: UUID
    do {
      meetingID = try await intake.admit(file: file, metadata: metadata, device: device)
    } catch {
      try? await transition(&receipt, to: .failed(Self.intakeRefused))
      return .internalError("the intake", error)
    }
    try? await transition(&receipt, to: .complete(meetingID: meetingID))
    // A revoke during the intake lets another phone announce the same
    // recording id; its files and receipt stay. Checked after the last
    // suspension, in the same actor step as the discard.
    if !ownedByAnotherDevice(recordingID, device: device) {
      inbox.discard(recordingID)
    }
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

  /// The receipt from memory or the store, kept in memory (`remember`).
  /// Another request may have made, loaded or advanced it while the store
  /// read was awaited; memory wins then, also over a read that found none
  /// or failed, so a first announce that raced another answers as a
  /// re-announce and keeps the receipt the other made, chunks and
  /// `.complete` included.
  func receipt(_ recordingID: UUID) async -> HandoverReceipt? {
    if let active = activeReceipts[recordingID] { return active }
    let stored = try? await store.handoverReceipt(recordingID: recordingID)
    if let active = activeReceipts[recordingID] { return active }
    guard let stored else { return nil }
    remember(stored)
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
