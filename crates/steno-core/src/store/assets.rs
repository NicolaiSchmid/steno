//! `audioAsset` rows.

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

/// When the asset `a` of the meeting `m` is due for the sweep: stamped,
/// expired at `?1`, and its meeting not recording, queued or processing.
const DUE: &str = "a.expiresAt IS NOT NULL AND a.expiresAt <= ?1 \
                   AND m.state NOT IN ('recording', 'queued', 'processing')";

/// The select of [`Store::expired_assets`], narrowed by `filter`: `?1` is
/// the time.
fn expired_sql(filter: &str) -> String {
    format!(
        "SELECT {} FROM audioAsset a JOIN meeting m ON m.id = a.meetingID \
         WHERE {DUE}{filter} ORDER BY a.expiresAt, a.id",
        COLUMNS
            .split(", ")
            .map(|column| format!("a.{column}"))
            .collect::<Vec<_>>()
            .join(", ")
    )
}

/// The id of another asset row that names `asset`'s master and is not
/// [`DUE`] at `now` itself, if any: it still needs that file.
fn keeper_of_master(
    connection: &Connection,
    asset: &AudioAsset,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<Option<Uuid>> {
    Ok(connection
        .query_row(
            &format!(
                "SELECT a.id FROM audioAsset a JOIN meeting m ON m.id = a.meetingID \
                 WHERE a.url = ?2 AND a.id != ?3 AND NOT ({DUE}) ORDER BY a.id LIMIT 1"
            ),
            params![DbDate(now), asset.url, DbUuid(asset.id)],
            |row| row.col::<DbUuid>("id"),
        )
        .optional()?)
}

/// What [`Store::sweep_expired_asset`] hands its `remove`, read in the
/// write that removes the files.
#[derive(Debug, Clone, PartialEq)]
pub struct ExpiredAsset {
    pub asset: AudioAsset,
    pub speakers: Vec<Speaker>,
    /// Every `sampleClipURL` a speaker row names, of every meeting
    /// ([`Store::sample_clip_urls`]).
    pub sample_clip_urls: Vec<String>,
}

impl ExpiredAsset {
    /// The meeting's confirmed speakers with a sample clip: their clips go
    /// with the audio. Unconfirmed speakers keep theirs.
    pub fn confirmed_with_clips(&self) -> impl Iterator<Item = &Speaker> {
        self.speakers.iter().filter(|speaker| {
            speaker.assignment.is_confirmed() && speaker.sample_clip_url.is_some()
        })
    }
}

impl Store {
    pub fn save_asset(&self, asset: &AudioAsset) -> Result<()> {
        self.write(|transaction| save(transaction, asset))
    }

    /// [`Store::save_asset`], durable ([`Store::write_durably`]). Every stamp
    /// and every keep goes this way: once a stamp is on disk the sweep may
    /// delete the recording, and the durable commit also syncs the results
    /// before it, so a power loss never leaves the recording deleted and its
    /// results rolled back; and a power loss never brings back a stamp the
    /// user's keep removed.
    pub fn save_asset_durably(&self, asset: &AudioAsset) -> Result<()> {
        self.write_durably(|transaction| save(transaction, asset))
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

    /// Every asset, in id order.
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

    /// Every asset's `url`, each once and in order: the launch's recovery
    /// looks in every folder a master was written to.
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
    pub fn expired_assets(&self, now: chrono::DateTime<chrono::Utc>) -> Result<Vec<AudioAsset>> {
        self.read(|connection| query_all(connection, &expired_sql(""), [DbDate(now)], from_row))
    }

    /// Sweeps one asset of [`Store::expired_assets`] in one write. While it
    /// is still due at `now`, `remove` gets the [`ExpiredAsset`], removes
    /// its files and the clips of [`ExpiredAsset::confirmed_with_clips`],
    /// and returns whether all went; then those speakers lose
    /// `sampleClipURL` and the asset `expiresAt`. Returns whether the asset
    /// was swept.
    ///
    /// An asset whose master another, not due, row names is skipped, so the
    /// master goes once every row naming it is due; a master known to be
    /// gone needs no keeper. The write lock is held from the check through
    /// the removal, so a run saved `queued` meanwhile comes either before
    /// the check or after the files are gone.
    pub fn sweep_expired_asset(
        &self,
        asset_id: Uuid,
        now: chrono::DateTime<chrono::Utc>,
        remove: impl FnOnce(&ExpiredAsset) -> bool,
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
            // Only a master known to be gone skips the keeper check: one
            // whose metadata cannot be read may still be there.
            let master_not_gone = crate::paths::file_url_path(&asset.url)
                .is_some_and(|master| !matches!(master.try_exists(), Ok(false)));
            if master_not_gone && let Some(keeper) = keeper_of_master(transaction, &asset, now)? {
                tracing::info!(
                    asset_id = %asset.id,
                    meeting_id = %asset.meeting_id,
                    %keeper,
                    "an expired recording is kept: another asset row still needs its master"
                );
                return Ok(false);
            }
            let expired = ExpiredAsset {
                speakers: people::speakers_of_meeting(transaction, asset.meeting_id)?,
                sample_clip_urls: people::sample_clip_urls(transaction)?,
                asset,
            };
            if !remove(&expired) {
                return Ok(false);
            }
            let ids: Vec<Uuid> = expired.confirmed_with_clips().map(|s| s.id).collect();
            people::clear_sample_clips(transaction, expired.asset.meeting_id, &ids)?;
            transaction.execute(
                "UPDATE audioAsset SET expiresAt = NULL WHERE id = ?1",
                [DbUuid(expired.asset.id)],
            )?;
            Ok(true)
        })
    }

    /// Marks every listed asset `keepForever` with `expiresAt` cleared, in
    /// one durable write, so a power loss never brings back a stamp.
    pub fn keep_forever(&self, asset_ids: &[Uuid]) -> Result<()> {
        self.write_durably(|transaction| {
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
