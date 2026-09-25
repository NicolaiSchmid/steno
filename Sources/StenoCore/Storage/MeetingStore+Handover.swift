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
  /// never stored.
  public func save(_ device: PairedDevice, tokenHash: Data) async throws {
    try await writer.write { db in try PairedDeviceRow(device, tokenHash: tokenHash).save(db) }
  }

  public func device(forTokenHash tokenHash: Data) async throws -> PairedDevice? {
    try await writer.read { db in
      try PairedDeviceRow.filter(PairedDeviceRow.Columns.tokenHash == tokenHash).fetchOne(db)?
        .device
    }
  }

  public func device(id: UUID) async throws -> PairedDevice? {
    try await writer.read { db in
      try PairedDeviceRow.filter(PairedDeviceRow.Columns.id == id.uuidString).fetchOne(db)?.device
    }
  }

  /// Revokes the phone; its handover receipts go with it.
  public func delete(deviceID: UUID) async throws {
    try await writer.write { db in
      _ = try PairedDeviceRow.filter(PairedDeviceRow.Columns.id == deviceID.uuidString).deleteAll(
        db)
    }
  }

  public func receipt(_ recordingID: UUID) async throws -> HandoverReceipt? {
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
}
