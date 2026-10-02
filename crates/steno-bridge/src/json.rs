//! The one JSON convention, as `StenoJSON` (`Sources/StenoCore/Model/StenoJSON.swift`)
//! and `BridgeDispatcher.encoder()` produce it: sorted keys, slashes unescaped,
//! dates as `2026-09-29T12:48:00.000Z`, UUIDs upper case. Two styles:
//! [`to_canonical_string`] is Foundation's `.prettyPrinted` (what the fixtures
//! hold), [`to_compact_string`] is the one-line form the dispatcher sends.
//!
//! The printer walks a [`serde_json::Value`] rather than trusting
//! `serde_json::to_string_pretty`: Foundation puts a space on both sides of the
//! colon, prints an empty container as an open bracket, a blank line and a
//! closing bracket, and writes integral doubles without a fraction.

use std::fmt::Write as _;

use serde::Serialize;
use serde_json::Value;

/// `BridgeJSON.encode`: pretty printed, sorted keys, no trailing newline. The
/// fixture files are this plus one `\n` (`BridgeFixture.fileData()`).
pub fn to_canonical_string<T: Serialize + ?Sized>(value: &T) -> Result<String, serde_json::Error> {
    Ok(canonical(&serde_json::to_value(value)?))
}

/// `BridgeDispatcher.encoder()`: the same bytes without whitespace, for
/// replies and events that no person reads.
pub fn to_compact_string<T: Serialize + ?Sized>(value: &T) -> Result<String, serde_json::Error> {
    Ok(compact(&serde_json::to_value(value)?))
}

/// The pretty form of an already converted value.
pub fn canonical(value: &Value) -> String {
    let mut out = String::new();
    write_value(&mut out, value, Some(0));
    out
}

/// The compact form of an already converted value.
pub fn compact(value: &Value) -> String {
    let mut out = String::new();
    write_value(&mut out, value, None);
    out
}

const INDENT: &str = "  ";

fn write_indent(out: &mut String, depth: usize) {
    for _ in 0..depth {
        out.push_str(INDENT);
    }
}

/// `depth` is `None` for the compact style.
fn write_value(out: &mut String, value: &Value, depth: Option<usize>) {
    match value {
        Value::Null => out.push_str("null"),
        Value::Bool(true) => out.push_str("true"),
        Value::Bool(false) => out.push_str("false"),
        Value::Number(number) => write_number(out, number),
        Value::String(string) => write_string(out, string),
        Value::Array(items) => {
            write_container(out, '[', ']', items.len(), depth, |out, i, depth| {
                write_value(out, &items[i], depth);
            });
        }
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort_unstable();
            write_container(out, '{', '}', keys.len(), depth, |out, i, depth| {
                write_string(out, keys[i]);
                out.push_str(if depth.is_some() { " : " } else { ":" });
                write_value(out, &map[keys[i]], depth);
            });
        }
    }
}

fn write_container(
    out: &mut String,
    open: char,
    close: char,
    count: usize,
    depth: Option<usize>,
    mut write_item: impl FnMut(&mut String, usize, Option<usize>),
) {
    out.push(open);
    match depth {
        None => {
            for i in 0..count {
                if i > 0 {
                    out.push(',');
                }
                write_item(out, i, None);
            }
        }
        Some(depth) => {
            // Foundation prints `[\n\n  ]` for an empty container: the newline
            // after the bracket, the (empty) item line, the closing line.
            out.push('\n');
            for i in 0..count {
                if i > 0 {
                    out.push_str(",\n");
                }
                write_indent(out, depth + 1);
                write_item(out, i, Some(depth + 1));
            }
            out.push('\n');
            write_indent(out, depth);
        }
    }
    out.push(close);
}

/// Integers as they are; doubles in the shortest round-trip form, and an
/// integral double without a fraction (`1200`, not `1200.0`), as Foundation
/// writes a `Double`.
fn write_number(out: &mut String, number: &serde_json::Number) {
    if let Some(i) = number.as_i64() {
        let _ = write!(out, "{i}");
    } else if let Some(u) = number.as_u64() {
        let _ = write!(out, "{u}");
    } else if let Some(f) = number.as_f64() {
        if f.fract() == 0.0 && f.abs() < 1e15 {
            // Within range, so the cast is exact.
            #[allow(clippy::cast_possible_truncation)]
            let _ = write!(out, "{}", f as i64);
        } else {
            let _ = write!(out, "{f}");
        }
    }
}

