import Foundation
import GRDB

extension MeetingStore {
  /// Every paired phone, oldest pairing first.
  public func pairedDevices() async throws -> [PairedDevice] {
    try await writer.read { db in
      try PairedDeviceRow.order(PairedDeviceRow.Columns.pairedAt, PairedDeviceRow.Columns.id)
        .fetchAll(db)
        .map(\.device)
    }
  }

  /// `tokenHash` is the SHA-256 of the bearer token; the token itself is
  /// never stored. On the disk when it returns (`writeDurably`): the phone
  /// keeps the token from the answer that follows, and a pairing a power
  /// loss rolled back would unpair it. Rust: `Store::save_paired_device`.
  public func save(_ device: PairedDevice, tokenHash: Data) async throws {
    try await writeDurably { db in try PairedDeviceRow(device, tokenHash: tokenHash).save(db) }
  }

  /// Refreshes `lastSeenAt` of the device that still holds `tokenHash`. An
  /// update, never an insert: the engine touches a device it read before a
  /// suspension, and a revoke or a new pairing in between must stand.
  public func touchPairedDevice(id: UUID, tokenHash: Data, seenAt: Date) async throws {
    _ = try await writer.write { db in
      try PairedDeviceRow
        .filter(PairedDeviceRow.Columns.id == id.uuidString)
        .filter(PairedDeviceRow.Columns.tokenHash == tokenHash)
        .updateAll(db, PairedDeviceRow.Columns.lastSeenAt.set(to: seenAt))
    }
  }

  public func device(forTokenHash tokenHash: Data) async throws -> PairedDevice? {
    try await writer.read { db in
      try PairedDeviceRow.filter(PairedDeviceRow.Columns.tokenHash == tokenHash).fetchOne(db)?
        .device
    }
  }

  public func pairedDevice(id: UUID) async throws -> PairedDevice? {
    try await writer.read { db in
      try PairedDeviceRow.filter(PairedDeviceRow.Columns.id == id.uuidString).fetchOne(db)?.device
    }
  }

  /// Revokes the phone; its handover receipts go with it. On the disk when
  /// it returns (`writeDurably`), so a power loss cannot bring a revoked
  /// phone back. Rust: `Store::delete_paired_device`.
  public func delete(deviceID: UUID) async throws {
    try await writeDurably { db in
      _ = try PairedDeviceRow.filter(PairedDeviceRow.Columns.id == deviceID.uuidString).deleteAll(
        db)
    }
  }

  public func handoverReceipt(recordingID: UUID) async throws -> HandoverReceipt? {
    try await writer.read { db in
      try HandoverReceiptRow
        .filter(HandoverReceiptRow.Columns.recordingID == recordingID.uuidString)
        .fetchOne(db)?
        .receipt
    }
  }

  public func save(_ receipt: HandoverReceipt) async throws {
    try await writer.write { db in try HandoverReceiptRow(receipt).save(db) }
  }

  /// `save(_:)` on the disk when it returns (`writeDurably`): the phone
  /// intake's `.failed` receipt after a failed admission commit, whose
  /// frames may still sit in the WAL for recovery to replay. This commit
  /// writes over them, or voids them when the WAL restarts, so once it
  /// returns no restart brings the admission back.
  /// Rust: `Store::save_handover_receipt_durably`.
  public func saveDurably(_ receipt: HandoverReceipt) async throws {
    try await writeDurably { db in try HandoverReceiptRow(receipt).save(db) }
  }

