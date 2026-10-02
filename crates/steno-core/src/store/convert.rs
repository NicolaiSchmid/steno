//! Column encodings shared with GRDB: dates as `yyyy-MM-dd HH:mm:ss.SSS`
//! UTC text, UUIDs as uppercase text, JSON columns through `StenoJSON`,
//! string enums by case name, embeddings as little-endian `f32` blobs.
//! Each wrapper is a `ToSql` for `params!` and a `FromSql` that
//! [`RowExt::col`] unwraps again. This is the pattern for a query the
//! `Store` does not have a method for: [`DbUuid`], [`DbDate`], [`DbJson`],
//! [`DbEnum`] and [`DbEmbedding`] in the parameters, [`RowExt::col`] on the
//! row, against the connection [`Store::read`](super::Store::read) or
//! [`Store::write`](super::Store::write) hands out.
//!
//! ```
//! use rusqlite::params;
//! use steno_core::Store;
//! use steno_core::store::convert::{DbDate, DbUuid, RowExt as _};
//!
//! # fn main() -> steno_core::store::Result<()> {
//! let store = Store::in_memory()?;
//! let id = uuid::Uuid::new_v4();
//! // On a whole millisecond, so the column text reads back equal.
//! let created_at = steno_core::json::parse_date("2026-09-29T13:49:11.135Z").unwrap();
//! store.write(|transaction| {
//!     transaction.execute(
//!         "INSERT INTO person (id, displayName, createdAt) VALUES (?1, ?2, ?3)",
//!         params![DbUuid(id), "Anna", DbDate(created_at)],
//!     )?;
//!     Ok(())
//! })?;
//! let (read_id, read_at) = store.read(|connection| {
//!     Ok(connection.query_row(
//!         "SELECT id, createdAt FROM person WHERE id = ?1",
//!         [DbUuid(id)],
//!         |row| Ok((row.col::<DbUuid>("id")?, row.col::<DbDate>("createdAt")?)),
//!     )?)
//! })?;
//! assert_eq!(read_id, id);
//! assert_eq!(read_at, created_at);
//! # Ok(())
//! # }
//! ```
//!
//! Swift: `Sources/StenoCore/Storage/Records.swift` and GRDB's
//! `DatabaseDateEncodingStrategy.deferredToDate`.
//!
//! GRDB writes a `Date` column through Foundation's `DateFormatter` with
//! `yyyy-MM-dd HH:mm:ss.SSS` (GRDB `Date.swift`, not
//! `DatabaseDateComponents`), which rounds to the nearest millisecond;
//! [`date_text`] does the same, measured identical on 10,000 timestamps
//! against the Swift CLI. Swift's `StenoJSON.format`
//! (`Sources/StenoCore/Model/StenoJSON.swift`) truncates instead, so a
//! Swift export of a date that is not on a whole millisecond can read one
//! millisecond lower than the column text. `json::format_date` mirrors the
//! Swift formatter until that is fixed on the Swift side (parity list in
//! `.plans/2026-10-02-rust-core-and-tauri-shell.md`).

use std::fmt::Display;
use std::str::FromStr;

use chrono::{DateTime, NaiveDate, NaiveDateTime, TimeDelta, Utc};
use rusqlite::Row;
use rusqlite::types::{FromSql, FromSqlError, FromSqlResult, ToSql, ToSqlOutput, ValueRef};
use serde::Serialize;
use serde::de::DeserializeOwned;
use uuid::Uuid;

use crate::json;
use crate::model::Embedding;

/// GRDB's storage format for `Date`.
pub const GRDB_DATE_FORMAT: &str = "%Y-%m-%d %H:%M:%S%.3f";

/// `2026-09-29 13:49:11.135`, UTC, rounded to the nearest millisecond
/// (half up) as GRDB's formatter rounds.
#[must_use]
pub fn date_text(date: DateTime<Utc>) -> String {
    let rounded = date + TimeDelta::microseconds(500);
    rounded.format(GRDB_DATE_FORMAT).to_string()
}

