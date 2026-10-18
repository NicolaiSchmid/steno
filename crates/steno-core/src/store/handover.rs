//! `pairedDevice`, `handoverReceipt` and `handoverAdmission` rows: the
//! phones paired with this computer, where each phone recording's handover
//! stands, and the ledger of the recordings admitted.

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

/// What an admission's commit wrote ([`Store::commit_admission_durably`],
/// [`Store::adopt_upload_durably`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdmissionCommit {
    /// The meeting, its asset and the ledger row.
    Wrote,
    /// None of them: the ledger already held the bytes, as `meeting_id`.
    Earlier {
        meeting_id: Uuid,
        /// The user deleted that meeting. Only the launch's adoption
        /// answers so.
        deleted: bool,
    },
}

impl Store {
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

    /// Inserts or replaces the device. `token_hash` is the SHA-256 of the
    /// bearer token, never the token. Durable: the phone keeps the token
    /// from the answer that follows, and a rolled-back pairing would unpair
    /// it.
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
    /// An `UPDATE`, not a save, so a revoke between the read and the touch
    /// stays a revoke.
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

    pub fn paired_device_for_token_hash(&self, token_hash: &[u8]) -> Result<Option<PairedDevice>> {
        self.read(|connection| device_where(connection, "tokenHash = ?1", token_hash))
    }

    pub fn paired_device(&self, id: Uuid) -> Result<Option<PairedDevice>> {
        self.read(|connection| device_where(connection, "id = ?1", DbUuid(id)))
    }

    /// Revokes the phone; its receipts cascade. Durable, so a power loss
    /// cannot bring a revoked phone back.
    pub fn delete_paired_device(&self, id: Uuid) -> Result<()> {
        self.write_durably(|transaction| {
            transaction.execute("DELETE FROM pairedDevice WHERE id = ?1", [DbUuid(id)])?;
            Ok(())
        })
    }

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

    pub fn save_handover_receipt(&self, receipt: &HandoverReceipt) -> Result<()> {
        self.write(|transaction| save_receipt(transaction, receipt))
    }

    /// [`Store::save_handover_receipt`], durable: the intake's `failed`
    /// receipt after a failed admission commit, whose frames may still sit
    /// in the WAL. This commit writes over them, so no restart brings the
    /// admission back.
    pub fn save_handover_receipt_durably(&self, receipt: &HandoverReceipt) -> Result<()> {
        self.write_durably(|transaction| save_receipt(transaction, receipt))
    }

    /// The phone intake's admission: the `complete` receipt, the meeting,
    /// its asset and the ledger row ([`Store::admitted_meeting`]) in one
    /// durable transaction. Returns the meeting the receipt was completed
    /// with. The phone deletes its copy once `complete` answers 200, so no
    /// commit may hold the receipt without the meeting.
    ///
    /// When the ledger already holds these bytes (recording id, size and
    /// SHA-256) and their meeting exists, the receipt is completed with
    /// that meeting and nothing else is written: the same bytes are one
    /// recording. When the user deleted that meeting, `meeting` is written
    /// and the ledger row moves to it.
    ///
    /// Fails with [`StoreError::ReceiptOfAnotherUpload`], writing nothing,
    /// when the stored receipt belongs to another device or holds other
    /// bytes: the phone would otherwise delete a recording never admitted.
    pub fn save_admission_durably(
        &self,
        receipt: &HandoverReceipt,
        meeting: &Meeting,
        asset: &AudioAsset,
    ) -> Result<Uuid> {
        Ok(
            match self.commit_admission_durably(receipt, meeting, asset)? {
                AdmissionCommit::Wrote => meeting.id,
                AdmissionCommit::Earlier { meeting_id, .. } => meeting_id,
            },
        )
    }

