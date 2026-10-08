//! `Settings` as one row per property in the `setting` table, each value a
//! one-line `StenoJSON` fragment. A property missing from the table loads
//! as its default. A row this build does not know (a newer build's or the
//! Swift app's, which share the file until the Mac cutover) is ignored on
//! load and kept on save.
//! Swift: `Sources/StenoCore/Storage/SettingsStore.swift`, which still
//! deletes and rewrites every row.
//!
//! Parakeet v3 ([`PARAKEET_V3_ENGINE_ID`]) is the one engine the Rust app
//! runs; [`Store::retire_speech_engine`] moves any other stored engine id
//! (Whisper, Parakeet Ultra and the German Parakeet the Swift app offered)
//! to it. Rust only.

use rusqlite::{OptionalExtension as _, params};
use serde::Deserialize;
use serde::de::{self, Deserializer, Visitor};
use serde_json::Value;

use super::{Result, Store, execute_cached, query_all};
use crate::json;
use crate::model::Settings;

/// The one engine the Rust app runs, a Swift engine too: what
/// [`Store::retire_speech_engine`] stores in place of any other id.
/// Swift: `SpeechEngineID.parakeetV3`
/// (`Sources/StenoSpeech/Engines/SpeechEngineID.swift`).
pub const PARAKEET_V3_ENGINE_ID: &str = "parakeet-v3";

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

    /// When an engine id other than [`PARAKEET_V3_ENGINE_ID`] is stored
    /// (an engine the Swift app offered and the Rust app does not run),
    /// calls `before_rewrite`, then stores `parakeet-v3` in its place, in
    /// one transaction; returns whether it did. No id, or `parakeet-v3`,
    /// changes nothing and calls nothing. The app calls it at launch,
    /// before anything reads the engine, and records the one-time notice
    /// in `before_rewrite`, so a crash between the two shows the notice
    /// once too often rather than switching the engine without a word.
    pub fn retire_speech_engine(&self, before_rewrite: impl FnOnce()) -> Result<bool> {
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
            if id.as_str() == Some(PARAKEET_V3_ENGINE_ID) {
                return Ok(false);
            }
            before_rewrite();
            execute_cached(
                transaction,
                "UPDATE setting SET value = ?2 WHERE key = ?1",
                params![
                    SPEECH_ENGINE_KEY,
                    json::to_column_string(&PARAKEET_V3_ENGINE_ID)?
                ],
            )?;
            Ok(true)
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