/// GRDB's reading of a date column: the storage format first, then the
/// other SQLite date forms with at least year, month and day.
#[must_use]
pub fn parse_date_text(text: &str) -> Option<DateTime<Utc>> {
    const FORMATS: &[&str] = &[
        "%Y-%m-%d %H:%M:%S%.f",
        "%Y-%m-%d %H:%M:%S",
        "%Y-%m-%dT%H:%M:%S%.f",
        "%Y-%m-%dT%H:%M:%S",
        "%Y-%m-%d %H:%M",
        "%Y-%m-%dT%H:%M",
    ];
    let text = text.trim_end_matches('Z');
    for format in FORMATS {
        if let Ok(naive) = NaiveDateTime::parse_from_str(text, format) {
            return Some(naive.and_utc());
        }
    }
    NaiveDate::parse_from_str(text, "%Y-%m-%d")
        .ok()
        .and_then(|date| date.and_hms_opt(0, 0, 0))
        .map(|naive| naive.and_utc())
}

fn text_of(value: ValueRef<'_>) -> FromSqlResult<&str> {
    match value {
        ValueRef::Text(bytes) => {
            std::str::from_utf8(bytes).map_err(|error| FromSqlError::Other(Box::new(error)))
        }
        _ => Err(FromSqlError::InvalidType),
    }
}

/// A `DATETIME` column; text both ways, which is all Swift ever writes.
pub struct DbDate(pub DateTime<Utc>);

impl ToSql for DbDate {
    fn to_sql(&self) -> rusqlite::Result<ToSqlOutput<'_>> {
        Ok(ToSqlOutput::from(date_text(self.0)))
    }
}

impl FromSql for DbDate {
    fn column_result(value: ValueRef<'_>) -> FromSqlResult<Self> {
        parse_date_text(text_of(value)?)
            .map(DbDate)
            .ok_or(FromSqlError::InvalidType)
    }
}

/// A `TEXT` id column holding an uppercase UUID.
pub struct DbUuid(pub Uuid);

impl ToSql for DbUuid {
    fn to_sql(&self) -> rusqlite::Result<ToSqlOutput<'_>> {
        Ok(ToSqlOutput::from(json::uuid_string(self.0)))
    }
}

impl FromSql for DbUuid {
    fn column_result(value: ValueRef<'_>) -> FromSqlResult<Self> {
        // The hyphenated form only, as GRDB's `UUID(uuidString:)` reads it.
        json::parse_uuid(text_of(value)?)
            .map(DbUuid)
            .ok_or(FromSqlError::InvalidType)
    }
}

/// A `TEXT` column holding one-line `StenoJSON`.
pub struct DbJson<T>(pub T);

impl<T: Serialize> ToSql for DbJson<T> {
    fn to_sql(&self) -> rusqlite::Result<ToSqlOutput<'_>> {
        let text = json::to_column_string(&self.0)
            .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))?;
        Ok(ToSqlOutput::from(text))
    }
}

impl<T: DeserializeOwned> FromSql for DbJson<T> {
    fn column_result(value: ValueRef<'_>) -> FromSqlResult<Self> {
        json::from_column_str(text_of(value)?)
            .map(DbJson)
            .map_err(|error| FromSqlError::Other(Box::new(error)))
    }
}

/// A `TEXT` column holding a string enum's case name.
pub struct DbEnum<T>(pub T);

impl<T: Display> ToSql for DbEnum<T> {
    fn to_sql(&self) -> rusqlite::Result<ToSqlOutput<'_>> {
        Ok(ToSqlOutput::from(self.0.to_string()))
    }
}

impl<T> FromSql for DbEnum<T>
where
    T: FromStr,
    T::Err: std::error::Error + Send + Sync + 'static,
{
    fn column_result(value: ValueRef<'_>) -> FromSqlResult<Self> {
        text_of(value)?
            .parse()
            .map(DbEnum)
            .map_err(|error| FromSqlError::Other(Box::new(error)))
    }
}

