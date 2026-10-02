//! The `StenoJSON` convention (`Sources/StenoCore/Model/StenoJSON.swift`):
//! sorted keys, ISO 8601 dates with three fraction digits, `Data` as base64,
//! slashes unescaped. JSON columns use the compact one-line form; the pretty
//! form (`meeting.json`) belongs to a later package.
//!
//! The date and UUID text codecs ([`format_date`], [`parse_date`],
//! [`uuid_string`], [`parse_uuid`] and the `with` modules [`iso_time`],
//! [`iso_time_opt`], [`uuid_text`] and [`uuid_text_opt`]) are the one
//! implementation for every Steno crate; `steno-bridge` reads and writes its
//! envelopes with them.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Deserializer, Serialize, Serializer, de};
use serde_json::Value;
use uuid::Uuid;

/// `value` as the one-line JSON text a column holds: keys sorted, whole
/// doubles written as integers the way Swift's `JSONEncoder` writes them.
/// Serialised to text before sorting so an `f32` keeps its shortest form
/// (`0.7`, not the widened `0.699999988079071` a `Value` would hold).
pub fn to_column_string<T: Serialize>(value: &T) -> Result<String, serde_json::Error> {
    let mut json: Value = serde_json::from_str(&serde_json::to_string(value)?)?;
    normalise(&mut json);
    serde_json::to_string(&json)
}

/// The inverse of [`to_column_string`].
pub fn from_column_str<T: de::DeserializeOwned>(text: &str) -> Result<T, serde_json::Error> {
    serde_json::from_str(text)
}

fn normalise(value: &mut Value) {
    match value {
        Value::Object(map) => {
            let mut entries: Vec<(String, Value)> = std::mem::take(map).into_iter().collect();
            entries.sort_by(|left, right| left.0.cmp(&right.0));
            for (key, mut entry) in entries {
                normalise(&mut entry);
                map.insert(key, entry);
            }
        }
        Value::Array(items) => items.iter_mut().for_each(normalise),
        Value::Number(number) => {
            if number.is_f64()
                && let Some(float) = number.as_f64()
                && float.fract() == 0.0
                && float.abs() < 9_007_199_254_740_992.0
            {
                // Exact: the magnitude is below 2^53 and the fraction is zero.
                #[allow(clippy::cast_possible_truncation)]
                let whole = float as i64;
                *value = Value::from(whole);
            }
        }
        _ => {}
    }
}

/// `2026-09-25T10:00:00.000Z`: always UTC, always three fraction digits,
/// truncated like Swift's `StenoJSON.format` (see `store::convert`).
#[must_use]
pub fn format_date(date: DateTime<Utc>) -> String {
    date.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string()
}

/// Accepts the fractional and the whole-second ISO 8601 forms.
#[must_use]
pub fn parse_date(text: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(text)
        .ok()
        .map(|date| date.with_timezone(&Utc))
}

/// The uppercase hyphenated form Swift's `UUID.uuidString` produces, which
/// is what every id column and JSON id holds.
#[must_use]
pub fn uuid_string(id: Uuid) -> String {
    id.hyphenated()
        .encode_upper(&mut Uuid::encode_buffer())
        .to_owned()
}

/// The inverse of [`uuid_string`]: either case of the hyphenated
/// 36-character form and nothing else, as `UUID(uuidString:)` reads it. The
/// `uuid` crate alone would also take the 32-digit, braced and `urn:uuid:`
/// forms, which Foundation rejects.
#[must_use]
pub fn parse_uuid(text: &str) -> Option<Uuid> {
    if text.len() != 36 {
        return None;
    }
    Uuid::try_parse(text).ok()
}

/// [`parse_uuid`] with serde's error, for the `with` modules.
fn uuid_from_text<E: de::Error>(text: &str) -> Result<Uuid, E> {
    parse_uuid(text).ok_or_else(|| E::custom(format!("not a UUID: {text}")))
}

/// `Uuid` as uppercase text.
pub mod uuid_text {
    use super::{Deserialize, Deserializer, Serializer, Uuid};

    pub fn serialize<S: Serializer>(id: &Uuid, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&super::uuid_string(*id))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Uuid, D::Error> {
        let text = String::deserialize(deserializer)?;
        super::uuid_from_text(&text)
    }
}

/// `Option<Uuid>` as uppercase text; pair with `skip_serializing_if` and
/// `default`.
pub mod uuid_text_opt {
    use super::{Deserialize, Deserializer, Serializer, Uuid};

