//! The platform services behind the host's small traits: the wall clock,
//! the folder usage walk, the input device list, and the first-launch
//! flags in `preferences.json`. Swift: `Date()`,
//! `AudioSettingsViewModel.measureFolderUsage` in
//! `apps/macos/Steno/Settings/AudioSettingsViewModel.swift`, the Core Audio
//! device list, and `UserDefaults` in `apps/macos/Steno/AppController.swift`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use chrono::{DateTime, Utc};
use serde_json::Value;
use steno_core::protocols::BoundaryResult;
use steno_host::services::{AudioDevices, Clock, FolderUsage, InputDevice, Preferences};
use steno_pipeline::files::{Access, create_dir_all_durably, replace_file, set_aside};

/// `Utc::now`.
#[derive(Debug, Default)]
pub struct WallClock;

impl Clock for WallClock {
    fn now(&self) -> DateTime<Utc> {
        Utc::now()
    }
}

/// Walks the folder and sums file sizes.
#[derive(Debug, Default)]
pub struct DiskFolderUsage;

impl FolderUsage for DiskFolderUsage {
    fn measure(&self, folder: &Path) -> BoundaryResult<i64> {
        fn walk(path: &Path) -> std::io::Result<u64> {
            let mut total = 0;
            for entry in std::fs::read_dir(path)? {
                let entry = entry?;
                let kind = entry.file_type()?;
                if kind.is_dir() {
                    total += walk(&entry.path())?;
                } else if kind.is_file() {
                    total += entry.metadata()?.len();
                }
            }
            Ok(total)
        }
        if !folder.exists() {
            return Ok(0);
        }
        Ok(walk(folder).map(|bytes| i64::try_from(bytes).unwrap_or(i64::MAX))?)
    }
}

/// The input devices. Core Audio enumerates them on the Mac through the
/// audio crate's live backend; elsewhere the list is empty until the
/// `PipeWire` and WASAPI backends land, and the default input is used.
#[derive(Debug, Default)]
pub struct PlatformAudioDevices;

impl AudioDevices for PlatformAudioDevices {
    fn inputs(&self) -> BoundaryResult<Vec<InputDevice>> {
        Ok(Vec::new())
    }
}

/// Boolean flags in `preferences.json` under the support directory
/// (Swift: `UserDefaults`), for the first-launch markers. A write replaces
/// the file in one durable step ([`replace_file`]). A value that is not a
/// flag (a newer build's) is kept as it is. A file that does not parse is
/// moved aside ([`set_aside`]) and logged before the flags start empty; a
/// file that cannot be read for another reason, or that cannot be moved
/// aside, is left alone and never written, and the flags of this run live
/// in memory only: a first-launch marker shown again is better than a file
/// replaced unread.
#[derive(Debug)]
pub struct FilePreferences {
    path: PathBuf,
    state: Mutex<PreferencesState>,
}

#[derive(Debug)]
struct PreferencesState {
    values: BTreeMap<String, Value>,
    /// False when the file on disk could not be read or set aside.
    writable: bool,
}

impl FilePreferences {
    #[must_use]
    pub fn new(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        let (values, writable) = match std::fs::read(&path) {
            Ok(bytes) => match serde_json::from_slice(&bytes) {
                Ok(values) => (values, true),
                Err(error) => match set_aside(&path) {
                    Ok(aside) => {
                        tracing::warn!(
                            "{} did not parse ({error}); moved it to {} and started empty",
                            path.display(),
                            aside.display()
                        );
                        (BTreeMap::new(), true)
                    }
                    Err(move_error) => {
                        tracing::warn!(
                            "{} did not parse ({error}) and could not be moved aside \
                             ({move_error}); it stays and is not written",
                            path.display()
                        );
                        (BTreeMap::new(), false)
                    }
                },
            },
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => (BTreeMap::new(), true),
            Err(error) => {
                tracing::warn!(
                    "{} could not be read ({error}); it stays and is not written",
                    path.display()
                );
                (BTreeMap::new(), false)
            }
        };
        FilePreferences {
            path,
            state: Mutex::new(PreferencesState { values, writable }),
        }
    }

