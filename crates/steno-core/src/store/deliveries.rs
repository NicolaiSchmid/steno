use rusqlite::{Connection, Row, params};
use uuid::Uuid;

use super::convert::{DbDate, DbEnum, DbJson, DbUuid, Unwrap as _};
use super::{Result, Store};
use crate::model::{Delivery, DeliveryStatus};

const COLUMNS: &str =
    "id, meetingID, destinationID, status, failureMessage, lastAttemptAt, receipt";

fn from_row(row: &Row<'_>) -> rusqlite::Result<Delivery> {
    let kind: DbEnum<_> = row.get("status")?;
    let failure_message: Option<String> = row.get("failureMessage")?;
    Ok(Delivery {
        id: row.get::<_, DbUuid>("id")?.0,
        meeting_id: row.get::<_, DbUuid>("meetingID")?.0,
        destination_id: row.get("destinationID")?,
        status: DeliveryStatus::from_columns(kind.0, failure_message),
        last_attempt_at: row.get::<_, Option<DbDate>>("lastAttemptAt")?.unwrap_db(),
        receipt: row.get::<_, Option<DbJson<_>>>("receipt")?.unwrap_db(),
    })
}

pub(super) fn save(connection: &Connection, delivery: &Delivery) -> Result<()> {
    connection.execute(
        &format!(
            "INSERT INTO delivery ({COLUMNS}) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7) \
             ON CONFLICT(id) DO UPDATE SET meetingID = excluded.meetingID, \
             destinationID = excluded.destinationID, status = excluded.status, \
             failureMessage = excluded.failureMessage, lastAttemptAt = excluded.lastAttemptAt, \
             receipt = excluded.receipt"
        ),
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
            let mut statement = connection.prepare(&format!(
                "SELECT {COLUMNS} FROM delivery WHERE meetingID = ?1 ORDER BY destinationID"
            ))?;
            let rows = statement.query_map([DbUuid(meeting_id)], from_row)?;
            Ok(rows.collect::<rusqlite::Result<_>>()?)
        })
    }
}
