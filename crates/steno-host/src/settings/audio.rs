//! Recording: the two recording permissions, the input device, the
//! recordings folder with its disk usage, and the "Keep recordings" rule.
//! Switching the rule to Forever keeps every recording still on disk; a
//! shorter rule applies to new recordings only.
//! Swift: `Settings/AudioSettingsViewModel.swift`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use steno_bridge::{PermissionKind, PermissionState, Platform, RetentionMode};
use steno_core::paths::{file_url, file_url_path};
use steno_core::protocols::BoundaryResult;
use steno_core::{AudioRetention, Store};

use super::{SectionError, update_settings};
use crate::labels::retention_footnote;
use crate::services::{InputDevice, Services};

/// The recordings folder's logical size. Swift: `AudioSettingsViewModel.FolderUsage`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FolderUsageState {
    Measuring,
    Bytes(i64),
    Unavailable,
}

/// The two permissions the section shows, in order.
pub const RECORDING_PERMISSIONS: [PermissionKind; 2] =
    [PermissionKind::Microphone, PermissionKind::SystemAudio];

/// The ones of [`RECORDING_PERMISSIONS`] `platform` has: both on the Mac,
/// the microphone alone on Windows and Linux.
pub fn recording_permissions(platform: Platform) -> impl Iterator<Item = PermissionKind> {
    RECORDING_PERMISSIONS
        .into_iter()
        .filter(move |kind| PermissionKind::for_platform(platform).contains(kind))
}

/// The stepper's range for the days rule.
pub const DAY_RANGE: std::ops::RangeInclusive<i64> = 1..=3650;

#[derive(Debug)]
pub struct AudioSettingsViewModel {
    pub devices: Vec<InputDevice>,
    /// The last list failed, so `devices` says nothing about which are
    /// connected.
    pub devices_failed: bool,
    pub input_device_uid: Option<String>,
    pub audio_folder: PathBuf,
    pub folder_usage: FolderUsageState,
    /// Whether a power cut may lose recent recordings in the folder (a
    /// Windows drive that is neither NTFS nor `ReFS`, or a network drive);
    /// Settings warns.
    pub folder_may_lose_recent_writes: bool,
    pub retention_mode: RetentionMode,
    pub retention_days: i64,
    /// How many recordings the last switch to Forever kept; `None` until then.
    pub kept_forever: Option<i64>,
    pub permissions: BTreeMap<PermissionKind, PermissionState>,
    pub requesting: Option<PermissionKind>,
    pub errors: SectionError,
}

impl AudioSettingsViewModel {
    #[must_use]
    pub fn new() -> Self {
        AudioSettingsViewModel {
            devices: Vec::new(),
            devices_failed: false,
            input_device_uid: None,
            audio_folder: PathBuf::new(),
            folder_usage: FolderUsageState::Measuring,
            folder_may_lose_recent_writes: false,
            retention_mode: RetentionMode::KeepForever,
            retention_days: 30,
            kept_forever: None,
            permissions: BTreeMap::new(),
            requesting: None,
            errors: SectionError::default(),
        }
    }

    /// The rule the picker and the stepper currently describe.
    #[must_use]
    pub fn retention(&self) -> AudioRetention {
        match self.retention_mode {
            RetentionMode::KeepForever => AudioRetention::KeepForever,
            RetentionMode::KeepDays => AudioRetention::KeepDays(self.retention_days),
            RetentionMode::DeleteAfterProcessing => AudioRetention::DeleteAfterProcessing,
        }
    }

    /// The sentence under the picker for the current selection.
    #[must_use]
    pub fn footnote(&self) -> String {
        retention_footnote(self.retention())
    }

    #[must_use]
    pub fn folder_name(&self) -> String {
        self.audio_folder
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default()
    }

    pub fn load(&mut self, store: &Store, services: &Services) {
        match store.settings() {
            Ok(settings) => {
                self.input_device_uid = settings.input_device_uid;
                self.audio_folder = file_url_path(&settings.audio_folder)
                    .unwrap_or_else(|| PathBuf::from(&settings.audio_folder));
                match settings.default_retention {
                    AudioRetention::DeleteAfterProcessing => {
                        self.retention_mode = RetentionMode::DeleteAfterProcessing;
                    }
                    AudioRetention::KeepDays(days) => {
                        self.retention_mode = RetentionMode::KeepDays;
                        self.retention_days = days;
                    }
                    AudioRetention::KeepForever => self.retention_mode = RetentionMode::KeepForever,
                }
            }
            Err(error) => self.errors.fail("Settings could not be loaded.", error),
        }
        self.refresh_devices(services);
        self.refresh_permissions(services);
        self.measure_folder_usage(services);
    }

