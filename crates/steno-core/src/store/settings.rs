//! `Settings` as one row per property in the `setting` table, each value a
//! one-line `StenoJSON` fragment. A missing property loads as its default.
//! A row this build does not know (a newer build's, or the Swift app's,
//! which shares the file until the Mac cutover) is ignored on load and kept
//! on save. A row that cannot be read fails the load with
//! [`StoreError::UndecodableSetting`], naming the key but not the value.

use rusqlite::{Connection, OptionalExtension as _, params};
use serde::Deserialize;
use serde::de::{self, Deserializer, Visitor};
use serde_json::Value;

use super::{Result, Store, StoreError, execute_cached, query_all};
use crate::json;
use crate::model::{AudioRetention, Settings};

/// The one engine the app runs: what [`Store::retire_speech_engine`]
/// stores in place of any other id. The Swift app reads the same id.
pub const PARAKEET_V3_ENGINE_ID: &str = "parakeet-v3";

const SPEECH_ENGINE_KEY: &str = "speechEngineID";

/// The key [`Settings::handover_port`] is stored under, which users type
/// by hand.
const HANDOVER_PORT_KEY: &str = "handoverPort";

const DEFAULT_RETENTION_KEY: &str = "defaultRetention";

/// What is wrong with a stored setting, in words that never quote the
/// value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingProblem {
    /// The row's text is not JSON.
    NotJson,
    /// The row is JSON, but not of the shape the setting has in this build.
    WrongShape,
}

impl std::fmt::Display for SettingProblem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            SettingProblem::NotJson => "is not in a form Steno can read",
            SettingProblem::WrongShape => "holds a value this version of Steno does not understand",
        })
    }
}

/// `settings` as a JSON object. Through text, so an `f32` keeps its
/// shortest form (`0.6`) as in [`json::to_column_string`].
fn object(settings: &Settings) -> Result<serde_json::Map<String, Value>> {
    Ok(serde_json::from_str(&serde_json::to_string(settings)?)?)
}

/// Every key `Settings` reads and writes, the `None` ones included, from
/// its derived `Deserialize`, so a new field needs no list kept in step.
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

/// `merged` (the defaults with every stored row over them) as `Settings`.
/// A failure never carries serde's message, which quotes the value: it
/// names the first row that fails on its own, or none
/// ([`StoreError::UndecodableSettings`]).
fn decode(defaults: &Settings, merged: &Value) -> Result<Settings> {
    Settings::deserialize(merged)
        .map_err(|_| undecodable_row(defaults, merged).unwrap_or(StoreError::UndecodableSettings))
}

/// The first stored key whose value alone over `defaults` does not decode.
fn undecodable_row(defaults: &Settings, merged: &Value) -> Option<StoreError> {
    let defaults = object(defaults).ok()?;
    merged
        .as_object()?
        .iter()
        .filter(|(key, value)| defaults.get(*key) != Some(*value))
        .find(|(key, value)| {
            let mut alone = defaults.clone();
            alone.insert((*key).clone(), (*value).clone());
            Settings::deserialize(&Value::Object(alone)).is_err()
        })
        .map(|(key, _)| StoreError::UndecodableSetting {
            key: key.clone(),
            problem: SettingProblem::WrongShape,
        })
}

fn stored_rows(connection: &Connection) -> Result<Vec<(String, String)>> {
    query_all(connection, "SELECT key, value FROM setting", [], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })
}

fn overlaid(defaults: &Settings, rows: Vec<(String, String)>) -> Result<Settings> {
    let mut merged = object(defaults)?;
    for (key, value) in rows {
        if key == HANDOVER_PORT_KEY && json::from_column_str::<Option<u16>>(&value).is_err() {
            tracing::warn!(
                target: "steno::store",
                "the stored {HANDOVER_PORT_KEY} is not a port from 0 to 65535, so it is ignored"
            );
            continue;
        }
        let Ok(value) = json::from_column_str(&value) else {
            return Err(StoreError::UndecodableSetting {
                key,
                problem: SettingProblem::NotJson,
            });
        };
        merged.insert(key, value);
    }
    decode(defaults, &Value::Object(merged))
}

/// The default retention from its own row alone, inside the stop's save,
/// so no other setting can fail it. A missing row is the default's; an
/// unreadable one is [`AudioRetention::KeepForever`], which deletes
/// nothing, with a warning that names the key only.
pub(super) fn default_retention(connection: &Connection) -> Result<AudioRetention> {
    let stored: Option<String> = connection
        .query_row(
            "SELECT value FROM setting WHERE key = ?1",
            params![DEFAULT_RETENTION_KEY],
            |row| row.get(0),
        )
        .optional()?;
    let Some(stored) = stored else {
        return Ok(Settings::default().default_retention);
    };
    Ok(json::from_column_str(&stored).unwrap_or_else(|_| {
        tracing::warn!(
            target: "steno::store",
            "the stored {DEFAULT_RETENTION_KEY} is not a retention rule, so this recording's audio is kept"
        );
        AudioRetention::KeepForever
    }))
}

impl Store {
    pub fn settings(&self) -> Result<Settings> {
        self.settings_with_defaults(&Settings::default())
    }

