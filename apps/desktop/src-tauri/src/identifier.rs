//! The app's identifier, `com.nicolaischmid.steno.desktop`, and the one
//! earlier desktop builds carried, [`EARLIER`]. No data is named after
//! either: the database, settings, audio, models and the panels' anchor
//! live in the support directory. Under a new identifier, Tauri's own
//! directories, the single-instance guard and macOS permissions start afresh.

/// The identifier of the desktop builds up to `desktop-v0.1.0-rc.2`; its
/// config directory holds the panels' anchor such a build saved.
pub const EARLIER: &str = "uno.schmid.steno.desktop";

#[cfg(test)]
mod tests {
    use steno_core::{DatabaseLock, DatabaseLockError, StenoPaths};

    use super::*;

    /// The identifier in `tauri.conf.json`.
    fn configured() -> String {
        let config: serde_json::Value =
            serde_json::from_str(include_str!("../tauri.conf.json")).unwrap();
        config["identifier"].as_str().unwrap().to_owned()
    }

    #[test]
    fn the_identifier_is_the_desktop_one_on_every_platform() {
        assert_eq!(configured(), "com.nicolaischmid.steno.desktop");
        assert_ne!(configured(), EARLIER);
    }

    /// No other `tauri*.conf.json` sets another identifier.
    #[test]
    fn no_other_config_file_sets_an_identifier() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let mut checked = Vec::new();
        for entry in std::fs::read_dir(dir).unwrap() {
            let name = entry.unwrap().file_name().to_string_lossy().into_owned();
            if !(name.starts_with("tauri") && name.ends_with(".conf.json"))
                || name == "tauri.conf.json"
            {
                continue;
            }
            let text = std::fs::read_to_string(dir.join(&name)).unwrap();
            let config: serde_json::Value = serde_json::from_str(&text).unwrap();
            assert!(
                config.get("identifier").is_none(),
                "{name} sets one: {config}"
            );
            checked.push(name);
        }
        checked.sort();
        assert!(
            checked.len() >= 3 && checked.contains(&"tauri.linux.conf.json".to_owned()),
            "{checked:?}"
        );
    }

    /// The bundle keeps the Swift app's Sparkle public key, which its last
    /// update to this app needs; inert here.
    #[test]
    fn the_info_plist_carries_the_swift_apps_sparkle_key() {
        let plist = include_str!("../Info.plist");
        let key = plist
            .split_once("<key>SUPublicEDKey</key>")
            .and_then(|(_, rest)| rest.trim_start().strip_prefix("<string>"))
            .and_then(|rest| rest.split_once("</string>"))
            .map(|(key, _)| key);
        assert_eq!(key, Some("RxaX7phoHvb7M0P4yaOC7zngDo+lqlOE6Iq89UtOuQI="));
    }

    /// Apps under both identifiers share the database lock, so the second
    /// is refused.
    #[test]
    fn both_identifiers_share_one_database_lock() {
        let home = tempfile::tempdir().unwrap();
        let support = StenoPaths::support_directory(|name| {
            ["HOME", "XDG_DATA_HOME", "APPDATA"]
                .contains(&name)
                .then(|| home.path().as_os_str().to_owned())
        });
        for identifier in [EARLIER, configured().as_str()] {
            assert!(
                !support.to_string_lossy().contains(identifier),
                "{}",
                support.display()
            );
        }
        std::fs::create_dir_all(&support).unwrap();
        let database = StenoPaths::new(&support).database_path();
        let _first = DatabaseLock::acquire(&database).unwrap();
        assert!(matches!(
            DatabaseLock::acquire(&database),
            Err(DatabaseLockError::Held(_))
        ));
    }
}
