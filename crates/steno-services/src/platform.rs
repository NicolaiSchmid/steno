//! The platform services behind the host's small traits: the wall clock,
//! the folder usage walk, the input device list, and the first-launch
//! flags in `preferences.json`. Swift: `Date()`,
//! `AudioSettingsViewModel.measureFolderUsage` in
//! `apps/macos/Steno/Settings/AudioSettingsViewModel.swift`, the Core Audio
//! device list, and `UserDefaults` in `apps/macos/Steno/AppController.swift`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use crate::files::{may_lose_recent_writes, read_json, write_json};
use chrono::{DateTime, Utc};
use serde_json::Value;
use steno_core::protocols::BoundaryResult;
use steno_host::services::{AudioDevices, Clock, FolderUsage, InputDevice, Preferences};

/// `Utc::now`.
#[derive(Debug, Default)]
pub struct WallClock;

impl Clock for WallClock {
    fn now(&self) -> DateTime<Utc> {
        Utc::now()
    }
}

/// Walks the folder and sums file sizes; asks the durable writes whether
/// its drive may lose recent writes ([`may_lose_recent_writes`]).
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

    fn may_lose_recent_writes(&self, folder: &Path) -> bool {
        may_lose_recent_writes(folder)
    }
}

/// The input devices, from the audio crate's live backend: Core Audio on
/// the Mac, the `PipeWire` registry on Linux, WASAPI on Windows; an empty
/// list on other targets. A failed enumeration is logged and returned, so
/// Settings shows Swift's "Microphones could not be listed." over an empty
/// list and keeps working.
#[derive(Debug, Default)]
pub struct PlatformAudioDevices;

impl AudioDevices for PlatformAudioDevices {
    fn inputs(&self) -> BoundaryResult<Vec<InputDevice>> {
        #[cfg(any(target_os = "macos", target_os = "linux", windows))]
        {
            let devices = steno_audio::capture::live::AudioDevices::inputs()
                .inspect_err(|error| tracing::warn!("listing the input devices failed: {error}"))?;
            Ok(devices
                .into_iter()
                .map(|device| InputDevice {
                    uid: device.uid,
                    name: device.name,
                })
                .collect())
        }
        #[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
        Ok(Vec::new())
    }
}

/// Boolean flags in `preferences.json` under the support directory
/// (Swift: `UserDefaults`), for the first-launch markers. A write replaces
/// the file in one durable step ([`write_json`]). A value that is not a
/// flag (a newer build's) is kept as it is. The file is read with
/// [`read_json`]: a file that does not parse is moved aside and logged
/// before the flags start empty; a file that cannot be read for another
/// reason, or that cannot be moved aside, is left alone and never written,
/// and the flags of this run live in memory only: a first-launch marker
/// shown again is better than a file replaced unread.
#[derive(Debug)]
pub struct FilePreferences {
    path: PathBuf,
    values: Mutex<BTreeMap<String, Value>>,
    /// False when the file on disk could not be read or set aside.
    writable: bool,
}

impl FilePreferences {
    #[must_use]
    pub fn new(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        let (values, writable) = read_json(&path);
        FilePreferences {
            path,
            values: Mutex::new(values),
            writable,
        }
    }
}

impl Preferences for FilePreferences {
    fn flag(&self, key: &str) -> bool {
        self.values
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(key)
            .and_then(Value::as_bool)
            .unwrap_or(false)
    }

    fn set_flag(&self, key: &str, value: bool) {
        let mut values = self
            .values
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        values.insert(key.to_owned(), Value::Bool(value));
        if !self.writable {
            return;
        }
        if let Err(error) = write_json(&self.path, &*values) {
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

    /// A file that does not parse and cannot be moved aside (its folder
    /// is read-only) stays as it is and is never written.
    #[cfg(unix)]
    #[test]
    fn a_corrupt_file_that_cannot_be_moved_aside_is_never_written() {
        use std::os::unix::fs::PermissionsExt as _;
        let directory = tempfile::tempdir().unwrap();
        let folder = directory.path().join("support");
        std::fs::create_dir(&folder).unwrap();
        let path = folder.join("preferences.json");
        std::fs::write(&path, b"{\"seen\": tr").unwrap();
        std::fs::set_permissions(&folder, std::fs::Permissions::from_mode(0o500)).unwrap();
        if std::fs::File::create(folder.join("probe")).is_ok() {
            // Root writes past the mode; nothing to test.
            return;
        }
        let preferences = FilePreferences::new(&path);
        // Writable again: only the failed move aside keeps the file from
        // being written.
        std::fs::set_permissions(&folder, std::fs::Permissions::from_mode(0o700)).unwrap();
        preferences.set_flag("onboarded", true);
        assert!(preferences.flag("onboarded"), "the run still has its flag");
        assert_eq!(std::fs::read(&path).unwrap(), b"{\"seen\": tr");
        assert_eq!(names(&folder), vec!["preferences.json"]);
    }

    /// The disk's folder usage asks the durable writes about the folder's
    /// drive: a test folder is not reported, and on Windows the FAT32 drive
    /// CI mounts (`STENO_FAT32_VOLUME`) is.
    #[test]
    fn the_disk_folder_usage_reports_a_drive_that_may_lose_recent_writes() {
        let directory = tempfile::tempdir().unwrap();
        assert!(!DiskFolderUsage.may_lose_recent_writes(directory.path()));
        if cfg!(windows)
            && let Some(volume) = std::env::var_os("STENO_FAT32_VOLUME")
        {
            assert!(DiskFolderUsage.may_lose_recent_writes(Path::new(&volume)));
        }
    }
}
