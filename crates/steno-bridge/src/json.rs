//! The one JSON convention, as `StenoJSON` (`Sources/StenoCore/Model/StenoJSON.swift`)
//! and `BridgeDispatcher.encoder()` produce it: sorted keys, slashes unescaped,
//! dates as `2026-09-29T12:48:00.000Z`, UUIDs upper case. Two styles:
//! [`to_canonical_string`] is Foundation's `.prettyPrinted` (what the fixtures
//! hold), [`to_compact_string`] is the one-line form the dispatcher sends.
//!
//! The printer walks a [`serde_json::Value`] rather than trusting
//! `serde_json::to_string_pretty`: Foundation puts a space on both sides of the
//! colon, prints an empty container as an open bracket, a blank line and a
//! closing bracket, and spells doubles its own way (see `write_number`).
//!
//! NaN and infinity: `serde_json::to_value` turns a non-finite `f64` into
//! `null` before the printer sees it, so both styles here write `null`. Swift
//! differs: `BridgeJSON.encode` (the fixtures) throws, and the dispatcher's
//! compact encoder writes the strings `"NaN"`, `"Infinity"` and
//! `"-Infinity"`. Mirroring the strings would need a `serde::Serializer` of
//! our own, since the `Value` tree cannot tell a `null` from a NaN after the
//! fact; the page's schema (`z.number()`) rejects both spellings alike and
//! treats the field as "no value", so `null` is the recorded choice until a
//! plan says otherwise. `nan_and_infinity_are_null` pins it.

use std::fmt::Write as _;

use serde::Serialize;
use serde_json::Value;

/// `BridgeJSON.encode`: pretty printed, sorted keys, no trailing newline. The
/// fixture files are this plus one `\n` (`BridgeFixture.fileData()`).
///
/// ```
/// use serde_json::json;
/// use steno_bridge::json::to_canonical_string;
///
/// let value = json!({"tags": [], "durationSeconds": 1200.0, "title": "Sync / weekly"});
/// assert_eq!(
///     to_canonical_string(&value).unwrap(),
///     "{\n  \"durationSeconds\" : 1200,\n  \"tags\" : [\n\n  ],\n  \"title\" : \"Sync / weekly\"\n}"
/// );
/// ```
pub fn to_canonical_string<T: Serialize + ?Sized>(value: &T) -> Result<String, serde_json::Error> {
    let mut out = String::new();
    write_value(&mut out, &serde_json::to_value(value)?, Some(0));
    Ok(out)
}

