//! `audioAsset` rows.
//! Swift: the asset methods of `Sources/StenoCore/Storage/MeetingStore.swift`.

use rusqlite::{Connection, OptionalExtension, Row, params};
use uuid::Uuid;

use super::convert::{DbDate, DbEnum, DbJson, DbUuid, RowExt as _};
use super::{Result, Store, query_all, upsert_sql};
use crate::model::{AudioAsset, AudioRetention};

const COLUMNS: &str = "id, meetingID, url, format, lanes, sidecars16k, mixdownURL, retention, \
     retentionDays, expiresAt";

fn from_row(row: &Row<'_>) -> rusqlite::Result<AudioAsset> {
    let kind = row.col::<DbEnum<_>>("retention")?;
    let days = row.get("retentionDays")?;
    Ok(AudioAsset {
        id: row.col::<DbUuid>("id")?,
        meeting_id: row.col::<DbUuid>("meetingID")?,
        url: row.get("url")?,
        format: row.col::<DbEnum<_>>("format")?,
        lanes: row.col::<DbJson<_>>("lanes")?,
        sidecars_16k: row.col::<DbJson<_>>("sidecars16k")?,
        mixdown_url: row.get("mixdownURL")?,
        retention: AudioRetention::from_columns(kind, days),
        expires_at: row.col::<Option<DbDate>>("expiresAt")?,
    })
}

pub(super) fn save(connection: &Connection, asset: &AudioAsset) -> Result<()> {
    connection.execute(
        &upsert_sql("audioAsset", COLUMNS),
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

pub(super) fn assets_of_meeting(
    connection: &Connection,
    meeting_id: Uuid,
) -> Result<Vec<AudioAsset>> {
    query_all(
        connection,
        &format!("SELECT {COLUMNS} FROM audioAsset WHERE meetingID = ?1 ORDER BY id"),
        [DbUuid(meeting_id)],
        from_row,
    )
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
            query_all(
                connection,
                &format!("SELECT {COLUMNS} FROM audioAsset ORDER BY id"),
                [],
                from_row,
            )
        })
    }
}