    /// [`Store::save_admission_durably`], saying whether it wrote `meeting`
    /// or the ledger answered. Fails with [`StoreError::MeetingExists`],
    /// writing nothing, when the ledger does not answer and `meeting`
    /// already has a row (the launch adopted the copy without its ledger
    /// row): an upsert would give the one master a second asset row.
    pub fn commit_admission_durably(
        &self,
        receipt: &HandoverReceipt,
        meeting: &Meeting,
        asset: &AudioAsset,
    ) -> Result<AdmissionCommit> {
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
            let earlier = transaction
                .query_row(
                    "SELECT meetingID FROM handoverAdmission \
                     WHERE recordingID = ?1 AND byteCount = ?2 AND sha256 = ?3 \
                     AND meetingID IN (SELECT id FROM meeting)",
                    params![
                        DbUuid(receipt.recording_id),
                        receipt.byte_count,
                        receipt.sha256
                    ],
                    |row| row.col::<DbUuid>("meetingID"),
                )
                .optional()?;
            if let Some(meeting_id) = earlier {
                let completed = HandoverReceipt {
                    state: HandoverState::Complete { meeting_id },
                    ..receipt.clone()
                };
                save_receipt(transaction, &completed)?;
                return Ok(AdmissionCommit::Earlier {
                    meeting_id,
                    deleted: false,
                });
            }
            if meetings::has_row(transaction, meeting.id)? {
                return Err(StoreError::MeetingExists(meeting.id));
            }
            meetings::save(transaction, meeting)?;
            assets::save(transaction, asset)?;
            save_receipt(transaction, receipt)?;
            save_admission_row(
                transaction,
                receipt.recording_id,
                receipt.byte_count,
                &receipt.sha256,
                meeting.id,
                receipt.updated_at,
            )?;
            Ok(AdmissionCommit::Wrote)
        })
    }

    /// The launch's adoption of a copy a phone admission left with no
    /// meeting: `meeting`, `asset` and the ledger row in one durable
    /// transaction, so the phone's retry is answered from that row. When
    /// the ledger already holds those bytes, nothing is written and the
    /// row's meeting id comes back with whether that meeting is gone; the
    /// caller then removes the copy, since the user deleted those bytes.
    /// Fails with [`StoreError::MeetingExists`] when `meeting` has a row.
    /// The receipt is left as it is.
    pub fn adopt_upload_durably(
        &self,
        meeting: &Meeting,
        asset: &AudioAsset,
        recording_id: Uuid,
        byte_count: i64,
        sha256: &[u8],
    ) -> Result<AdmissionCommit> {
        self.write_durably(|transaction| {
            let earlier = transaction
                .query_row(
                    "SELECT meetingID FROM handoverAdmission \
                     WHERE recordingID = ?1 AND byteCount = ?2 AND sha256 = ?3",
                    params![DbUuid(recording_id), byte_count, sha256],
                    |row| row.col::<DbUuid>("meetingID"),
                )
                .optional()?;
            if let Some(meeting_id) = earlier {
                return Ok(AdmissionCommit::Earlier {
                    meeting_id,
                    deleted: !meetings::has_row(transaction, meeting_id)?,
                });
            }
            if meetings::has_row(transaction, meeting.id)? {
                return Err(StoreError::MeetingExists(meeting.id));
            }
            meetings::save(transaction, meeting)?;
            assets::save(transaction, asset)?;
            save_admission_row(
                transaction,
                recording_id,
                byte_count,
                sha256,
                meeting.id,
                meeting.created_at,
            )?;
            Ok(AdmissionCommit::Wrote)
        })
    }

    /// The meeting the phone recording `recording_id` of `byte_count` bytes
    /// hashing to `sha256` was admitted as, from the ledger. A row is never
    /// deleted (not by a revoke, nor a meeting delete), so the meeting may
    /// be gone. The handover answers a phone announcing those bytes again
    /// "delivered", so it does not upload a second meeting.
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

    /// Writes the ledger row of every `complete` receipt whose meeting
    /// exists and that has none yet: admissions before schema v5, and
    /// those an older app committed during a rollback (it ignores v5).
    /// [`Store::open`] runs it every time.
    pub(crate) fn backfill_handover_admissions(&self) -> Result<()> {
        self.write(|transaction| {
            transaction.execute_batch(BACKFILL_ADMISSIONS)?;
            Ok(())
        })
    }
}

/// [`Store::backfill_handover_admissions`]; the Swift store's backfill runs
/// the same text.
const BACKFILL_ADMISSIONS: &str = "INSERT OR IGNORE INTO \"handoverAdmission\" \
     (\"recordingID\", \"byteCount\", \"sha256\", \"meetingID\", \"admittedAt\") \
     SELECT \"recordingID\", \"byteCount\", \"sha256\", \"meetingID\", \"updatedAt\" \
     FROM \"handoverReceipt\" \
     WHERE \"state\" = 'complete' \
     AND \"meetingID\" IN (SELECT \"id\" FROM \"meeting\")";

/// The ledger row of the given bytes, admitted as `meeting_id` at `at`. A
/// row whose meeting still exists stays; one whose meeting is gone moves.
fn save_admission_row(
    connection: &Connection,
    recording_id: Uuid,
    byte_count: i64,
    sha256: &[u8],
    meeting_id: Uuid,
    at: DateTime<Utc>,
) -> Result<()> {
    connection.execute(
        "INSERT INTO handoverAdmission \
         (recordingID, byteCount, sha256, meetingID, admittedAt) \
         VALUES (?1, ?2, ?3, ?4, ?5) \
         ON CONFLICT (recordingID, byteCount, sha256) DO UPDATE \
         SET meetingID = excluded.meetingID, admittedAt = excluded.admittedAt \
         WHERE meetingID NOT IN (SELECT id FROM meeting)",
        params![
            DbUuid(recording_id),
            byte_count,
            sha256,
            DbUuid(meeting_id),
            DbDate(at),
        ],
    )?;
    Ok(())
}

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

    /// A pairing and a revoke commit under `synchronous = FULL`.
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
