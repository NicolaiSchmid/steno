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

  /// The phone intake's admission: the `.complete` receipt, the meeting and
  /// its asset in one transaction, on the disk when it returns
  /// (`writeDurably`). The phone deletes its copy once `complete` answers
  /// 200, so no commit may hold the receipt without the meeting, and a
  /// power loss must not roll either back. Throws
  /// `MeetingStoreError.receiptOfAnotherDevice`, with nothing written, when
  /// the stored receipt belongs to another device than `receipt`:
  /// completed, it would answer that device's `complete` with this meeting.
  /// Rust: `Store::save_admission_durably`.
  public func saveDurably(_ receipt: HandoverReceipt, meeting: Meeting, asset: AudioAsset)
    async throws
  {
    try await writeDurably { db in
      let stored = try HandoverReceiptRow
        .filter(HandoverReceiptRow.Columns.recordingID == receipt.recordingID.uuidString)
        .fetchOne(db)?
        .receipt
      if let stored, stored.deviceID != receipt.deviceID {
        throw MeetingStoreError.receiptOfAnotherDevice(receipt.recordingID)
      }
      try MeetingRow(meeting).save(db)
      try AudioAssetRow(asset).save(db)
      try HandoverReceiptRow(receipt).save(db)
    }
  }
}
