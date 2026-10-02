//! `delivery` rows.
//! Swift: the delivery methods of `Sources/StenoCore/Storage/MeetingStore.swift`.

use rusqlite::{Connection, Row, params};
use uuid::Uuid;

use super::convert::{DbDate, DbEnum, DbJson, DbUuid, RowExt as _};
use super::{Result, Store, query_all, upsert_sql};
use crate::model::{Delivery, DeliveryStatus};

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
