//! `Settings` as one row per property in the `setting` table, each value a
//! one-line `StenoJSON` fragment. A property missing from the table loads
//! as its default. A row this build does not know (a newer build's or the
//! Swift app's, which share the file until the Mac cutover) is ignored on
//! load and kept on save.
//! Swift: `Sources/StenoCore/Storage/SettingsStore.swift`, which still
//! deletes and rewrites every row.
//!
//! The speech engines the Swift app offered beyond Parakeet v3
//! ([`RETIRED_SPEECH_ENGINE_IDS`]) have no Rust engine:
//! [`Store::retire_speech_engine`] moves a stored one to `parakeet-v3`,
//! which the Swift app decodes too, and leaves the one-time notice that says
//! so pending in the row [`SPEECH_ENGINE_NOTICE_KEY`] until
//! [`Store::dismiss_speech_engine_notice`]. Rust only.

use rusqlite::{OptionalExtension as _, params};
use serde::Deserialize;
use serde::de::{self, Deserializer, Visitor};
use serde_json::Value;

use super::{Result, Store, execute_cached, query_all};
use crate::json;
use crate::model::Settings;

/// The engine ids the Swift app offered that the Rust app has no engine
/// for: Whisper large-v3 turbo, Parakeet Ultra and the German Parakeet.
pub const RETIRED_SPEECH_ENGINE_IDS: [&str; 3] =
    ["whisperkit-large-v3-turbo", "parakeet-ultra", "parakeet-de"];

/// The engine a retired id becomes, a Swift engine too.
pub const PARAKEET_V3_ENGINE_ID: &str = "parakeet-v3";

/// The `setting` row of the pending engine notice; its value is the
/// retired engine id, as a JSON string. Not a [`Settings`] property, so no
/// save writes it and the Swift app's load ignores it.
pub const SPEECH_ENGINE_NOTICE_KEY: &str = "speechEngineNotice";

/// The key [`Settings::speech_engine_id`] is stored under.
const SPEECH_ENGINE_KEY: &str = "speechEngineID";

/// `settings` as a JSON object. Through text, not `to_value`, so an `f32`
/// keeps its shortest form (`0.6`) as in [`json::to_column_string`].
fn object(settings: &Settings) -> Result<serde_json::Map<String, Value>> {
    Ok(serde_json::from_str(&serde_json::to_string(settings)?)?)
}

/// Every key `Settings` reads and writes, the `None` ones included: the
/// field names its derived `Deserialize` hands to `deserialize_struct`, so
/// a new field is a known key without a list to keep in step.
fn known_keys() -> &'static [&'static str] {
    let mut fields = None;
    // Always an error: the deserializer stops at the field list.
    let _ = Settings::deserialize(FieldNames(&mut fields));
    fields.expect("Settings deserializes as a struct")
}

/// A deserializer that records the field list of the struct it is asked
/// for and fails everything else.
struct FieldNames<'a>(&'a mut Option<&'static [&'static str]>);

impl<'de> Deserializer<'de> for FieldNames<'_> {
    type Error = de::value::Error;

    fn deserialize_any<V: Visitor<'de>>(self, _: V) -> std::result::Result<V::Value, Self::Error> {
        Err(de::Error::custom("only the field names are read"))
    }

    fn deserialize_struct<V: Visitor<'de>>(
        self,
        _: &'static str,
        fields: &'static [&'static str],
        _: V,
    ) -> std::result::Result<V::Value, Self::Error> {
        *self.0 = Some(fields);
        Err(de::Error::custom("only the field names are read"))
    }

    serde::forward_to_deserialize_any! {
        bool i8 i16 i32 i64 i128 u8 u16 u32 u64 u128 f32 f64 char str string
        bytes byte_buf option unit unit_struct newtype_struct seq tuple
        tuple_struct map enum identifier ignored_any
    }
}

impl Store {
    /// The defaults overlaid with every stored row.
    pub fn settings(&self) -> Result<Settings> {
        self.settings_with_defaults(&Settings::default())
    }

