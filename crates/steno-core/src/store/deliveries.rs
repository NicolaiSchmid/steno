//! `delivery` rows.
//! Swift: the delivery methods of `Sources/StenoCore/Storage/MeetingStore.swift`.

use chrono::{DateTime, Utc};
use rusqlite::{Connection, Row, params};
use uuid::Uuid;

use super::convert::{DbDate, DbEnum, DbJson, DbUuid, RowExt as _};
use super::{Result, Store, execute_cached, query_all, upsert_sql};
use crate::model::{Delivery, DeliveryStatus, DeliveryStatusKind, MeetingStateKind};

const COLUMNS: &str =
    "id, meetingID, destinationID, status, failureMessage, lastAttemptAt, receipt";

fn from_row(row: &Row<'_>) -> rusqlite::Result<Delivery> {
    let kind = row.col::<DbEnum<_>>("status")?;
    let failure_message = row.get("failureMessage")?;
    Ok(Delivery {
        id: row.col::<DbUuid>("id")?,
        meeting_id: row.col::<DbUuid>("meetingID")?,
        destination_id: row.get("destinationID")?,
        status: DeliveryStatus::from_columns(kind, failure_message),
        last_attempt_at: row.col::<Option<DbDate>>("lastAttemptAt")?,
        receipt: row.col::<Option<DbJson<_>>>("receipt")?,
    })
}

pub(super) fn save(connection: &Connection, delivery: &Delivery) -> Result<()> {
    connection.execute(
        &upsert_sql("delivery", COLUMNS),
        params![
            DbUuid(delivery.id),
            DbUuid(delivery.meeting_id),
            delivery.destination_id,
            DbEnum(delivery.status.kind()),
            delivery.status.failure_message(),
            delivery.last_attempt_at.map(DbDate),
            delivery.receipt.as_ref().map(DbJson),
        ],
    )?;
    Ok(())
}

impl Store {
    /// One row per (meeting, destination); the id derives from the pair, so
    /// this is a plain upsert.
    pub fn save_delivery(&self, delivery: &Delivery) -> Result<()> {
        self.write(|transaction| save(transaction, delivery))
    }

    /// Removes the rows with `ids`; ids without a row are ignored. The
    /// dispatcher drops the rows of a destination that is no longer
    /// configured with this.
    pub fn delete_deliveries(&self, ids: &[Uuid]) -> Result<()> {
        if ids.is_empty() {
            return Ok(());
        }
        self.write(|transaction| {
            for id in ids {
                execute_cached(
                    transaction,
                    "DELETE FROM delivery WHERE id = ?1",
                    [DbUuid(*id)],
                )?;
            }
            Ok(())
        })
    }

    /// The ready meetings with an export left unfinished, oldest first: a
    /// delivery still `pending` (the process ended while it ran) or one
    /// that `failed` with its last attempt before `attempted_before` (or
    /// none). The launch delivers these again. Rust only: Swift retried a
    /// failed export only when asked.
    pub fn meetings_with_unfinished_deliveries(
        &self,
        attempted_before: DateTime<Utc>,
    ) -> Result<Vec<Uuid>> {
        self.read(|connection| {
            query_all(
                connection,
                "SELECT meeting.id AS id FROM meeting WHERE meeting.state = ?1 AND EXISTS ( \
                 SELECT 1 FROM delivery WHERE delivery.meetingID = meeting.id AND ( \
                 delivery.status = ?2 OR (delivery.status = ?3 AND \
                 (delivery.lastAttemptAt IS NULL OR delivery.lastAttemptAt < ?4)))) \
                 ORDER BY meeting.startedAt, meeting.id",
                params![
                    MeetingStateKind::Ready.as_str(),
                    DeliveryStatusKind::Pending.as_str(),
                    DeliveryStatusKind::Failed.as_str(),
                    DbDate(attempted_before),
                ],
                |row| row.col::<DbUuid>("id"),
            )
        })
    }

    /// The meeting's deliveries by destination id.
    pub fn deliveries(&self, meeting_id: Uuid) -> Result<Vec<Delivery>> {
        self.read(|connection| {
            query_all(
                connection,
                &format!(
                    "SELECT {COLUMNS} FROM delivery WHERE meetingID = ?1 ORDER BY destinationID"
                ),
                [DbUuid(meeting_id)],
                from_row,
            )
        })
    }
}