    pub fn refresh_devices(&mut self, services: &Services) {
        self.apply_devices(services.audio_devices.inputs());
    }

    /// A device list's outcome, for a caller that listed without holding
    /// the host's lock (on Linux a list may wait seconds for `PipeWire`).
    pub fn apply_devices(&mut self, listed: BoundaryResult<Vec<InputDevice>>) {
        self.devices_failed = listed.is_err();
        match listed {
            Ok(devices) => self.devices = devices,
            Err(error) => {
                self.devices = Vec::new();
                self.errors.fail("Microphones could not be listed.", error);
            }
        }
    }

    pub fn set_input_device(&mut self, uid: Option<String>, store: &Store) {
        self.input_device_uid.clone_from(&uid);
        self.save(store, move |settings| settings.input_device_uid = uid);
    }

    /// Saves `folder` as the audio folder. The folder it leaves, as the
    /// stored settings name it, is remembered first
    /// ([`Recorder::remember_audio_folder`](crate::services::Recorder::remember_audio_folder)):
    /// a recording started there goes on there, and crash recovery must
    /// look for it there (Rust only: Swift had no recovery). Nothing is
    /// moved. Swift: `AudioSettingsViewModel.setAudioFolder`.
    pub fn set_audio_folder(&mut self, folder: &Path, store: &Store, services: &Services) {
        let leaving = store
            .settings()
            .ok()
            .and_then(|settings| file_url_path(&settings.audio_folder))
            .unwrap_or_else(|| self.audio_folder.clone());
        if leaving != folder {
            services.recorder.remember_audio_folder(&leaving);
        }
        self.audio_folder = folder.to_path_buf();
        let url = file_url(folder, true);
        self.save(store, move |settings| settings.audio_folder = url);
        self.measure_folder_usage(services);
    }

    pub fn reveal_folder(&self, services: &Services) {
        services.opener.reveal(&self.audio_folder);
    }

    /// Swift measured off the main actor and dropped a result for a folder
    /// that changed meanwhile; the blocking host measures in place. The
    /// folder's drive is checked with it.
    pub fn measure_folder_usage(&mut self, services: &Services) {
        self.folder_usage = match services.folder_usage.measure(&self.audio_folder) {
            Ok(bytes) => FolderUsageState::Bytes(bytes),
            Err(_) => FolderUsageState::Unavailable,
        };
        self.folder_may_lose_recent_writes = services
            .folder_usage
            .may_lose_recent_writes(&self.audio_folder);
    }

    /// Saves the rule; Forever also keeps every recording still on disk and
    /// reports how many in `kept_forever`. Nothing is ever deleted here.
    pub fn set_retention(
        &mut self,
        mode: RetentionMode,
        days: i64,
        store: &Store,
        services: &Services,
    ) {
        self.retention_mode = mode;
        self.retention_days = days.clamp(*DAY_RANGE.start(), *DAY_RANGE.end());
        let retention = self.retention();
        self.save(store, move |settings| {
            settings.default_retention = retention;
        });
        if mode != RetentionMode::KeepForever || self.errors.error.is_some() {
            return;
        }
        match services.pipeline.keep_all_recordings() {
            Ok(count) => self.kept_forever = Some(count),
            Err(error) => self
                .errors
                .fail("Recordings could not be marked as kept.", error),
        }
    }

    pub fn refresh_permissions(&mut self, services: &Services) {
        self.permissions = RECORDING_PERMISSIONS
            .iter()
            .map(|kind| (*kind, services.permissions.state(*kind)))
            .collect();
    }

    #[must_use]
    pub fn state_of(&self, kind: PermissionKind) -> PermissionState {
        self.permissions
            .get(&kind)
            .copied()
            .unwrap_or(PermissionState::Unknown)
    }

    /// Marks the step as requesting; false while another request is up.
    /// The host runs the prompt outside its lock and calls
    /// [`Self::finish_request`].
    pub fn begin_request(&mut self, kind: PermissionKind) -> bool {
        if self.requesting.is_some() {
            return false;
        }
        self.requesting = Some(kind);
        true
    }

    pub fn finish_request(&mut self, kind: PermissionKind, state: PermissionState) {
        self.permissions.insert(kind, state);
        self.requesting = None;
    }

    fn save(&mut self, store: &Store, mutate: impl FnOnce(&mut steno_core::Settings)) {
        match update_settings(store, mutate) {
            Ok(_) => self.errors.clear(),
            Err(error) => self.errors.fail("The setting could not be saved.", error),
        }
    }
}

impl Default for AudioSettingsViewModel {
    fn default() -> Self {
        Self::new()
    }
}
