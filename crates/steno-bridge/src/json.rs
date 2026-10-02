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
/// round-trip, a tie between two shortest candidates broken to the even
/// digit, in plain notation when the decimal exponent is at least -4 and
/// `|f|` is at most 2^53 (`0.0001`, `1200`, `9007199254740992`), otherwise
/// `d.ddde±XX` with a signed exponent of at least two digits (`1e-05`,
/// `9.007199254740994e+15`, `1.7976931348623157e+308`). An integral double
/// has no fraction and a negative zero keeps its sign (`-0`).
///
/// The digits come from `zmij` (Schubfach: shortest round-trip digits, ties
/// to even, as `SwiftDtoa` breaks them) and are re-spelt here. Rust's own
/// `Display` and `LowerExp` take the upper candidate at a tie, at every digit
/// count (`999999999999999.3` and `-3981.6287231445313` where Foundation
/// writes `.2` and `…312`), which is why they are not the digit source.
fn write_number(out: &mut String, number: &serde_json::Number) {
    if let Some(i) = number.as_i64() {
        let _ = write!(out, "{i}");
    } else if let Some(u) = number.as_u64() {
        let _ = write!(out, "{u}");
    } else if let Some(f) = number.as_f64() {
        write_double(out, f);
    }
}

/// `write_number` for a double. `f` is finite: a `serde_json::Number` holds
/// no NaN or infinity (the module doc says where they went).
fn write_double(out: &mut String, f: f64) {
    const TWO_53: f64 = 9_007_199_254_740_992.0;
    if f.is_sign_negative() {
        out.push('-');
    }
    if f == 0.0 {
        out.push('0');
        return;
    }
    let (digits, exponent) = shortest_digits(f.abs());
    if exponent >= -4 && f.abs() <= TWO_53 {
        // Plain: the point sits `exponent + 1` digits in, padded with zeros
        // on whichever side runs out of digits.
        if exponent < 0 {
            out.push_str("0.");
            for _ in exponent..-1 {
                out.push('0');
            }
            out.push_str(&digits);
        } else {
            let point = usize::try_from(exponent).expect("non-negative") + 1;
            if point >= digits.len() {
                out.push_str(&digits);
                for _ in digits.len()..point {
                    out.push('0');
                }
            } else {
                let (whole, fraction) = digits.split_at(point);
                out.push_str(whole);
                out.push('.');
                out.push_str(fraction);
            }
        }
    } else {
        let (first, rest) = digits.split_at(1);
        out.push_str(first);
        if !rest.is_empty() {
            out.push('.');
            out.push_str(rest);
        }
        let sign = if exponent < 0 { '-' } else { '+' };
        let _ = write!(out, "e{sign}{:02}", exponent.abs());
    }
}

/// The shortest digits that read back to a finite, positive `f`, without
/// leading or trailing zeros, and the decimal exponent of the first one
/// (`f = d.ddd × 10^exponent`). `zmij` spells the number its own way (a
/// plain or an exponent form); this reads the digits back out of that.
fn shortest_digits(f: f64) -> (String, i32) {
    let mut buffer = zmij::Buffer::new();
    let printed = buffer.format_finite(f);
    let (mantissa, exponent) = printed.split_once('e').unwrap_or((printed, "0"));
    let exponent: i32 = exponent.parse().expect("zmij writes a decimal exponent");
    let (whole, fraction) = mantissa.split_once('.').unwrap_or((mantissa, ""));
    // `point` is where the decimal point sits in `whole ++ fraction`; every
    // leading zero dropped moves it one left.
    let mut point = i32::try_from(whole.len()).expect("at most 17 digits") + exponent;
    let mut digits = format!("{whole}{fraction}");
    while digits.starts_with('0') {
        digits.remove(0);
        point -= 1;
    }
    while digits.ends_with('0') {
        digits.pop();
    }
    debug_assert!(!digits.is_empty(), "a non-zero double has a digit");
    (digits, point - 1)
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
/// either case and only the hyphenated 36-character form, as
/// `UUID(uuidString:)` does. Use as `#[serde(with = "json::uuid")]` and
/// `#[serde(with = "json::uuid::option")]`.
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
        parse(&string).ok_or_else(|| serde::de::Error::custom(format!("Not a UUID: {string}")))
    }

    /// The hyphenated form only: the `uuid` crate would also read the
    /// 32-digit, braced and `urn:uuid:` forms, which Foundation rejects.
    pub fn parse(string: &str) -> Option<Uuid> {
        (string.len() == 36)
            .then(|| Uuid::try_parse(string).ok())
            .flatten()
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
    /// bits c0af1b41e8000000 => -3981.6287231445312
    /// bits c050c2ca80000000 => -67.04360961914062
    /// bits 3f8f010000000000 => 0.015138626098632812
    /// bits beba000000000000 => -1.5497207641601562e-06
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
            // Ties between two shortest candidates go to the even digit, as
            // measured; Rust's `Display` would write `.3`, `…313`, `…063`.
            (f64::from_bits(0x430c_6bf5_2633_fffa), "999999999999999.2"),
            (f64::from_bits(0xc30b_de3e_b7e9_7982), "-980523165757232.2"),
            (f64::from_bits(0xc0af_1b41_e800_0000), "-3981.6287231445312"),
            (f64::from_bits(0xc050_c2ca_8000_0000), "-67.04360961914062"),
            (
                f64::from_bits(0x3f8f_0100_0000_0000),
                "0.015138626098632812",
            ),
            (
                f64::from_bits(0xbeba_0000_0000_0000),
                "-1.5497207641601562e-06",
            ),
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

    /// `UUID(uuidString:)` reads either case of the hyphenated form and
    /// nothing else.
    #[test]
    fn uuids_read_only_the_hyphenated_form() {
        let id = ::uuid::Uuid::parse_str("00000000-0000-0000-0000-00000000000c").unwrap();
        assert_eq!(
            uuid::parse("00000000-0000-0000-0000-00000000000C"),
            Some(id)
        );
        assert_eq!(
            uuid::parse("00000000-0000-0000-0000-00000000000c"),
            Some(id)
        );
        for rejected in [
            "0000000000000000000000000000000c",
            "{00000000-0000-0000-0000-00000000000c}",
            "urn:uuid:00000000-0000-0000-0000-00000000000c",
            "00000000-0000-0000-0000-00000000000",
            "",
        ] {
            assert_eq!(uuid::parse(rejected), None, "{rejected:?}");
        }
        let wire: Wire = serde_json::from_str("\"00000000-0000-0000-0000-00000000000c\"").unwrap();
        assert_eq!(wire.0, id);
        let error = serde_json::from_str::<Wire>("\"0000000000000000000000000000000c\"")
            .unwrap_err()
            .to_string();
        assert!(error.starts_with("Not a UUID: 0000"), "{error}");
    }

    #[derive(Debug, serde::Deserialize)]
    struct Wire(#[serde(with = "uuid")] ::uuid::Uuid);
}