/// `BridgeDispatcher.encoder()`: the same bytes without whitespace, for
/// replies and events that no person reads.
pub fn to_compact_string<T: Serialize + ?Sized>(value: &T) -> Result<String, serde_json::Error> {
    let mut out = String::new();
    write_value(&mut out, &serde_json::to_value(value)?, None);
    Ok(out)
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

/// Integers as they are. Doubles as Foundation's `JSONEncoder` spells a
/// `Double` (measured with `StenoJSON.encoder()` on macOS 26, Swift 6.4:
/// the table in `numbers_print_like_foundation` and the corpus in
/// `tests/fixtures/foundation-doubles.txt`): the shortest digits that
/// round-trip, in plain notation when the decimal exponent is at least -4
/// and `|f|` is at most 2^53 (`0.0001`, `1200`, `9007199254740992`),
/// otherwise `d.ddde±XX` with a signed exponent of at least two digits
/// (`1e-05`, `9.007199254740994e+15`, `1.7976931348623157e+308`). An
/// integral double has no fraction and a negative zero keeps its sign (`-0`).
///
/// Rust's `Display` for `f64` is the same shortest-digit algorithm in plain
/// notation and covers the plain range, bar one tie (`write_tie_break`); the
/// exponent range is re-spelt from `{:e}`.
fn write_number(out: &mut String, number: &serde_json::Number) {
    if let Some(i) = number.as_i64() {
        let _ = write!(out, "{i}");
    } else if let Some(u) = number.as_u64() {
        let _ = write!(out, "{u}");
    } else if let Some(f) = number.as_f64() {
        const TWO_53: f64 = 9_007_199_254_740_992.0;
        let scientific = format!("{f:e}");
        let (mantissa, exponent) = scientific
            .split_once('e')
            .expect("`{:e}` always writes an exponent");
        let exponent: i32 = exponent.parse().expect("`{:e}` writes a decimal exponent");
        if exponent < -4 || f.abs() > TWO_53 {
            let sign = if exponent < 0 { '-' } else { '+' };
            let _ = write!(out, "{mantissa}e{sign}{:02}", exponent.abs());
        } else if !write_tie_break(out, f) {
            let _ = write!(out, "{f}");
        }
    }
}

/// The one place Rust and Foundation pick different shortest digits. For
/// 2^49 <= |f| < 2^51 doubles are 1/8 or 1/4 apart, so a value ending in
/// `.25` or `.75` lies exactly halfway between two one-digit decimals that
/// both round-trip to it (`…999.25` is `.2` or `.3`). Rust's `Display` takes
/// the upper one, Foundation the one whose last digit is even, which
/// differs for `.25` (`999999999999999.2`, not `.3`) and agrees for `.75`.
/// Below 2^49 the neighbours are too close for a one-digit candidate, from
/// 2^51 no double has such a fraction. Returns whether it wrote the number.
fn write_tie_break(out: &mut String, f: f64) -> bool {
    const TWO_49: f64 = 562_949_953_421_312.0;
    const TWO_51: f64 = 2_251_799_813_685_248.0;
    let magnitude = f.abs();
    if !(TWO_49..TWO_51).contains(&magnitude) {
        return false;
    }
    // Exact: the value has at most three fraction bits in this range.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let eighths = (magnitude * 8.0) as u64;
    // In tenths, `value = eighths * 5 / 4`; a tie is a remainder of 2.
    let tenths = eighths * 5;
    if tenths % 4 != 2 {
        return false;
    }
    let lower = tenths / 4;
    // The even one of `lower` and `lower + 1`.
    let even = lower + (lower & 1);
    let sign = if f.is_sign_negative() { "-" } else { "" };
    let _ = write!(out, "{sign}{}.{}", even / 10, even % 10);
    true
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

    /// The same format on an `Option`; `None` is `null`, or an omitted key
    /// with `skip_serializing_if`.
    pub mod option {
        use chrono::{DateTime, Utc};
        use serde::{Deserialize, Deserializer, Serialize, Serializer};

        #[derive(Serialize, Deserialize)]
        struct Wire(#[serde(with = "super")] DateTime<Utc>);

        pub fn serialize<S: Serializer>(
            date: &Option<DateTime<Utc>>,
            serializer: S,
        ) -> Result<S::Ok, S::Error> {
            date.map(Wire).serialize(serializer)
        }

        pub fn deserialize<'de, D: Deserializer<'de>>(
            deserializer: D,
        ) -> Result<Option<DateTime<Utc>>, D::Error> {
            Ok(Option::<Wire>::deserialize(deserializer)?.map(|wire| wire.0))
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

    /// The same format on an `Option`; `None` is `null`, or an omitted key
    /// with `skip_serializing_if`.
    pub mod option {
        use serde::{Deserialize, Deserializer, Serialize, Serializer};
        use uuid::Uuid;

        #[derive(Serialize, Deserialize)]
        struct Wire(#[serde(with = "super")] Uuid);

        pub fn serialize<S: Serializer>(
            id: &Option<Uuid>,
            serializer: S,
        ) -> Result<S::Ok, S::Error> {
            id.map(Wire).serialize(serializer)
        }

        pub fn deserialize<'de, D: Deserializer<'de>>(
            deserializer: D,
        ) -> Result<Option<Uuid>, D::Error> {
            Ok(Option::<Wire>::deserialize(deserializer)?.map(|wire| wire.0))
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
            to_canonical_string(&value).unwrap(),
            "{\n  \"a\" : {\n    \"x\" : \"s/t\",\n    \"y\" : 1\n  },\n  \"b\" : [\n\n  ],\n  \"c\" : {\n\n  }\n}"
        );
    }

    #[test]
    fn compact_has_no_whitespace() {
        let value = json!({"b": [1, 2.5], "a": null, "t": true});
        assert_eq!(
            to_compact_string(&value).unwrap(),
            r#"{"a":null,"b":[1,2.5],"t":true}"#
        );
    }

    /// The oracle: `StenoJSON.encoder().encode([value])` on macOS 26 (Swift
    /// 6.4, Xcode 27), one `Double` per line as `input => output`.
    ///
    /// ```text
    /// 0.0 => 0                        -0.0 => -0
    /// 1.0 => 1                        -1.0 => -1
    /// 1200.0 => 1200                  0.42 => 0.42
    /// 0.62 => 0.62                    2.5 => 2.5
    /// -3.25 => -3.25                  1234.5678 => 1234.5678
    /// 0.1+0.2 => 0.30000000000000004  0.7307184925394371 => 0.7307184925394371
    /// 0.001 => 0.001                  0.0001234 => 0.0001234
    /// 1e-4 => 0.0001                  1e-5 => 1e-05
    /// 0.00001234 => 1.234e-05         0.0000999 => 9.99e-05
    /// 1e-6 => 1e-06                   1e-7 => 1e-07
    /// 3.2e-5 => 3.2e-05               5.5e-9 => 5.5e-09
    /// 1e14 => 100000000000000         1e15 => 1000000000000000
    /// 9007199254740992.0 => 9007199254740992
    /// 9007199254740993.0 => 9007199254740992
    /// 9007199254740994.0 => 9.007199254740994e+15
    /// 9.5e15 => 9.5e+15
    /// 9999999999999998.0 => 9.999999999999998e+15
    /// 99999999999999990.0 => 9.999999999999998e+16
    /// bits 430c6bf52633fffa => 999999999999999.2
    /// bits c30bde3eb7e97982 => -980523165757232.2
    /// 1e16 => 1e+16                   1.234e16 => 1.234e+16
    /// 1e17 => 1e+17                   4.5e17 => 4.5e+17
    /// 123456789012345680.0 => 1.2345678901234568e+17
    /// 12345678901234567890.0 => 1.2345678901234567e+19
    /// 1.1e19 => 1.1e+19               1.1e20 => 1.1e+20
    /// 1e21 => 1e+21                   1.5e300 => 1.5e+300
    /// f64::MAX => 1.7976931348623157e+308
    /// 1e-308 => 1e-308                f64::MIN_POSITIVE => 2.2250738585072014e-308
    /// 5e-324 (smallest subnormal) => 5e-324
    /// ```
    #[test]
    fn numbers_print_like_foundation() {
        let cases: &[(f64, &str)] = &[
            (0.0, "0"),
            (-0.0, "-0"),
            (1.0, "1"),
            (-1.0, "-1"),
            (1200.0, "1200"),
            (0.42, "0.42"),
            (0.62, "0.62"),
            (2.5, "2.5"),
            (-3.25, "-3.25"),
            (1234.5678, "1234.5678"),
            (0.1 + 0.2, "0.30000000000000004"),
            (0.730_718_492_539_437_1, "0.7307184925394371"),
            (0.001, "0.001"),
            (0.000_123_4, "0.0001234"),
            (1e-4, "0.0001"),
            (1e-5, "1e-05"),
            (0.000_012_34, "1.234e-05"),
            (0.000_099_9, "9.99e-05"),
            (1e-6, "1e-06"),
            (1e-7, "1e-07"),
            (3.2e-5, "3.2e-05"),
            (5.5e-9, "5.5e-09"),
            (1e14, "100000000000000"),
            (1e15, "1000000000000000"),
            (9_007_199_254_740_992.0, "9007199254740992"),
            (9_007_199_254_740_993.0, "9007199254740992"),
            (9_007_199_254_740_994.0, "9.007199254740994e+15"),
            (9.5e15, "9.5e+15"),
            (9_999_999_999_999_998.0, "9.999999999999998e+15"),
            (99_999_999_999_999_990.0, "9.999999999999998e+16"),
            // The `.25` ties of `write_tie_break`, as measured.
            (f64::from_bits(0x430c_6bf5_2633_fffa), "999999999999999.2"),
            (f64::from_bits(0xc30b_de3e_b7e9_7982), "-980523165757232.2"),
            // A `.75` tie: the even digit is the upper one, as Rust prints it.
            (562_949_953_421_312.0 + 0.75, "562949953421312.8"),
            (1e16, "1e+16"),
            (1.234e16, "1.234e+16"),
            (1e17, "1e+17"),
            (4.5e17, "4.5e+17"),
            (123_456_789_012_345_680.0, "1.2345678901234568e+17"),
            (12_345_678_901_234_567_890.0, "1.2345678901234567e+19"),
            (1.1e19, "1.1e+19"),
            (1.1e20, "1.1e+20"),
            (1e21, "1e+21"),
            (1.5e300, "1.5e+300"),
            (f64::MAX, "1.7976931348623157e+308"),
            (1e-308, "1e-308"),
            (f64::MIN_POSITIVE, "2.2250738585072014e-308"),
            (5e-324, "5e-324"),
        ];
        for (value, expected) in cases {
            assert_eq!(
                to_compact_string(value).unwrap(),
                *expected,
                "for {value:e}"
            );
        }
        // Integers stay integers.
        assert_eq!(
            to_compact_string(&json!([42, -3, 9_007_199_254_740_993_i64])).unwrap(),
            "[42,-3,9007199254740993]"
        );
    }

    /// The decision recorded in the module doc: a non-finite double is `null`
    /// here, where Swift's compact encoder writes `"NaN"` and `"Infinity"`.
    #[test]
    fn nan_and_infinity_are_null() {
        let value = [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, 1.0];
        assert_eq!(to_compact_string(&value).unwrap(), "[null,null,null,1]");
        assert_eq!(
            to_canonical_string(&value).unwrap(),
            "[\n  null,\n  null,\n  null,\n  1\n]"
        );
    }

    #[test]
    fn strings_escape_only_what_foundation_escapes() {
        let value = json!("a\"b\\c\nd\te\u{1}f/g é … \u{2028}");
        assert_eq!(
            to_compact_string(&value).unwrap(),
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
