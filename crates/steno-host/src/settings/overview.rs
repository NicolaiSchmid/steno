//! The sidebar subtitles: one short status per section, computed from the
//! settings, the permissions, the model store, the paired devices and the
//! updater. Swift: `Settings/SettingsOverviewViewModel.swift`.

use std::collections::BTreeMap;
use std::path::Path;

use steno_bridge::{PermissionKind, PermissionState, SettingsSection};
use steno_core::{Settings, Store};

use super::llm::LlmPreset;
use crate::services::{Services, UpdateOutcome};
use crate::setup::llm_configured;
use crate::speech::{ModelAsset, SpeechEngineId};

/// The pure rule behind the subtitles. Swift: `SettingsOverviewViewModel.subtitles`.
#[must_use]
pub fn subtitles(
    settings: Option<&Settings>,
    recording_ready: bool,
    models_installed: bool,
    paired_count: usize,
    handover_available: bool,
    update_outcome: &UpdateOutcome,
    version: &str,
) -> BTreeMap<SettingsSection, String> {
    let mut result = BTreeMap::new();
    result.insert(
        SettingsSection::General,
        match update_outcome {
            UpdateOutcome::Available(available) => format!("Update available: {available}"),
            UpdateOutcome::Failed(_) => "Update check failed".to_owned(),
            UpdateOutcome::UpToDate | UpdateOutcome::NotChecked => format!("Steno {version}"),
        },
    );
    result.insert(
        SettingsSection::Recording,
        if recording_ready {
            "Ready"
        } else {
            "Permission needed"
        }
        .to_owned(),
    );
    result.insert(
        SettingsSection::Transcription,
        if models_installed {
            "Ready"
        } else {
            "Download needed"
        }
        .to_owned(),
    );
    result.insert(
        SettingsSection::Summaries,
        match settings.filter(|settings| llm_configured(settings)) {
            Some(settings) => {
                let preset = LlmPreset::infer_from_settings(settings);
                if preset == LlmPreset::Custom {
                    settings
                        .llm_model
                        .clone()
                        .unwrap_or_else(|| "Custom server".to_owned())
                } else {
                    preset.title().to_owned()
                }
            }
            None => "Not set up".to_owned(),
        },
    );
    result.insert(
        SettingsSection::Export,
        settings
            .and_then(|settings| settings.obsidian.as_ref())
            .map(|obsidian| obsidian.vault_path.as_str())
            .filter(|vault| !vault.is_empty())
            .map_or_else(
                || "Off".to_owned(),
                |vault| {
                    Path::new(vault)
                        .file_name()
                        .map(|name| name.to_string_lossy().into_owned())
                        .unwrap_or_default()
                },
            ),
    );
    result.insert(
        SettingsSection::Phone,
        if handover_available {
            match paired_count {
                0 => "No iPhone paired".to_owned(),
                1 => "1 iPhone paired".to_owned(),
                count => format!("{count} iPhones paired"),
            }
        } else {
            "Unavailable".to_owned()
        },
    );
    result
}

/// The subtitles over the store and the services as they stand now.
/// Swift: `SettingsOverviewViewModel.refresh`.
#[must_use]
pub fn refresh(
    store: &Store,
    services: &Services,
    version: &str,
) -> BTreeMap<SettingsSection, String> {
    let settings = store.settings().ok();
    let granted = |kind| services.permissions.state(kind) == PermissionState::Granted;
    let engine: SpeechEngineId = settings
        .as_ref()
        .and_then(|settings| settings.speech_engine_id.parse().ok())
        .unwrap_or(SpeechEngineId::ParakeetV3);
    let models_installed = services.speech_models.is_installed(engine.asset())
        && services
            .speech_models
            .is_installed(ModelAsset::OfflineDiarizer);
    let paired_count = services
        .handover
        .as_ref()
        .and_then(|handover| handover.paired_devices().ok())
        .map_or(0, |devices| devices.len());
    subtitles(
        settings.as_ref(),
        granted(PermissionKind::Microphone) && granted(PermissionKind::SystemAudio),
        models_installed,
        paired_count,
        services.handover.is_some(),
        &services.updater.last_outcome(),
        version,
    )
}
