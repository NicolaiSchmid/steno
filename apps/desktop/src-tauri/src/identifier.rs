//! The app's identifier, `com.nicolaischmid.steno.desktop` on every
//! platform (`identifier` in `tauri.conf.json`; D5 of
//! `.plans/2026-10-07-stable-promotion.md`), and the one the desktop
//! builds before it carried, [`EARLIER`].
//!
//! No data is named after either, so changing the identifier moves no
//! data; what it resets is listed below. The database, its lock, the
//! preferences, the audio, the models and the panels' anchor
//! (`panel_anchor`) live in the support directory
//! (`steno_core::StenoPaths`), and every secret under the keyring
//! service `uno.schmid.steno.mac`. What is named after the identifier,
//! and so starts afresh under the new one: Tauri's app config and data
//! directories and the webview's data (the web app keeps nothing there),
//! the single-instance guard (a socket in `/tmp` on macOS, the session
//! bus name on Linux, a named mutex on Windows), the Windows
//! `AppUserModelID`, and on macOS what the system files under the bundle
//! id: the permissions and the login item (`autostart`). An app under the
//! earlier identifier and one under the new one miss each other's
//! single-instance guard, so both may start; the database's lock
//! (`steno_core::DatabaseLock`, beside the database) refuses the second
//! before it builds anything (`Refusal::DatabaseHeld` in `main.rs`).

/// The identifier of the desktop builds up to `desktop-v0.1.0-rc.2` and
/// the `desktop-beta` lane before this one. Its app config directory
/// holds the panels' anchor such a build saved (`panel_anchor`).
pub const EARLIER: &str = "uno.schmid.steno.desktop";

#[cfg(test)]
mod tests {
    use steno_core::{DatabaseLock, DatabaseLockError, StenoPaths};

    use super::*;

    /// The identifier in `tauri.conf.json`, which every platform's build
    /// takes.
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

    /// No file merged over `tauri.conf.json` (the platform files, the
    /// release files a `--config` names) sets another identifier: every
    /// `tauri*.conf.json` beside it is checked, so a new one is too.
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

    /// The bundle keeps the Swift app's Sparkle key (stable plan S6):
    /// Sparkle refuses an update without a public key, so the Swift app's
    /// last update to this one carries it, inert here. The key is
    /// `SUPublicEDKey` in `apps/macos/project.yml`.
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

    /// An app under the earlier identifier and one under the new one find
    /// the same support directory, which names neither, and so the same
    /// database lock: the second one is refused.
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
