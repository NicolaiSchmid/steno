//! `pairedDevice`, `handoverReceipt` and `handoverAdmission` rows: the
//! phones paired with this computer, where each phone recording's handover
//! stands, and the ledger of the recordings admitted.
//! Swift: `Sources/StenoCore/Storage/MeetingStore+Handover.swift`.

use chrono::{DateTime, Utc};
use rusqlite::{Connection, OptionalExtension, Row, params};
use uuid::Uuid;

use super::convert::{DbDate, DbEnum, DbJson, DbUuid, RowExt as _};
use super::{Result, Store, StoreError, assets, meetings, query_all, upsert_sql};
use crate::model::{AudioAsset, HandoverReceipt, HandoverState, Meeting, PairedDevice};

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
    /// SHA-256 of the bearer token; the token itself is never stored. On
    /// the disk when it returns ([`Store::write_durably`]): the phone keeps
    /// the token from the answer that follows, and a pairing a power loss
    /// rolled back would unpair it.
    pub fn save_paired_device(&self, device: &PairedDevice, token_hash: &[u8]) -> Result<()> {
        self.write_durably(|transaction| {
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
    /// is gone, or whose token changed, is left alone. Swift:
    /// `MeetingStore.touchPairedDevice`.
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
    /// cascades). On the disk when it returns ([`Store::write_durably`]), so
    /// a power loss cannot bring a revoked phone back.
    pub fn delete_paired_device(&self, id: Uuid) -> Result<()> {
        self.write_durably(|transaction| {
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
        self.write(|transaction| save_receipt(transaction, receipt))
    }

    /// [`Store::save_handover_receipt`], on the disk when it returns
    /// ([`Store::write_durably`]): the phone intake's `failed` receipt
    /// after a failed admission commit, whose frames may still sit in the
    /// WAL for recovery to replay. This commit writes over them, or voids
    /// them when the WAL restarts, so once it returns no restart brings
    /// the admission back. Swift: `MeetingStore.saveDurably(_:)`.
    pub fn save_handover_receipt_durably(&self, receipt: &HandoverReceipt) -> Result<()> {
        self.write_durably(|transaction| save_receipt(transaction, receipt))
    }

    /// The phone intake's admission: the `complete` receipt, the meeting,
    /// its asset and the admission's ledger row (see
    /// [`Store::admitted_meeting`]) in one transaction, on the disk when it
    /// returns ([`Store::write_durably`]). The phone deletes its copy once
    /// `complete` answers 200, so no commit may hold the receipt without
    /// the meeting, and a power loss must not roll either back. A ledger
    /// row of the same recording id, size and SHA-256 stays as it is
    /// (`INSERT OR IGNORE`): the first admission of those bytes stands.
    /// Fails with [`StoreError::ReceiptOfAnotherUpload`], writing nothing,
    /// when the stored receipt belongs to another device than `receipt` or
    /// holds another size or SHA-256: completed, it would answer that
    /// device's `complete`, or the `complete` of the other bytes, with this
    /// meeting, and the phone would delete a recording never admitted.
    /// Swift: `MeetingStore.saveDurably(_:meeting:asset:)`.
    pub fn save_admission_durably(
        &self,
        receipt: &HandoverReceipt,
        meeting: &Meeting,
        asset: &AudioAsset,
    ) -> Result<()> {
        self.write_durably(|transaction| {
            let stored = transaction
                .query_row(
                    "SELECT deviceID, byteCount, sha256 FROM handoverReceipt \
                     WHERE recordingID = ?1",
                    [DbUuid(receipt.recording_id)],
                    |row| {
                        Ok((
                            row.col::<DbUuid>("deviceID")?,
                            row.get::<_, i64>("byteCount")?,
                            row.get::<_, Vec<u8>>("sha256")?,
                        ))
                    },
                )
                .optional()?;
            if stored.is_some_and(|(device_id, byte_count, sha256)| {
                (device_id, byte_count, sha256.as_slice())
                    != (
                        receipt.device_id,
                        receipt.byte_count,
                        receipt.sha256.as_slice(),
                    )
            }) {
                return Err(StoreError::ReceiptOfAnotherUpload(receipt.recording_id));
            }
            meetings::save(transaction, meeting)?;
            assets::save(transaction, asset)?;
            save_receipt(transaction, receipt)?;
            transaction.execute(
                "INSERT OR IGNORE INTO handoverAdmission \
                 (recordingID, byteCount, sha256, meetingID, admittedAt) \
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    DbUuid(receipt.recording_id),
                    receipt.byte_count,
                    receipt.sha256,
                    DbUuid(meeting.id),
                    DbDate(receipt.updated_at),
                ],
            )?;
            Ok(())
        })
    }

    /// The meeting the phone recording `recording_id` of `byte_count` bytes
    /// hashing to `sha256` was admitted as, from the admission ledger
    /// (`handoverAdmission`, schema v5). A row is written in the
    /// admission's own transaction ([`Store::save_admission_durably`]) and
    /// never deleted: not by a revoke, whose cascade takes the receipts,
    /// nor by a meeting delete, so the meeting may be gone. The handover
    /// answers a phone that announces those bytes again "delivered" from
    /// it, so the phone deletes its copy instead of uploading it as a
    /// second meeting. Swift: `MeetingStore.admittedMeeting`.
    pub fn admitted_meeting(
        &self,
        recording_id: Uuid,
        byte_count: i64,
        sha256: &[u8],
    ) -> Result<Option<Uuid>> {
        self.read(|connection| {
            Ok(connection
                .query_row(
                    "SELECT meetingID FROM handoverAdmission \
                     WHERE recordingID = ?1 AND byteCount = ?2 AND sha256 = ?3",
                    params![DbUuid(recording_id), byte_count, sha256],
                    |row| row.col::<DbUuid>("meetingID"),
                )
                .optional()?)
        })
    }

    /// Writes the ledger row of every `complete` receipt whose meeting row
    /// exists and that has none yet: the admissions before schema v5, and
    /// those an older app committed during a rollback, since it ignores v5
    /// and writes no row. [`Store::open`] runs it every time. Swift: the
    /// backfill in `MeetingStore.init`.
    pub fn backfill_handover_admissions(&self) -> Result<()> {
        self.write(|transaction| {
            transaction.execute_batch(BACKFILL_ADMISSIONS)?;
            Ok(())
        })
    }
}

/// [`Store::backfill_handover_admissions`]; the Swift backfill runs the same
/// text.
const BACKFILL_ADMISSIONS: &str = "INSERT OR IGNORE INTO \"handoverAdmission\" \
     (\"recordingID\", \"byteCount\", \"sha256\", \"meetingID\", \"admittedAt\") \
     SELECT \"recordingID\", \"byteCount\", \"sha256\", \"meetingID\", \"updatedAt\" \
     FROM \"handoverReceipt\" \
     WHERE \"state\" = 'complete' \
     AND \"meetingID\" IN (SELECT \"id\" FROM \"meeting\")";

fn save_receipt(connection: &Connection, receipt: &HandoverReceipt) -> Result<()> {
    let kind = receipt.state.kind();
    let failure_message = match &receipt.state {
        HandoverState::Failed(message) => Some(message.as_str()),
        _ => None,
    };
    connection.execute(
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
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use chrono::Utc;

    use super::*;

    /// A pairing and a revoke commit under `synchronous = FULL` (2): the phone
    /// keeps the token from the pairing's answer, so a power loss must not
    /// forget the pairing, nor bring a revoked phone back. That the commit then
    /// survives a power loss is SQLite's and cannot be tested. Swift:
    /// `aPairingAndARevokeCommitDurably`.
    #[test]
    fn a_pairing_and_a_revoke_commit_durably() {
        let directory = tempfile::tempdir().unwrap();
        let store = Store::open(directory.path().join("steno.sqlite")).unwrap();
        let levels: Arc<Mutex<Vec<i64>>> = Arc::default();
        let seen = levels.clone();
        store.probe_commits(move |connection| {
            let level = connection
                .query_row("PRAGMA synchronous", [], |row| row.get(0))
                .unwrap();
            seen.lock().unwrap().push(level);
        });
        let device = PairedDevice {
            id: Uuid::new_v4(),
            name: "Phone".to_owned(),
            paired_at: Utc::now(),
            last_seen_at: None,
        };

        store.save_paired_device(&device, &[1; 32]).unwrap();
        store
            .touch_paired_device(device.id, &[1; 32], Utc::now())
            .unwrap();
        store.delete_paired_device(device.id).unwrap();

        assert_eq!(
            *levels.lock().unwrap(),
            [2, 1, 2],
            "the pairing and the revoke under FULL, the last-seen touch at NORMAL"
        );
        assert_eq!(store.paired_device(device.id).unwrap(), None);
    }
}
