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
  /// recording (no receipt, or other bytes than the receipt's), 200 for a
  /// known one, both with `RecordingStatus`; 200 `.complete` with every chunk
  /// listed for bytes the admission ledger shows admitted; 409 only for
  /// admitted bytes over another device's unfinished upload of other bytes;
  /// 500 with nothing opened when the store cannot read the ledger or the
  /// receipt. The decision table is in
  /// `.plans/2026-10-08-handover-admission-ledger.md`. Rust:
  /// `Engine::announce`.
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

    var known: HandoverReceipt?
    do {
      known = try await readReceipt(recordingID)
    } catch {
      return .internalError("reading the receipt", error)
    }
    // The ledger is read only where it decides something: no receipt, or
    // one of other bytes. Its rows are never deleted, so a row read stays
    // true. The read suspends, so memory may hold another receipt by then
    // (a first announce of the same recording, a chunk, an admission): the
    // announce then decides again with that one.
    var admitted: UUID?
    while known.map({ $0.byteCount != metadata.byteCount || $0.sha256 != metadata.sha256 })
      ?? true
    {
      do {
        admitted = try await store.admittedMeeting(
          recordingID: recordingID, byteCount: metadata.byteCount, sha256: metadata.sha256)
      } catch {
        return .internalError("reading the admission ledger", error)
      }
      guard let held = activeReceipts[recordingID], held != known else { break }
      known = held
    }
    // From here to the first suspension (the save in `persist`, or
    // `transition`) is one actor step with the last look at memory.
    guard let existing = known else {
      if let admitted {
        // Admitted before: the phone posts `complete`, takes the meeting id
        // and deletes its copy.
        return await replace(
          with: freshReceipt(metadata, device: device, state: .complete(meetingID: admitted)),
          opening: nil, status: .ok)
      }
      return await replace(
        with: freshReceipt(metadata, device: device, state: .receiving), opening: metadata,
        status: .created)
    }
    let complete = existing.state.kind == .complete
    guard existing.byteCount == metadata.byteCount, existing.sha256 == metadata.sha256 else {
      // Another file under the id.
      if let admitted {
        // Answered `complete` over another device's unfinished upload, that
        // phone's `complete` would get this meeting and delete its copy.
        guard complete || existing.deviceID == device.id else {
          return .problem(.conflict, "another device owns this recording")
        }
        return await replace(
          with: freshReceipt(metadata, device: device, state: .complete(meetingID: admitted)),
          opening: nil, status: .ok)
      }
      // A new recording under the same recording id; its own `complete`
      // admits a meeting of its own.
      return await replace(
        with: freshReceipt(metadata, device: device, state: .receiving), opening: metadata,
        status: .created)
    }
    var receipt = existing
    if existing.deviceID != device.id {
      // Another device announces the same bytes: they are the same
      // recording, so it takes the receipt over with its chunks and files.
      // Whichever device's `complete` admits them, the other phone's next
      // announce takes the `.complete` receipt back and is answered
      // delivered. The older device's requests then find no receipt of
      // theirs, and its late writes leave this one alone (`transition`).
      receipt.deviceID = device.id
      receipt.updatedAt = now()
      remember(receipt)
      if complete {
        do {
          try await persist(receipt)
        } catch {
          return .internalError("saving the receipt", error)
        }
      }
    }
    // The chunk size matters only until the receipt is `.complete`: the
    // same bytes split otherwise are the file the computer holds. The
    // answer lists every chunk of the phone's split, so it posts
    // `complete`; the copy it is built from is never saved.
    if complete {
      var resplit = receipt
      resplit.chunkSize = metadata.chunkSize
      return .json(.ok, Self.status(of: resplit))
    }
    if receipt.chunkSize != metadata.chunkSize {
      // The partial starts over under the announced split.
      receipt.state = .receiving
      receipt.chunkSize = metadata.chunkSize
      receipt.receivedChunks = []
      receipt.updatedAt = now()
      return await replace(with: receipt, opening: metadata, status: .ok)
    }
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

  /// A receipt of `metadata`'s bytes for `device` in `state`, no chunk
  /// received, made now.
  private func freshReceipt(
    _ metadata: RecordingMetadata, device: PairedDevice, state: HandoverState
  ) -> HandoverReceipt {
    let timestamp = now()
    return HandoverReceipt(
      recordingID: metadata.recordingID, deviceID: device.id, state: state,
      byteCount: metadata.byteCount, sha256: metadata.sha256, chunkSize: metadata.chunkSize,
      receivedChunks: [], createdAt: timestamp, updatedAt: timestamp)
  }

  /// Makes `receipt` the receipt of its recording id in place of whatever
  /// the announce read (none, or one it decided to replace), opens its files
  /// (`opening`; none for a receipt answered `.complete`) and answers
  /// `status` with it.
  ///
  /// Every file of the recording id goes before `begin`. The discard runs in
  /// the same actor step as the read's last look at memory and the
  /// `remember` in `persist`, so no request lands in between. After a first
  /// announce whatever is there belongs to no receipt (a verified file left
  /// by an intake failure whose receipt a revoke deleted after a restart,
  /// the files a revoked phone's re-announce opened before the revoke's
  /// delete committed). Kept, `begin` would add this upload's chunks to an
  /// old partial, and `complete` would hand an old verified file to the
  /// intake unhashed. After a replacement they are the replaced upload's: of
  /// other bytes, of the same bytes in another split, or of bytes the ledger
  /// shows admitted.
  ///
  /// No live upload of another device the computer could still admit is in
  /// them. A device's recording routes read its receipt into memory before
  /// they touch a file, and memory drops it only when that device is
  /// revoked, so only a revoked device's request, or one of the replaced
  /// upload, can still be at work on them. Such a request answers a refusal,
  /// an error, a 404 or a re-announce's status, and the phone deletes its
  /// copy only on a 200 from `complete`, so it keeps its recording; or it
  /// answers the 200 of an admission whose intake opened the verified file
  /// before the discard and copies it whole. Rust: `Engine::replace`.
  private func replace(
    with receipt: HandoverReceipt, opening metadata: RecordingMetadata?,
    status: HTTPResponseStatus
  ) async -> HandoverResponse {
    let recordingID = receipt.recordingID
    inbox.discard(recordingID)
    if let metadata {
      do {
        try inbox.begin(metadata)
      } catch {
        return .internalError("opening the partial file", error)
      }
    }
    do {
      try await persist(receipt)
    } catch {
      // A revoke while the save waited lets another phone announce the same
      // recording id; its files and receipt stay. Checked after the save,
      // in the same actor step as the discard.
      if !ownedByAnotherDevice(recordingID, deviceID: receipt.deviceID) {
        inbox.discard(recordingID)
      }
      return .internalError("saving the receipt", error)
    }
    return .json(status, Self.status(of: receipt))
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
    // The upload this chunk was written for, in its split: not another
    // device's receipt, not one of other bytes, not one restarted under
    // another chunk size (`sameUpload`).
    guard var current = activeReceipts[recordingID], Self.sameUpload(current, receipt),
      current.chunkSize == receipt.chunkSize
    else {
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
  /// `device` was revoked, so the files in the inbox are taken as that
  /// phone's upload and the receipt is its own. The caller checks and removes files in one
  /// actor step, with no suspension in between, so no announce lands between
  /// the two.
  private func ownedByAnotherDevice(_ recordingID: UUID, device: PairedDevice) -> Bool {
    ownedByAnotherDevice(recordingID, deviceID: device.id)
  }

  private func ownedByAnotherDevice(_ recordingID: UUID, deviceID: UUID) -> Bool {
    guard let held = activeReceipts[recordingID] else { return false }
    return held.deviceID != deviceID
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
  /// after the intake took the verified file. When another device announced
  /// the same recording id meanwhile (this one was revoked during the
  /// intake), only the verified file goes, if the intake left it, and the
  /// rest is that phone's upload (`ownedByAnotherDevice`); no other request
  /// creates the verified file while this `complete` holds the `completing`
  /// mark. A replayed complete returns the same id through the early
  /// `.complete` check. On failure the verified file stays for the phone's
  /// retry and the reason is fixed text, because the error may name the
  /// file's path; unless memory holds a receipt of other bytes by then (the
  /// phone announced another file under the id during the intake): that
  /// upload's `complete` would hand this file to the intake unhashed, so it
  /// goes.
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
      // The phone announced another file under the id during the intake:
      // that upload's `complete` would hand this file to the intake unhashed
      // (`verifiedFile`), so it goes.
      if let held = activeReceipts[recordingID],
        held.byteCount != receipt.byteCount || held.sha256 != receipt.sha256
      {
        try? FileManager.default.removeItem(at: file)
      }
      return .internalError("the intake", error)
    }
    try? await transition(&receipt, to: .complete(meetingID: meetingID))
    try? FileManager.default.removeItem(at: file)
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
  /// The `.failed` reason a stored `.complete` receipt reads as when its
  /// meeting is missing (`storedReceipt`). Rust: `Engine::MEETING_MISSING`.
  static let meetingMissing = "the admitted meeting is missing"

  static func status(of receipt: HandoverReceipt) -> Wire.RecordingStatus {
    if receipt.state.kind == .complete {
      let count = MetadataValidation.chunkCount(
        byteCount: receipt.byteCount, chunkSize: receipt.chunkSize)
      return Wire.RecordingStatus(state: .complete, receivedChunks: Array(0..<count))
    }
    return Wire.RecordingStatus(state: receipt.state.kind, receivedChunks: receipt.receivedChunks)
  }

  /// `readReceipt` with a failed store read counted as none, so status,
  /// chunk and complete keep their 404 (the plan's "Store reads" note).
  func receipt(_ recordingID: UUID) async -> HandoverReceipt? {
    try? await readReceipt(recordingID)
  }

  /// The receipt from memory or the store (`storedReceipt`, so a
  /// `.complete` one whose meeting is missing comes back not admitted),
  /// kept in memory (`remember`). Another request may have made, loaded or
  /// advanced it while the store read was awaited; memory wins then, also
  /// over a read that found none or failed, so a first announce that raced
  /// another answers as a re-announce and keeps the receipt the other made,
  /// chunks and `.complete` included. A failed read with nothing in memory
  /// throws: `announce` must not take it for no receipt, or it would
  /// discard the files of a receipt only in the store and save a new one
  /// over it.
  func readReceipt(_ recordingID: UUID) async throws -> HandoverReceipt? {
    if let active = activeReceipts[recordingID] { return active }
    let stored: HandoverReceipt?
    do {
      stored = try await storedReceipt(recordingID)
    } catch {
      if let active = activeReceipts[recordingID] { return active }
      throw error
    }
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