    /// [`Store::settings`] over explicit defaults.
    pub fn settings_with_defaults(&self, defaults: &Settings) -> Result<Settings> {
        let rows = self.read(stored_rows)?;
        overlaid(defaults, rows)
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

    /// When an engine id other than [`PARAKEET_V3_ENGINE_ID`] is stored,
    /// calls `before_rewrite`, then stores `parakeet-v3` in one
    /// transaction; returns whether it did. A row that is not a JSON string
    /// changes nothing (the load refuses it). The app records its one-time
    /// notice in `before_rewrite`, so a crash between the two shows the
    /// notice once too often rather than switching silently.
    pub fn retire_speech_engine(&self, before_rewrite: impl FnOnce()) -> Result<bool> {
        self.write(|transaction| {
            let stored: Option<String> = transaction
                .query_row(
                    "SELECT value FROM setting WHERE key = ?1",
                    params![SPEECH_ENGINE_KEY],
                    |row| row.get(0),
                )
                .optional()?;
            // Not a JSON string: the settings load refuses it by its key.
            let Some(Value::String(id)) =
                stored.and_then(|stored| json::from_column_str(&stored).ok())
            else {
                return Ok(false);
            };
            if id == PARAKEET_V3_ENGINE_ID {
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
            handover_port: Some(23820),
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

    /// Stores `value` as the row of `key`, past `save_settings`.
    fn store_with_row(key: &str, value: &str) -> Store {
        let store = Store::in_memory().unwrap();
        store.save_settings(&Settings::default()).unwrap();
        store
            .write(|transaction| {
                execute_cached(
                    transaction,
                    "INSERT INTO setting (key, value) VALUES (?1, ?2) \
                     ON CONFLICT (key) DO UPDATE SET value = excluded.value",
                    params![key, value],
                )?;
                Ok(())
            })
            .unwrap();
        store
    }

    /// One unreadable row stops the load, and the error names its key and
    /// what is wrong, never its value.
    #[test]
    fn an_undecodable_row_is_named_by_its_key_without_its_value() {
        for (key, value, problem) in [
            (
                "llmBaseURL",
                "https://user:sk-secret@example.com",
                SettingProblem::NotJson,
            ),
            (
                "llmBaseURL",
                "{\"token\":\"sk-secret\"}",
                SettingProblem::WrongShape,
            ),
            (
                "defaultRetention",
                "\"sk-secret-rule\"",
                SettingProblem::WrongShape,
            ),
            ("launchAtLogin", "\"sk-secret\"", SettingProblem::WrongShape),
        ] {
            let error = store_with_row(key, value).settings().unwrap_err();
            let text = error.to_string();
            assert!(
                matches!(
                    &error,
                    StoreError::UndecodableSetting { key: named, problem: found }
                        if named == key && *found == problem
                ),
                "{key} = {value}: {error:?}"
            );
            assert_eq!(text, format!("the stored setting {key} {problem}"));
            assert!(!text.contains("sk-secret"), "{text}");
            assert!(!format!("{error:?}").contains("sk-secret"), "{error:?}");
        }
        assert_eq!(
            SettingProblem::NotJson.to_string(),
            "is not in a form Steno can read"
        );
        assert_eq!(
            SettingProblem::WrongShape.to_string(),
            "holds a value this version of Steno does not understand"
        );
    }

    /// Settings that fail as a whole while no row fails alone give an error
    /// without serde's message.
    #[test]
    fn settings_that_fail_together_name_no_value() {
        let error = decode(&Settings::default(), &Value::from("sk-secret")).unwrap_err();
        assert!(
            matches!(error, StoreError::UndecodableSettings),
            "{error:?}"
        );
        assert_eq!(
            error.to_string(),
            "the stored settings cannot be read together"
        );
        assert!(!format!("{error:?}").contains("sk-secret"));
    }

    /// A speech engine id that is not a string is left for the load to
    /// refuse, never quietly replaced.
    #[test]
    fn a_speech_engine_id_that_is_not_a_string_is_refused_not_replaced() {
        for (value, problem) in [
            ("4242424242", SettingProblem::WrongShape),
            ("parakeet-v3", SettingProblem::NotJson),
        ] {
            let store = store_with_row(SPEECH_ENGINE_KEY, value);
            assert!(
                !store
                    .retire_speech_engine(|| panic!("called for {value}"))
                    .unwrap()
            );
            let stored: String = store
                .read(|connection| {
                    Ok(connection.query_row(
                        "SELECT value FROM setting WHERE key = ?1",
                        params![SPEECH_ENGINE_KEY],
                        |row| row.get(0),
                    )?)
                })
                .unwrap();
            assert_eq!(stored, value);
            assert!(
                matches!(
                    store.settings(),
                    Err(StoreError::UndecodableSetting { ref key, problem: found })
                        if key == SPEECH_ENGINE_KEY && found == problem
                ),
                "{value}"
            );
        }
    }

    /// An unknown row, and every row this build wrote, load as before.
    #[test]
    fn unknown_and_well_formed_rows_still_load() {
        let store = store_with_row("someLaterSetting", "{\"any\":1}");
        assert_eq!(store.settings().unwrap(), Settings::default());
    }

    #[test]
    fn the_handover_port_is_stored_under_the_key_the_readme_names() {
        let settings = Settings {
            handover_port: Some(23900),
            ..Settings::default()
        };
        assert_eq!(object(&settings).unwrap()[HANDOVER_PORT_KEY], 23900);
    }
}