/// A `BLOB` column holding an [`Embedding`]. A blob whose length is not a
/// multiple of four is a read error, not a missing embedding.
pub struct DbEmbedding<T>(pub T);

impl ToSql for DbEmbedding<&Embedding> {
    fn to_sql(&self) -> rusqlite::Result<ToSqlOutput<'_>> {
        Ok(ToSqlOutput::from(self.0.to_bytes()))
    }
}

impl FromSql for DbEmbedding<Embedding> {
    fn column_result(value: ValueRef<'_>) -> FromSqlResult<Self> {
        let bytes = value.as_blob()?;
        Embedding::from_bytes(bytes)
            .map(DbEmbedding)
            .ok_or(FromSqlError::InvalidBlobSize {
                expected_size: bytes.len() - bytes.len() % 4,
                blob_size: bytes.len(),
            })
    }
}

/// A wrapper and the domain value inside it, for [`RowExt::col`].
pub trait Wrapped: FromSql {
    type Inner;
    fn inner(self) -> Self::Inner;
}

impl Wrapped for DbUuid {
    type Inner = Uuid;
    fn inner(self) -> Uuid {
        self.0
    }
}

impl Wrapped for DbDate {
    type Inner = DateTime<Utc>;
    fn inner(self) -> DateTime<Utc> {
        self.0
    }
}

impl<T: DeserializeOwned> Wrapped for DbJson<T> {
    type Inner = T;
    fn inner(self) -> T {
        self.0
    }
}

impl Wrapped for DbEmbedding<Embedding> {
    type Inner = Embedding;
    fn inner(self) -> Embedding {
        self.0
    }
}

impl<T> Wrapped for DbEnum<T>
where
    T: FromStr,
    T::Err: std::error::Error + Send + Sync + 'static,
{
    type Inner = T;
    fn inner(self) -> T {
        self.0
    }
}

impl<W: Wrapped> Wrapped for Option<W> {
    type Inner = Option<W::Inner>;
    fn inner(self) -> Self::Inner {
        self.map(Wrapped::inner)
    }
}

/// `row.col::<DbUuid>("id")?`, `row.col::<Option<DbDate>>("expiresAt")?`:
/// a column read through its wrapper and handed back as the domain value.
pub trait RowExt {
    fn col<W: Wrapped>(&self, name: &str) -> rusqlite::Result<W::Inner>;
}

impl RowExt for Row<'_> {
    fn col<W: Wrapped>(&self, name: &str) -> rusqlite::Result<W::Inner> {
        self.get::<_, W>(name).map(Wrapped::inner)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates_use_grdb_storage_format() {
        let date = parse_date_text("2026-09-29 13:49:11.135").unwrap();
        assert_eq!(date_text(date), "2026-09-29 13:49:11.135");
        assert_eq!(json::format_date(date), "2026-09-29T13:49:11.135Z");
        assert_eq!(
            parse_date_text("2026-09-29 13:49:11"),
            Some(date - chrono::Duration::milliseconds(135))
        );
        assert_eq!(parse_date_text("2026-09-29T13:49:11.135Z"), Some(date));
        assert!(parse_date_text("13:49").is_none());
    }

    #[test]
    fn dates_round_to_the_nearest_millisecond_like_grdb() {
        let whole = parse_date_text("2026-09-29 13:49:11.135").unwrap();
        assert_eq!(
            date_text(whole + TimeDelta::microseconds(500)),
            "2026-09-29 13:49:11.136"
        );
        assert_eq!(
            date_text(whole + TimeDelta::microseconds(499)),
            "2026-09-29 13:49:11.135"
        );
        let end_of_second = parse_date_text("2026-09-29 13:49:11.999").unwrap();
        assert_eq!(
            date_text(end_of_second + TimeDelta::microseconds(500)),
            "2026-09-29 13:49:12.000"
        );
    }
}
