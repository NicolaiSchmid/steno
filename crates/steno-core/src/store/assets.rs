use rusqlite::{Connection, OptionalExtension, Row, params};
use uuid::Uuid;

use super::convert::{DbDate, DbEnum, DbJson, DbUuid, Unwrap as _};
use super::{Result, Store};
use crate::model::{AudioAsset, AudioRetention};

const COLUMNS: &str = "id, meetingID, url, format, lanes, sidecars16k, mixdownURL, retention, \
     retentionDays, expiresAt";

fn from_row(row: &Row<'_>) -> rusqlite::Result<AudioAsset> {
    let kind: DbEnum<_> = row.get("retention")?;
    let days: Option<i64> = row.get("retentionDays")?;
    Ok(AudioAsset {
        id: row.get::<_, DbUuid>("id")?.0,
        meeting_id: row.get::<_, DbUuid>("meetingID")?.0,
        url: row.get("url")?,
        format: row.get::<_, DbEnum<_>>("format")?.0,
        lanes: row.get::<_, DbJson<_>>("lanes")?.0,
        sidecars_16k: row.get::<_, DbJson<_>>("sidecars16k")?.0,
        mixdown_url: row.get("mixdownURL")?,
        retention: AudioRetention::from_columns(kind.0, days),
        expires_at: row.get::<_, Option<DbDate>>("expiresAt")?.unwrap_db(),
    })
}

pub(super) fn save(connection: &Connection, asset: &AudioAsset) -> Result<()> {
    connection.execute(
        &format!(
            "INSERT INTO audioAsset ({COLUMNS}) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10) \
             ON CONFLICT(id) DO UPDATE SET meetingID = excluded.meetingID, url = excluded.url, \
             format = excluded.format, lanes = excluded.lanes, sidecars16k = excluded.sidecars16k, \
             mixdownURL = excluded.mixdownURL, retention = excluded.retention, \
             retentionDays = excluded.retentionDays, expiresAt = excluded.expiresAt"
        ),
        params![
            DbUuid(asset.id),
            DbUuid(asset.meeting_id),
            asset.url,
            DbEnum(asset.format),
            DbJson(&asset.lanes),
            DbJson(&asset.sidecars_16k),
            asset.mixdown_url,
            DbEnum(asset.retention.kind()),
            asset.retention.days(),
            asset.expires_at.map(DbDate),
        ],
    )?;
    Ok(())
}

pub(super) fn for_meeting(connection: &Connection, meeting_id: Uuid) -> Result<Vec<AudioAsset>> {
    let mut statement = connection.prepare(&format!(
        "SELECT {COLUMNS} FROM audioAsset WHERE meetingID = ?1 ORDER BY id"
    ))?;
    let rows = statement.query_map([DbUuid(meeting_id)], from_row)?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

impl Store {
    pub fn save_asset(&self, asset: &AudioAsset) -> Result<()> {
        self.write(|transaction| save(transaction, asset))
    }

    /// The meeting's asset (the first by id when there are several).
    pub fn asset(&self, meeting_id: Uuid) -> Result<Option<AudioAsset>> {
        self.read(|connection| {
            Ok(connection
                .query_row(
                    &format!(
                        "SELECT {COLUMNS} FROM audioAsset WHERE meetingID = ?1 ORDER BY id LIMIT 1"
                    ),
                    [DbUuid(meeting_id)],
                    from_row,
                )
                .optional()?)
        })
    }

    /// Every asset, in id order; the retention sweep pairs them with the
    /// files on disk.
    pub fn assets(&self) -> Result<Vec<AudioAsset>> {
        self.read(|connection| {
            let mut statement =
                connection.prepare(&format!("SELECT {COLUMNS} FROM audioAsset ORDER BY id"))?;
            let rows = statement.query_map([], from_row)?;
            Ok(rows.collect::<rusqlite::Result<_>>()?)
        })
    }
}