    /// [`Store::settings`] over explicit defaults (tests pass one with a
    /// known audio folder).
    pub fn settings_with_defaults(&self, defaults: &Settings) -> Result<Settings> {
        let rows = self.read(|connection| {
            query_all(connection, "SELECT key, value FROM setting", [], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
        })?;
        let mut merged = object(defaults)?;
        for (key, value) in rows {
            merged.insert(key, json::from_column_str(&value)?);
        }
        Ok(serde_json::from_value(Value::Object(merged))?)
    }

    /// Writes every property and removes the rows of known properties that
    /// are now `None`. Rows for keys this build does not know stay.
    pub fn save_settings(&self, settings: &Settings) -> Result<()> {
        let object = object(settings)?;
        let mut keys: Vec<&String> = object.keys().collect();
        keys.sort();
        self.write(|transaction| {
            for key in known_keys() {
                if !object.contains_key(*key) {
                    execute_cached(
                        transaction,
                        "DELETE FROM setting WHERE key = ?1",
                        params![key],
                    )?;
                }
            }
            for key in keys {
                let fragment = json::to_column_string(&object[key])?;
                execute_cached(
                    transaction,
                    "INSERT INTO setting (key, value) VALUES (?1, ?2) \
                     ON CONFLICT (key) DO UPDATE SET value = excluded.value",
                    params![key, fragment],
                )?;
            }
            Ok(())
        })
    }

    /// When the stored engine id is one of [`RETIRED_SPEECH_ENGINE_IDS`],
    /// stores [`PARAKEET_V3_ENGINE_ID`] in its place and leaves the engine
    /// notice pending, in one transaction; returns whether it did. Any
    /// other id, or none, changes nothing, so the notice comes once per
    /// move. The app calls it at launch, before anything reads the engine.
    pub fn retire_speech_engine(&self) -> Result<bool> {
        self.write(|transaction| {
            let stored: Option<String> = transaction
                .query_row(
                    "SELECT value FROM setting WHERE key = ?1",
                    params![SPEECH_ENGINE_KEY],
                    |row| row.get(0),
                )
                .optional()?;
            let Some(stored) = stored else {
                return Ok(false);
            };
            let id: Value = json::from_column_str(&stored)?;
            let Some(id) = id
                .as_str()
                .filter(|id| RETIRED_SPEECH_ENGINE_IDS.contains(id))
            else {
                return Ok(false);
            };
            for (key, value) in [
                (SPEECH_ENGINE_KEY, PARAKEET_V3_ENGINE_ID),
                (SPEECH_ENGINE_NOTICE_KEY, id),
            ] {
                execute_cached(
                    transaction,
                    "INSERT INTO setting (key, value) VALUES (?1, ?2) \
                     ON CONFLICT (key) DO UPDATE SET value = excluded.value",
                    params![key, json::to_column_string(&value)?],
                )?;
            }
            Ok(true)
        })
    }

    /// The retired engine id whose notice is pending, if any.
    pub fn speech_engine_notice(&self) -> Result<Option<String>> {
        let stored: Option<String> = self.read(|connection| {
            Ok(connection
                .query_row(
                    "SELECT value FROM setting WHERE key = ?1",
                    params![SPEECH_ENGINE_NOTICE_KEY],
                    |row| row.get(0),
                )
                .optional()?)
        })?;
        Ok(stored
            .map(|value| json::from_column_str::<Value>(&value))
            .transpose()?
            .and_then(|value| value.as_str().map(str::to_owned)))
    }

    /// Records the engine notice as seen, so it never shows again.
    pub fn dismiss_speech_engine_notice(&self) -> Result<()> {
        self.write(|transaction| {
            execute_cached(
                transaction,
                "DELETE FROM setting WHERE key = ?1",
                params![SPEECH_ENGINE_NOTICE_KEY],
            )?;
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::ObsidianSettings;

    /// The known keys are exactly the keys of a `Settings` with every
    /// `Option` filled, so none of them can be left out of the deletes.
    #[test]
    fn known_keys_are_every_serialized_key() {
        let settings = Settings {
            input_device_uid: Some("device".to_owned()),
            models_directory: Some("/models".to_owned()),
            llm_base_url: Some("http://localhost".to_owned()),
            llm_model: Some("model".to_owned()),
            codex_model: Some("codex".to_owned()),
            codex_confirmed_at: Some(chrono::DateTime::UNIX_EPOCH),
            obsidian: Some(ObsidianSettings {
                vault_path: "/vault".to_owned(),
                people_folder: None,
                include_audio: false,
                task_tag: None,
                extra: serde_json::Map::new(),
            }),
            ..Settings::default()
        };
        let serialized: Vec<String> = object(&settings).unwrap().keys().cloned().collect();
        let mut known = known_keys().to_vec();
        known.sort_unstable();
        assert_eq!(known, serialized);
    }
}