    pub fn serialize<S: Serializer>(id: &Option<Uuid>, serializer: S) -> Result<S::Ok, S::Error> {
        match id {
            Some(id) => serializer.serialize_some(&super::uuid_string(*id)),
            None => serializer.serialize_none(),
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Option<Uuid>, D::Error> {
        let text = Option::<String>::deserialize(deserializer)?;
        text.as_deref().map(super::uuid_from_text).transpose()
    }
}

/// `DateTime<Utc>` as `StenoJSON` text.
pub mod iso_time {
    use super::{DateTime, Deserialize, Deserializer, Serializer, Utc, de};

    pub fn serialize<S: Serializer>(
        date: &DateTime<Utc>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&super::format_date(*date))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<DateTime<Utc>, D::Error> {
        let text = String::deserialize(deserializer)?;
        super::parse_date(&text)
            .ok_or_else(|| de::Error::custom(format!("not an ISO 8601 date: {text}")))
    }
}

/// `Option<DateTime<Utc>>` as `StenoJSON` text.
pub mod iso_time_opt {
    use super::{DateTime, Deserialize, Deserializer, Serializer, Utc, de};

    pub fn serialize<S: Serializer>(
        date: &Option<DateTime<Utc>>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        match date {
            Some(date) => serializer.serialize_some(&super::format_date(*date)),
            None => serializer.serialize_none(),
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Option<DateTime<Utc>>, D::Error> {
        let text = Option::<String>::deserialize(deserializer)?;
        text.map(|text| {
            super::parse_date(&text)
                .ok_or_else(|| de::Error::custom(format!("not an ISO 8601 date: {text}")))
        })
        .transpose()
    }
}

/// Bytes as standard base64, Swift's `.base64` data strategy.
pub mod base64_bytes {
    use base64::Engine as _;
    use base64::engine::general_purpose::STANDARD;

    use super::{Deserialize, Deserializer, Serializer, de};

    pub fn serialize<S: Serializer>(bytes: &[u8], serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&STANDARD.encode(bytes))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Vec<u8>, D::Error> {
        let text = String::deserialize(deserializer)?;
        STANDARD.decode(text).map_err(de::Error::custom)
    }
}

/// The JSON shape of enums with payloads (`Model/CaseCoding.swift`): a bare
/// string for a case without a payload (`"ready"`) and a one-key object for
/// a case with one (`{"failed":"reason"}`, `{"keepDays":30}`).
pub(crate) mod case_coding {
    use serde::ser::SerializeMap;

    use super::{Deserialize, Serialize, Serializer, Value, de};

    /// What a payload enum decodes to before its cases are matched.
    #[derive(Deserialize)]
    #[serde(untagged)]
    pub enum Case {
        Bare(String),
        Payload(serde_json::Map<String, Value>),
    }

    impl Case {
        /// The case name and its payload, if any.
        pub fn split<E: de::Error>(self) -> Result<(String, Option<Value>), E> {
            match self {
                Case::Bare(name) => Ok((name, None)),
                Case::Payload(map) => {
                    if map.len() != 1 {
                        return Err(E::custom(format!(
                            "expected a case name or a one-key object, got {} keys",
                            map.len()
                        )));
                    }
                    let (name, payload) = map.into_iter().next().expect("one key");
                    Ok((name, Some(payload)))
                }
            }
        }
    }

    pub fn serialize_bare<S: Serializer>(name: &str, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(name)
    }

    pub fn serialize_payload<S: Serializer, P: Serialize + ?Sized>(
        name: &str,
        payload: &P,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(1))?;
        map.serialize_entry(name, payload)?;
        map.end()
    }

    /// The payload a case requires.
    pub fn payload<T: de::DeserializeOwned, E: de::Error>(
        name: &str,
        payload: Option<Value>,
    ) -> Result<T, E> {
        let value = payload.ok_or_else(|| E::custom(format!("case {name} needs a payload")))?;
        serde_json::from_value(value).map_err(E::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn column_json_sorts_keys_and_writes_whole_doubles_as_integers() {
        let value = serde_json::json!({"zeta": 1.0, "alpha": {"b": 2.5, "a": [3.0]}});
        assert_eq!(
            to_column_string(&value).unwrap(),
            r#"{"alpha":{"a":[3],"b":2.5},"zeta":1}"#
        );
    }

    #[test]
    fn dates_use_three_fraction_digits_in_utc() {
        let date = parse_date("2026-09-25T12:00:00.5+02:00").unwrap();
        assert_eq!(format_date(date), "2026-09-25T10:00:00.500Z");
        assert_eq!(
            parse_date("2026-09-25T10:00:00Z"),
            Some(date - chrono::Duration::milliseconds(500))
        );
    }

    #[test]
    fn uuids_are_uppercase() {
        let id = Uuid::parse_str("516eade8-40e5-4434-8aaf-9214a21a604e").unwrap();
        assert_eq!(uuid_string(id), "516EADE8-40E5-4434-8AAF-9214A21A604E");
    }

    /// `UUID(uuidString:)` reads either case of the hyphenated form and
    /// nothing else; the codecs follow it rather than the `uuid` crate.
    #[test]
    fn uuids_read_only_the_hyphenated_form() {
        #[derive(Debug, PartialEq, Deserialize)]
        struct Wire(
            #[serde(with = "uuid_text")] Uuid,
            #[serde(with = "uuid_text_opt")] Option<Uuid>,
        );

        let id = Uuid::parse_str("516eade8-40e5-4434-8aaf-9214a21a604e").unwrap();
        assert_eq!(parse_uuid("516EADE8-40E5-4434-8AAF-9214A21A604E"), Some(id));
        assert_eq!(parse_uuid("516eade8-40e5-4434-8aaf-9214a21a604e"), Some(id));
        for rejected in [
            "516eade840e544348aaf9214a21a604e",
            "{516eade8-40e5-4434-8aaf-9214a21a604e}",
            "urn:uuid:516eade8-40e5-4434-8aaf-9214a21a604e",
            "516eade8-40e5-4434-8aaf-9214a21a604",
            "",
        ] {
            assert_eq!(parse_uuid(rejected), None, "{rejected:?}");
            let error = serde_json::from_str::<Wire>(&format!(
                "[\"516EADE8-40E5-4434-8AAF-9214A21A604E\",{}]",
                serde_json::to_string(rejected).unwrap()
            ))
            .unwrap_err()
            .to_string();
            assert!(error.starts_with("not a UUID: "), "{rejected:?}: {error}");
        }
        let wire: Wire =
            serde_json::from_str("[\"516eade8-40e5-4434-8aaf-9214a21a604e\",null]").unwrap();
        assert_eq!(wire, Wire(id, None));
        let error = serde_json::from_str::<Wire>("[\"516eade840e544348aaf9214a21a604e\",null]")
            .unwrap_err()
            .to_string();
        assert!(error.starts_with("not a UUID: 516e"), "{error}");
    }
}
