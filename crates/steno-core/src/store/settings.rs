//! `Settings` as one row per property in the `setting` table, each value a
//! one-line `StenoJSON` fragment. A property missing from the table loads
//! as its default and an unknown row is ignored, so a property can be added
//! without a migration.
//! Swift: `Sources/StenoCore/Storage/SettingsStore.swift`.

use rusqlite::params;
use serde_json::Value;

use super::{Result, Store, query_all};
use crate::json;
use crate::model::Settings;

/// `settings` as a JSON object. Through text, not `to_value`, so an `f32`
/// keeps its shortest form (`0.6`) as in [`json::to_column_string`].
fn object(settings: &Settings) -> Result<serde_json::Map<String, Value>> {
    Ok(serde_json::from_str(&serde_json::to_string(settings)?)?)
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

    /// Writes every property, removing rows for properties that are now
    /// `None`.
    pub fn save_settings(&self, settings: &Settings) -> Result<()> {
        let object = object(settings)?;
        let mut keys: Vec<&String> = object.keys().collect();
        keys.sort();
        self.write(|transaction| {
            transaction.execute("DELETE FROM setting", [])?;
            for key in keys {
                let fragment = json::to_column_string(&object[key])?;
                transaction.execute(
                    "INSERT INTO setting (key, value) VALUES (?1, ?2)",
                    params![key, fragment],
                )?;
            }
            Ok(())
        })
    }
}
