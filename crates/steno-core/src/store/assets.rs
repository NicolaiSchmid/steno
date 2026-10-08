//! `audioAsset` rows.
//! Swift: the asset methods of `Sources/StenoCore/Storage/MeetingStore.swift`.

use rusqlite::{Connection, OptionalExtension, Row, params};
use uuid::Uuid;

use super::convert::{DbDate, DbEnum, DbJson, DbUuid, RowExt as _};
use super::{Result, Store, people, query_all, upsert_sql};
use crate::model::{AudioAsset, AudioRetention, Speaker};

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

/// The select of [`Store::expired_assets`], narrowed by `filter`: `?1` is
/// the time.
fn expired_sql(filter: &str) -> String {
    format!(
        "SELECT {} FROM audioAsset a JOIN meeting m ON m.id = a.meetingID \
         WHERE a.expiresAt IS NOT NULL AND a.expiresAt <= ?1 \
         AND m.state NOT IN ('recording', 'queued', 'processing'){filter} \
         ORDER BY a.expiresAt, a.id",
        COLUMNS
            .split(", ")
            .map(|column| format!("a.{column}"))
            .collect::<Vec<_>>()
            .join(", ")
    )
}

impl Store {
    /// Inserts or replaces the asset (GRDB's `save`).
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

    /// Every asset's `url`, each once and in order, without the rest of
    /// the row: the launch's recovery looks for an interrupted recording
    /// in every folder a master was written to. Rust only: Swift had no
    /// recovery.
    pub fn asset_urls(&self) -> Result<Vec<String>> {
        self.read(|connection| {
            query_all(
                connection,
                "SELECT DISTINCT url FROM audioAsset ORDER BY url",
                [],
                |row| row.get(0),
            )
        })
    }
}

impl Store {
    /// The asset row by its own id, `None` when there is none.
    pub fn asset_by_id(&self, id: Uuid) -> Result<Option<AudioAsset>> {
        self.read(|connection| {
            Ok(connection
                .query_row(
                    &format!("SELECT {COLUMNS} FROM audioAsset WHERE id = ?1"),
                    [DbUuid(id)],
                    from_row,
                )
                .optional()?)
        })
    }

    /// Every asset whose `expiresAt` has passed and whose meeting is not
    /// recording, queued or processing, in expiry order.
    /// Swift: `MeetingStore.expiredAssets(now:)`.
    pub fn expired_assets(&self, now: chrono::DateTime<chrono::Utc>) -> Result<Vec<AudioAsset>> {
        self.read(|connection| query_all(connection, &expired_sql(""), [DbDate(now)], from_row))
    }

    /// Sweeps one asset of [`Store::expired_assets`] in one write. When the
    /// asset is still due at `now` (stamped, expired, and its meeting not
    /// recording, queued or processing), `remove` gets the asset as stored
    /// and the meeting's confirmed speakers that have a sample clip, removes
    /// their files and returns whether every one went; then those speakers
    /// lose `sampleClipURL` and the asset loses `expiresAt`. Returns whether
    /// the asset was swept. The write lock is held from the check through
    /// the removal, so a run that `enqueue` or `reprocess` saves `queued` in
    /// a write of its own comes either before the check, and the asset is
    /// skipped, or after the files are gone. The store waits on the
    /// removal, a few files on the audio folder's disk. Rust only: Swift's
    /// sweep acts on the list it read first, and Swift has no `reprocess`.
    pub fn sweep_expired_asset(
        &self,
        asset_id: Uuid,
        now: chrono::DateTime<chrono::Utc>,
        remove: impl FnOnce(&AudioAsset, &[Speaker]) -> bool,
    ) -> Result<bool> {
        self.write(|transaction| {
            let Some(asset) = transaction
                .query_row(
                    &expired_sql(" AND a.id = ?2"),
                    params![DbDate(now), DbUuid(asset_id)],
                    from_row,
                )
                .optional()?
            else {
                return Ok(false);
            };
            let confirmed_with_clips: Vec<Speaker> =
                people::speakers_of_meeting(transaction, asset.meeting_id)?
                    .into_iter()
                    .filter(|speaker| {
                        speaker.assignment.is_confirmed() && speaker.sample_clip_url.is_some()
                    })
                    .collect();
            if !remove(&asset, &confirmed_with_clips) {
                return Ok(false);
            }
            let ids: Vec<Uuid> = confirmed_with_clips.iter().map(|s| s.id).collect();
            people::clear_sample_clips(transaction, asset.meeting_id, &ids)?;
            transaction.execute(
                "UPDATE audioAsset SET expiresAt = NULL WHERE id = ?1",
                [DbUuid(asset.id)],
            )?;
            Ok(true)
        })
    }

    /// Marks every listed asset `keepForever` with `expiresAt` cleared, in
    /// one write. Swift: `MeetingStore.keepForever(assetIDs:)`.
    pub fn keep_forever(&self, asset_ids: &[Uuid]) -> Result<()> {
        self.write(|transaction| {
            for id in asset_ids {
                transaction.execute(
                    "UPDATE audioAsset SET retention = ?1, retentionDays = NULL, expiresAt = NULL \
                     WHERE id = ?2",
                    params![DbEnum(AudioRetention::KeepForever.kind()), DbUuid(*id)],
                )?;
            }
            Ok(())
        })
    }
}