/// Foundation's escaping with `.withoutEscapingSlashes`: the quote, the
/// backslash and the C0 controls; everything else, including non-ASCII, as is.
fn write_string(out: &mut String, string: &str) {
    out.push('"');
    for c in string.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

/// `StenoJSON`'s dates on the wire: UTC, three fraction digits, `Z`. Use as
/// `#[serde(with = "json::date")]` on a `DateTime<Utc>` and
/// `#[serde(with = "json::date::option")]` on an `Option<DateTime<Utc>>`.
pub mod date {
    use chrono::{DateTime, SecondsFormat, Utc};
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    /// `2026-09-25T10:00:00.000Z`: always UTC, always three fraction digits.
    pub fn format(date: &DateTime<Utc>) -> String {
        date.to_rfc3339_opts(SecondsFormat::Millis, true)
    }

    /// Accepts the fractional and the whole-second ISO 8601 forms, and any
    /// offset (normalised to UTC), as `StenoJSON.parse` does.
    pub fn parse(string: &str) -> Option<DateTime<Utc>> {
        DateTime::parse_from_rfc3339(string)
            .ok()
            .map(|date| date.with_timezone(&Utc))
    }

    pub fn serialize<S: Serializer>(
        date: &DateTime<Utc>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        format(date).serialize(serializer)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<DateTime<Utc>, D::Error> {
        let string = String::deserialize(deserializer)?;
        parse(&string)
            .ok_or_else(|| serde::de::Error::custom(format!("Not an ISO 8601 date: {string}")))
    }

    pub mod option {
        use chrono::{DateTime, Utc};
        use serde::{Deserialize, Deserializer, Serializer};

        pub fn serialize<S: Serializer>(
            date: &Option<DateTime<Utc>>,
            serializer: S,
        ) -> Result<S::Ok, S::Error> {
            match date {
                Some(date) => super::serialize(date, serializer),
                None => serializer.serialize_none(),
            }
        }

        pub fn deserialize<'de, D: Deserializer<'de>>(
            deserializer: D,
        ) -> Result<Option<DateTime<Utc>>, D::Error> {
            let string = Option::<String>::deserialize(deserializer)?;
            string
                .map(|string| {
                    super::parse(&string).ok_or_else(|| {
                        serde::de::Error::custom(format!("Not an ISO 8601 date: {string}"))
                    })
                })
                .transpose()
        }
    }
}

/// Foundation's `UUID` on the wire: hyphenated, upper case. Decoding accepts
/// either case, as `UUID(uuidString:)` does. Use as `#[serde(with = "json::uuid")]`
/// and `#[serde(with = "json::uuid::option")]`.
pub mod uuid {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};
    use uuid::Uuid;

    pub fn format(id: &Uuid) -> String {
        id.hyphenated()
            .encode_upper(&mut Uuid::encode_buffer())
            .to_string()
    }

    pub fn serialize<S: Serializer>(id: &Uuid, serializer: S) -> Result<S::Ok, S::Error> {
        format(id).serialize(serializer)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Uuid, D::Error> {
        let string = String::deserialize(deserializer)?;
        Uuid::parse_str(&string).map_err(serde::de::Error::custom)
    }

    pub mod option {
        use serde::{Deserialize, Deserializer, Serializer};
        use uuid::Uuid;

        pub fn serialize<S: Serializer>(
            id: &Option<Uuid>,
            serializer: S,
        ) -> Result<S::Ok, S::Error> {
            match id {
                Some(id) => super::serialize(id, serializer),
                None => serializer.serialize_none(),
            }
        }

        pub fn deserialize<'de, D: Deserializer<'de>>(
            deserializer: D,
        ) -> Result<Option<Uuid>, D::Error> {
            let string = Option::<String>::deserialize(deserializer)?;
            string
                .map(|string| Uuid::parse_str(&string).map_err(serde::de::Error::custom))
                .transpose()
        }
    }
}

#[cfg(test)]
mod tests {
    use chrono::{TimeZone, Utc};
    use serde_json::json;

    use super::*;

    #[test]
    fn pretty_matches_foundation() {
        let value = json!({"b": [], "a": {"y": 1.0, "x": "s/t"}, "c": {}});
        assert_eq!(
            canonical(&value),
            "{\n  \"a\" : {\n    \"x\" : \"s/t\",\n    \"y\" : 1\n  },\n  \"b\" : [\n\n  ],\n  \"c\" : {\n\n  }\n}"
        );
    }

    #[test]
    fn compact_has_no_whitespace() {
        let value = json!({"b": [1, 2.5], "a": null, "t": true});
        assert_eq!(compact(&value), r#"{"a":null,"b":[1,2.5],"t":true}"#);
    }

    #[test]
    fn numbers_print_like_foundation() {
        let value = json!([1200.0, 0.62, 42, -3, 1e21, 0.1]);
        assert_eq!(
            compact(&value),
            "[1200,0.62,42,-3,1000000000000000000000,0.1]"
        );
    }

    #[test]
    fn strings_escape_only_what_foundation_escapes() {
        let value = json!("a\"b\\c\nd\te\u{1}f/g é … \u{2028}");
        assert_eq!(
            compact(&value),
            "\"a\\\"b\\\\c\\nd\\te\\u0001f/g é … \u{2028}\""
        );
    }

    #[test]
    fn dates_round_trip_in_the_fixture_format() {
        let date = Utc.with_ymd_and_hms(2026, 9, 29, 12, 48, 0).unwrap();
        assert_eq!(date::format(&date), "2026-09-29T12:48:00.000Z");
        assert_eq!(date::parse("2026-09-29T12:48:00.000Z"), Some(date));
        assert_eq!(date::parse("2026-09-29T12:48:00Z"), Some(date));
        assert_eq!(date::parse("2026-09-29T14:48:00+02:00"), Some(date));
        assert_eq!(date::parse("yesterday"), None);
    }

    #[test]
    fn uuids_are_upper_case_on_the_wire() {
        let id = ::uuid::Uuid::parse_str("00000000-0000-0000-0000-00000000000c").unwrap();
        assert_eq!(uuid::format(&id), "00000000-0000-0000-0000-00000000000C");
    }
}
