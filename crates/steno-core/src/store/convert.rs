//! Column encodings shared with GRDB: dates as `yyyy-MM-dd HH:mm:ss.SSS`
//! UTC text, UUIDs as uppercase text, JSON columns through `StenoJSON`,
//! string enums by case name.

use std::fmt::Display;
use std::str::FromStr;

use chrono::{DateTime, NaiveDate, NaiveDateTime, Utc};
use rusqlite::types::{FromSql, FromSqlError, FromSqlResult, ToSql, ToSqlOutput, ValueRef};
use serde::Serialize;
use serde::de::DeserializeOwned;
use uuid::Uuid;

use crate::json;

/// GRDB's storage format for `Date`.
pub const GRDB_DATE_FORMAT: &str = "%Y-%m-%d %H:%M:%S%.3f";

/// `2026-09-29 13:49:11.135`, UTC, milliseconds truncated.
#[must_use]
pub fn date_text(date: DateTime<Utc>) -> String {
    date.format(GRDB_DATE_FORMAT).to_string()
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

/// A `DATETIME` column.
pub struct DbDate(pub DateTime<Utc>);

impl ToSql for DbDate {
    fn to_sql(&self) -> rusqlite::Result<ToSqlOutput<'_>> {
        Ok(ToSqlOutput::from(date_text(self.0)))
    }
}

impl FromSql for DbDate {
    fn column_result(value: ValueRef<'_>) -> FromSqlResult<Self> {
        match value {
            ValueRef::Text(_) => parse_date_text(text_of(value)?)
                .map(DbDate)
                .ok_or(FromSqlError::InvalidType),
            ValueRef::Real(seconds) => DateTime::from_timestamp_millis(
                // GRDB reads a number as seconds since 1970.
                #[allow(clippy::cast_possible_truncation)]
                {
                    (seconds * 1000.0).round() as i64
                },
            )
            .map(DbDate)
            .ok_or(FromSqlError::InvalidType),
            ValueRef::Integer(seconds) => DateTime::from_timestamp(seconds, 0)
                .map(DbDate)
                .ok_or(FromSqlError::InvalidType),
            _ => Err(FromSqlError::InvalidType),
        }
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
        Uuid::parse_str(text_of(value)?)
            .map(DbUuid)
            .map_err(|error| FromSqlError::Other(Box::new(error)))
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

/// `Option<DbUuid>` → `Option<Uuid>` and friends, for row reads.
pub trait Unwrap {
    type Inner;
    fn unwrap_db(self) -> Self::Inner;
}

impl Unwrap for Option<DbUuid> {
    type Inner = Option<Uuid>;
    fn unwrap_db(self) -> Option<Uuid> {
        self.map(|value| value.0)
    }
}

impl Unwrap for Option<DbDate> {
    type Inner = Option<DateTime<Utc>>;
    fn unwrap_db(self) -> Option<DateTime<Utc>> {
        self.map(|value| value.0)
    }
}

impl<T> Unwrap for Option<DbJson<T>> {
    type Inner = Option<T>;
    fn unwrap_db(self) -> Option<T> {
        self.map(|value| value.0)
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
}
