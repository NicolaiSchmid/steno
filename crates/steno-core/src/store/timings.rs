//! The `stageRate` table: the learned seconds per unit of each pipeline
//! stage. The seeds and the moving average live with the pipeline
//! (`steno-pipeline`); the store only reads and writes rows.
//! Swift: `Sources/StenoCore/Storage/MeetingStore+Timings.swift`.

use chrono::{DateTime, Utc};
use rusqlite::{Row, params};

use super::convert::{DbDate, DbEnum, RowExt as _};
use super::{Result, Store, query_all};
use crate::model::{PipelineStage, StageRate};

/// One row of `stageRate`.
#[derive(Debug, Clone, PartialEq)]
pub struct StageRateRow {
    pub stage: PipelineStage,
    /// The speech engine id, the LLM model, or the empty string.
    pub key: String,
    pub rate: StageRate,
    pub updated_at: DateTime<Utc>,
}

fn from_row(row: &Row<'_>) -> rusqlite::Result<StageRateRow> {
    Ok(StageRateRow {
        stage: row.col::<DbEnum<_>>("stage")?,
        key: row.get("key")?,
        rate: StageRate {
            seconds_per_unit: row.get("secondsPerUnit")?,
            samples: row.get("samples")?,
        },
        updated_at: row.col::<DbDate>("updatedAt")?,
    })
}

impl Store {
    /// Every learned rate, stage order then key. A row whose stage the
    /// enum does not know is skipped, not an error (a newer schema's stage).
    pub fn stage_rates(&self) -> Result<Vec<StageRateRow>> {
        self.read(|connection| {
            let mut statement = connection.prepare(
                "SELECT stage, key, samples, secondsPerUnit, updatedAt FROM stageRate \
                 ORDER BY stage, key",
            )?;
            let rows = statement.query_map([], |row| Ok(from_row(row).ok()))?;
            Ok(rows
                .collect::<rusqlite::Result<Vec<_>>>()?
                .into_iter()
                .flatten()
                .collect())
        })
    }

    /// The one row for `stage` and `key`, `None` before the first sample.
    pub fn stage_rate(&self, stage: PipelineStage, key: &str) -> Result<Option<StageRateRow>> {
        self.read(|connection| {
            let rows = query_all(
                connection,
                "SELECT stage, key, samples, secondsPerUnit, updatedAt FROM stageRate \
                 WHERE stage = ?1 AND key = ?2",
                params![DbEnum(stage), key],
                from_row,
            )?;
            Ok(rows.into_iter().next())
        })
    }

    /// Folds one measurement into its row inside one write: `fold` sees
    /// the stored rate (`None` before the first sample) and returns the
    /// rate to keep. Swift: `MeetingStore.record`.
    pub fn update_stage_rate(
        &self,
        stage: PipelineStage,
        key: &str,
        updated_at: DateTime<Utc>,
        fold: impl FnOnce(Option<StageRate>) -> StageRate,
    ) -> Result<StageRate> {
        self.write(|transaction| {
            let current = query_all(
                transaction,
                "SELECT stage, key, samples, secondsPerUnit, updatedAt FROM stageRate \
                 WHERE stage = ?1 AND key = ?2",
                params![DbEnum(stage), key],
                from_row,
            )?
            .into_iter()
            .next()
            .map(|row| row.rate);
            let rate = fold(current);
            transaction.execute(
                "INSERT INTO stageRate (stage, key, samples, secondsPerUnit, updatedAt) \
                 VALUES (?1, ?2, ?3, ?4, ?5) \
                 ON CONFLICT(stage, key) DO UPDATE SET samples = excluded.samples, \
                 secondsPerUnit = excluded.secondsPerUnit, updatedAt = excluded.updatedAt",
                params![
                    DbEnum(stage),
                    key,
                    rate.samples,
                    rate.seconds_per_unit,
                    DbDate(updated_at)
                ],
            )?;
            Ok(rate)
        })
    }
}