  /// The phone intake's admission: the `.complete` receipt, the meeting,
  /// its asset and the admission's ledger row (`admittedMeeting`) in one
  /// transaction, on the disk when it returns (`writeDurably`). It returns
  /// the meeting the receipt was completed with. The phone deletes its copy
  /// once `complete` answers 200, so no commit may hold the receipt without
  /// the meeting, and a power loss must not roll either back.
  ///
  /// When the ledger already holds these bytes (the same recording id, size
  /// and SHA-256) and their meeting still exists, the receipt is completed
  /// with that meeting, `meeting` and `asset` are not written, and that
  /// meeting's id comes back: the same bytes are one recording. Another
  /// device's upload of them can reach the intake after the first admission
  /// committed (it took the receipt over during that intake), and a second
  /// meeting would be a duplicate. When the user deleted the row's meeting,
  /// the admission writes `meeting` and the row moves to it, so a later
  /// upload of the same bytes finds that meeting instead of writing a third.
  ///
  /// Throws `MeetingStoreError.receiptOfAnotherUpload`, with nothing
  /// written, when the stored receipt belongs to another device than
  /// `receipt` or holds another size or SHA-256: completed, it would answer
  /// that device's `complete`, or the `complete` of the other bytes, with
  /// this meeting, and the phone would delete a recording never admitted.
  /// Rust: `Store::save_admission_durably`.
  @discardableResult
  public func saveDurably(_ receipt: HandoverReceipt, meeting: Meeting, asset: AudioAsset)
    async throws -> UUID
  {
    try await writeDurably { db in
      let stored = try HandoverReceiptRow
        .filter(HandoverReceiptRow.Columns.recordingID == receipt.recordingID.uuidString)
        .fetchOne(db)?
        .receipt
      if let stored,
        stored.deviceID != receipt.deviceID || stored.byteCount != receipt.byteCount
          || stored.sha256 != receipt.sha256
      {
        throw MeetingStoreError.receiptOfAnotherUpload(receipt.recordingID)
      }
      if let earlier = try HandoverAdmissionRow.of(
        recordingID: receipt.recordingID, byteCount: receipt.byteCount, sha256: receipt.sha256
      ).fetchOne(db)?.meetingID,
        try MeetingRow.filter(MeetingRow.Columns.id == earlier.uuidString).fetchCount(db) > 0
      {
        var completed = receipt
        completed.state = .complete(meetingID: earlier)
        try HandoverReceiptRow(completed).save(db)
        return earlier
      }
      try MeetingRow(meeting).save(db)
      try AudioAssetRow(asset).save(db)
      try HandoverReceiptRow(receipt).save(db)
      try db.execute(
        sql: """
          INSERT INTO "handoverAdmission" \
          ("recordingID", "byteCount", "sha256", "meetingID", "admittedAt") \
          VALUES (?, ?, ?, ?, ?) \
          ON CONFLICT ("recordingID", "byteCount", "sha256") DO UPDATE \
          SET "meetingID" = excluded."meetingID", "admittedAt" = excluded."admittedAt" \
          WHERE "meetingID" NOT IN (SELECT "id" FROM "meeting")
          """,
        arguments: [
          receipt.recordingID.uuidString, receipt.byteCount, receipt.sha256,
          meeting.id.uuidString, receipt.updatedAt,
        ])
      return meeting.id
    }
  }

  /// The meeting the phone recording `recordingID` of `byteCount` bytes
  /// hashing to `sha256` was admitted as, from the admission ledger
  /// (`handoverAdmission`, schema v5). A row is written in the admission's
  /// own transaction (`saveDurably(_:meeting:asset:)`) and never deleted:
  /// not by a revoke, whose cascade takes the receipts, nor by a meeting
  /// delete, so the meeting may be gone. The handover answers a phone that
  /// announces those bytes again "delivered" from it, so the phone deletes
  /// its copy instead of uploading it as a second meeting.
  /// Rust: `Store::admitted_meeting`.
  public func admittedMeeting(recordingID: UUID, byteCount: Int64, sha256: Data) async throws
    -> UUID?
  {
    try await writer.read { db in
      try HandoverAdmissionRow.of(recordingID: recordingID, byteCount: byteCount, sha256: sha256)
        .fetchOne(db)?.meetingID
    }
  }

  /// Writes the ledger row of every `.complete` receipt whose meeting row
  /// exists and that has none yet: the admissions before schema v5, and
  /// those an older app committed during a rollback, since it ignores v5
  /// and writes no row. `init` runs it on every open.
  /// Rust: `Store::backfill_handover_admissions`, which runs the same text.
  static let backfillAdmissions = """
    INSERT OR IGNORE INTO "handoverAdmission" \
    ("recordingID", "byteCount", "sha256", "meetingID", "admittedAt") \
    SELECT "recordingID", "byteCount", "sha256", "meetingID", "updatedAt" \
    FROM "handoverReceipt" \
    WHERE "state" = 'complete' \
    AND "meetingID" IN (SELECT "id" FROM "meeting")
    """
}