    fn state(&self) -> std::sync::MutexGuard<'_, PreferencesState> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

impl Preferences for FilePreferences {
    fn flag(&self, key: &str) -> bool {
        self.state()
            .values
            .get(key)
            .and_then(Value::as_bool)
            .unwrap_or(false)
    }

    fn set_flag(&self, key: &str, value: bool) {
        let mut state = self.state();
        state.values.insert(key.to_owned(), Value::Bool(value));
        if !state.writable {
            return;
        }
        let written = serde_json::to_vec_pretty(&state.values)
            .map_err(std::io::Error::from)
            .and_then(|data| {
                if let Some(parent) = self.path.parent() {
                    create_dir_all_durably(parent)?;
                }
                replace_file(&self.path, &data, Access::Default)
            });
        if let Err(error) = written {
            tracing::warn!("{} could not be written: {error}", self.path.display());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(directory: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(directory)
            .unwrap()
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    /// A flag lands in one whole file, beside which no temporary is left,
    /// and a value this build does not read as a flag is written back.
    #[test]
    fn set_flag_replaces_the_whole_file_and_keeps_other_values() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("support").join("preferences.json");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, br#"{"futureCount":3,"seen":true}"#).unwrap();
        let preferences = FilePreferences::new(&path);
        assert!(preferences.flag("seen"));
        assert!(!preferences.flag("futureCount"));
        preferences.set_flag("onboarded", true);
        assert_eq!(names(path.parent().unwrap()), vec!["preferences.json"]);
        let stored: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(
            stored,
            serde_json::json!({"futureCount": 3, "onboarded": true, "seen": true})
        );
        assert!(FilePreferences::new(&path).flag("onboarded"));
    }

    /// The write replaces the file rather than writing into it: a second
    /// name for the old file still holds the old bytes, so a crash in the
    /// middle of a write cannot leave a torn file under the real name.
    #[test]
    fn set_flag_writes_a_new_file_instead_of_into_the_old_one() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("preferences.json");
        let old = br#"{"seen":true}"#;
        std::fs::write(&path, old).unwrap();
        let second_name = directory.path().join("old-name");
        std::fs::hard_link(&path, &second_name).unwrap();
        FilePreferences::new(&path).set_flag("onboarded", true);
        assert_eq!(std::fs::read(&second_name).unwrap(), old);
        assert!(FilePreferences::new(&path).flag("onboarded"));
    }

    /// A file that does not parse is moved aside with its bytes, and the
    /// next write cannot replace it.
    #[test]
    fn a_corrupt_file_is_moved_aside_not_overwritten() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("preferences.json");
        std::fs::write(&path, b"{\"seen\": tr").unwrap();
        let preferences = FilePreferences::new(&path);
        assert!(!preferences.flag("seen"));
        preferences.set_flag("onboarded", true);

        let names = names(directory.path());
        assert_eq!(names.len(), 2, "{names:?}");
        assert_eq!(names[0], "preferences.json");
        assert!(
            names[1].starts_with("preferences.json.corrupt-"),
            "{names:?}"
        );
        assert_eq!(
            std::fs::read(directory.path().join(&names[1])).unwrap(),
            b"{\"seen\": tr"
        );
        assert!(FilePreferences::new(&path).flag("onboarded"));
    }

    /// A file that exists but cannot be read stays as it is: the flags
    /// of the run are kept in memory and nothing is written.
    #[cfg(unix)]
    #[test]
    fn an_unreadable_file_is_never_written() {
        use std::os::unix::fs::PermissionsExt as _;
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("preferences.json");
        std::fs::write(&path, br#"{"seen":true}"#).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).unwrap();
        if std::fs::read(&path).is_ok() {
            // Root reads past the mode; nothing to test.
            return;
        }
        let preferences = FilePreferences::new(&path);
        preferences.set_flag("onboarded", true);
        assert!(preferences.flag("onboarded"), "the run still has its flag");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), br#"{"seen":true}"#);
        assert_eq!(names(directory.path()), vec!["preferences.json"]);
    }
}
