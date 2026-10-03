//! `pairedDevice` and `handoverReceipt` rows: the phones paired with this
//! computer and where each phone recording's handover stands.
//! Swift: `Sources/StenoCore/Storage/MeetingStore+Handover.swift`.

use chrono::{DateTime, Utc};
use rusqlite::{Connection, OptionalExtension, Row, params};
use uuid::Uuid;

use super::convert::{DbDate, DbEnum, DbJson, DbUuid, RowExt as _};
use super::{Result, Store, query_all, upsert_sql};
use crate::model::{HandoverReceipt, HandoverState, PairedDevice};

const DEVICE_COLUMNS: &str = "id, name, pairedAt, lastSeenAt, tokenHash";

fn device_from_row(row: &Row<'_>) -> rusqlite::Result<PairedDevice> {
    Ok(PairedDevice {
        id: row.col::<DbUuid>("id")?,
        name: row.get("name")?,
        paired_at: row.col::<DbDate>("pairedAt")?,
        last_seen_at: row.col::<Option<DbDate>>("lastSeenAt")?,
    })
}

fn device_where(
    connection: &Connection,
    clause: &str,
    param: impl rusqlite::ToSql,
) -> Result<Option<PairedDevice>> {
    Ok(connection
        .query_row(
            &format!("SELECT {DEVICE_COLUMNS} FROM pairedDevice WHERE {clause}"),
            [param],
            device_from_row,
        )
        .optional()?)
}

const RECEIPT_COLUMNS: &str = "recordingID, deviceID, state, meetingID, failureMessage, byteCount, \
     sha256, chunkSize, receivedChunks, createdAt, updatedAt";

fn receipt_from_row(row: &Row<'_>) -> rusqlite::Result<HandoverReceipt> {
    let kind = row.col::<DbEnum<_>>("state")?;
    let meeting_id = row.col::<Option<DbUuid>>("meetingID")?;
    let failure_message: Option<String> = row.get("failureMessage")?;
    Ok(HandoverReceipt {
        recording_id: row.col::<DbUuid>("recordingID")?,
        device_id: row.col::<DbUuid>("deviceID")?,
        state: HandoverState::from_columns(kind, meeting_id, failure_message),
        byte_count: row.get("byteCount")?,
        sha256: row.get("sha256")?,
        chunk_size: row.get("chunkSize")?,
        received_chunks: row.col::<DbJson<_>>("receivedChunks")?,
        created_at: row.col::<DbDate>("createdAt")?,
        updated_at: row.col::<DbDate>("updatedAt")?,
    })
}

impl Store {
    /// Every paired phone, oldest pairing first.
    pub fn paired_devices(&self) -> Result<Vec<PairedDevice>> {
        self.read(|connection| {
            query_all(
                connection,
                &format!("SELECT {DEVICE_COLUMNS} FROM pairedDevice ORDER BY pairedAt, id"),
                [],
                device_from_row,
            )
        })
    }

    /// Inserts or replaces the device (GRDB's `save`). `token_hash` is the
    /// SHA-256 of the bearer token; the token itself is never stored.
    pub fn save_paired_device(&self, device: &PairedDevice, token_hash: &[u8]) -> Result<()> {
        self.write(|transaction| {
            transaction.execute(
                &upsert_sql("pairedDevice", DEVICE_COLUMNS),
                params![
                    DbUuid(device.id),
                    device.name,
                    DbDate(device.paired_at),
                    device.last_seen_at.map(DbDate),
                    token_hash,
                ],
            )?;
            Ok(())
        })
    }

    /// Refreshes `lastSeenAt` of the device that still holds `token_hash`.
    /// An `UPDATE`, not a save: the engine reads the device and touches it
    /// after a yield, and a revoke in between must stay a revoke. A row that
    /// is gone, or whose token changed, is left alone. New here; Swift
    /// upserts (parity list).
    pub fn touch_paired_device(
        &self,
        id: Uuid,
        token_hash: &[u8],
        seen_at: DateTime<Utc>,
    ) -> Result<()> {
        self.write(|transaction| {
            transaction.execute(
                "UPDATE pairedDevice SET lastSeenAt = ?3 WHERE id = ?1 AND tokenHash = ?2",
                params![DbUuid(id), token_hash, DbDate(seen_at)],
            )?;
            Ok(())
        })
    }

    /// The device whose bearer token hashes to `token_hash`.
    pub fn paired_device_for_token_hash(&self, token_hash: &[u8]) -> Result<Option<PairedDevice>> {
        self.read(|connection| device_where(connection, "tokenHash = ?1", token_hash))
    }

    /// The device with `id`.
    pub fn paired_device(&self, id: Uuid) -> Result<Option<PairedDevice>> {
        self.read(|connection| device_where(connection, "id = ?1", DbUuid(id)))
    }

    /// Revokes the phone; its handover receipts go with it (the foreign key
    /// cascades).
    pub fn delete_paired_device(&self, id: Uuid) -> Result<()> {
        self.write(|transaction| {
            transaction.execute("DELETE FROM pairedDevice WHERE id = ?1", [DbUuid(id)])?;
            Ok(())
        })
    }

    /// The receipt of `recording_id`.
    pub fn handover_receipt(&self, recording_id: Uuid) -> Result<Option<HandoverReceipt>> {
        self.read(|connection| {
            Ok(connection
                .query_row(
                    &format!(
                        "SELECT {RECEIPT_COLUMNS} FROM handoverReceipt WHERE recordingID = ?1"
                    ),
                    [DbUuid(recording_id)],
                    receipt_from_row,
                )
                .optional()?)
        })
    }

    /// Inserts or replaces the receipt (GRDB's `save`).
    pub fn save_handover_receipt(&self, receipt: &HandoverReceipt) -> Result<()> {
        let kind = receipt.state.kind();
        let failure_message = match &receipt.state {
            HandoverState::Failed(message) => Some(message.as_str()),
            _ => None,
        };
        self.write(|transaction| {
            transaction.execute(
                &upsert_sql("handoverReceipt", RECEIPT_COLUMNS),
                params![
                    DbUuid(receipt.recording_id),
                    DbUuid(receipt.device_id),
                    DbEnum(kind),
                    receipt.state.meeting_id().map(DbUuid),
                    failure_message,
                    receipt.byte_count,
                    receipt.sha256,
                    receipt.chunk_size,
                    DbJson(&receipt.received_chunks),
                    DbDate(receipt.created_at),
                    DbDate(receipt.updated_at),
                ],
            )?;
            Ok(())
        })
    }
}
