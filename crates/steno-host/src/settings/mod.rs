//! The Settings window's six section view models, the sidebar overview and
//! their snapshots, after `apps/macos/Steno/Settings/` and
//! `Web/SettingsSnapshots.swift`.

pub mod audio;
pub mod general;
pub mod llm;
pub mod obsidian;
pub mod overview;
pub mod phones;
pub mod snapshots;
pub mod transcription;

use steno_core::{Settings, Store, StoreError};

pub use audio::{AudioSettingsViewModel, FolderUsageState};
pub use general::GeneralSettingsViewModel;
pub use llm::{LlmPreset, LlmSettingsViewModel};
pub use obsidian::ObsidianSettingsViewModel;
pub use phones::PhonesSettingsViewModel;
pub use transcription::{AssetState, SpeechSettingsViewModel};

/// The error pair every section renders: a plain sentence in `error` with
/// the original text in `details`. Swift: `SettingsSectionModel`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SectionError {
    pub error: Option<String>,
    pub details: Option<String>,
}

impl SectionError {
    pub fn fail(&mut self, message: &str, error: impl std::fmt::Display) {
        self.error = Some(message.to_owned());
        self.details = Some(error.to_string());
    }

    pub fn clear(&mut self) {
        self.error = None;
        self.details = None;
    }
}

/// Load, mutate, save: every settings edit goes through here.
/// Swift: `AppEnvironment.updateSettings`.
pub fn update_settings(
    store: &Store,
    mutate: impl FnOnce(&mut Settings),
) -> Result<Settings, StoreError> {
    let mut settings = store.settings()?;
    mutate(&mut settings);
    store.save_settings(&settings)?;
    Ok(settings)
}
