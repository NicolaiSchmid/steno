//! The small services: the clock, the folder usage, the audio device list,
//! the preferences file, and the stubs `WP8` replaces (clip player, QR
//! encoder, permissions, login item, updater).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use chrono::{DateTime, Utc};
use steno_host::services::{AudioDevices, Clock, FolderUsage, InputDevice, Preferences};

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
    fn measure(&self, folder: &Path) -> Result<i64, String> {
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
        walk(folder)
            .map(|bytes| i64::try_from(bytes).unwrap_or(i64::MAX))
            .map_err(|error| error.to_string())
    }
}

/// The input devices. Core Audio enumerates them on the Mac (`WP5`'s live
/// backend); elsewhere the list is empty until the `PipeWire` and `WASAPI`
/// backends land, and the default input is used.
#[derive(Debug, Default)]
pub struct PlatformAudioDevices;

impl AudioDevices for PlatformAudioDevices {
    fn inputs(&self) -> Result<Vec<InputDevice>, String> {
        Ok(Vec::new())
    }
}

/// Boolean flags in `preferences.json` under the support directory
/// (Swift: `UserDefaults`), for the first-launch markers.
#[derive(Debug)]
pub struct FilePreferences {
    path: PathBuf,
    flags: Mutex<BTreeMap<String, bool>>,
}

impl FilePreferences {
    #[must_use]
    pub fn new(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        let flags = std::fs::read(&path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        FilePreferences {
            path,
            flags: Mutex::new(flags),
        }
    }
}

impl Preferences for FilePreferences {
    fn flag(&self, key: &str) -> bool {
        self.flags
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(key)
            .copied()
            .unwrap_or(false)
    }

    fn set_flag(&self, key: &str, value: bool) {
        let mut flags = self
            .flags
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        flags.insert(key.to_owned(), value);
        if let Some(parent) = self.path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(data) = serde_json::to_vec_pretty(&*flags) {
            let _ = std::fs::write(&self.path, data);
        }
    }
}
