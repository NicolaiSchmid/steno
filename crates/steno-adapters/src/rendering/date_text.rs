//! Calendar dates and wall-clock times as text in a given time zone. No
//! locale enters, so the bytes are the same on every machine.
//! Swift: `Sources/StenoAdapters/Rendering/DateText.swift`.

use chrono::{DateTime, Utc};
use chrono_tz::Tz;

/// `"2026-09-24"`.
#[must_use]
pub fn day(date: DateTime<Utc>, time_zone: Tz) -> String {
    date.with_timezone(&time_zone)
        .format("%Y-%m-%d")
        .to_string()
}

/// `"14:00"`.
#[must_use]
pub fn clock(date: DateTime<Utc>, time_zone: Tz) -> String {
    date.with_timezone(&time_zone).format("%H:%M").to_string()
}

/// `"2026-09-24T14:00:00"`: Obsidian's Date & time property, no offset.
#[must_use]
pub fn date_time(date: DateTime<Utc>, time_zone: Tz) -> String {
    date.with_timezone(&time_zone)
        .format("%Y-%m-%dT%H:%M:%S")
        .to_string()
}

/// `"2026-09-24T12:00:00Z"`: the same instant in UTC, for files that carry
/// no time zone of their own.
#[must_use]
pub fn utc(date: DateTime<Utc>) -> String {
    date.format("%Y-%m-%dT%H:%M:%SZ").to_string()
}
