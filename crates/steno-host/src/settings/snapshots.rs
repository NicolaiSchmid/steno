//! The Settings window's topics as pure mappings from the six section view
//! models to the contract snapshots, after `Web/SettingsSnapshots.swift`.

use steno_bridge::{
    ExportSettingsSnapshot, GeneralAcknowledgement, GeneralAcknowledgementGroup, GeneralLoginItem,
    GeneralSettingsSnapshot, GeneralTemplate, GeneralUpdates, GeneralUpdatesOutcome, PhoneDevice,
    PhoneListener, PhoneListenerState, PhonePairing, PhoneReceipt, PhoneSettingsSnapshot, Platform,
    RecordingDevice, RecordingFolderUsage, RecordingPermission, RecordingRetention,
    RecordingSettingsSnapshot, SummariesCodex, SummariesCodexModel, SummariesCodexSignIn,
    SummariesKeyStore, SummariesPreset, SummariesSettingsSnapshot, SummariesTestResult,
    TranscriptionAsset, TranscriptionAssetState, TranscriptionEngine,
    TranscriptionSettingsSnapshot,
};
use steno_core::{HandoverReceipt, SecretPlace, SummaryTemplate};

use super::audio::{AudioSettingsViewModel, FolderUsageState, recording_permissions};
use super::general::GeneralSettingsViewModel;
use super::llm::{CodexStatus, LlmPreset, LlmSettingsViewModel, TestResult};
use super::obsidian::ObsidianSettingsViewModel;
use super::phones::PhonesSettingsViewModel;
use super::transcription::{AssetState, SpeechSettingsViewModel};
use crate::services::{ListenerState, LoginItemStatus, Services, SpeechModels, UpdateOutcome};
use crate::speech::ModelAsset;

fn login_item(status: LoginItemStatus) -> GeneralLoginItem {
    match status {
        LoginItemStatus::NotRegistered => GeneralLoginItem::NotRegistered,
        LoginItemStatus::Enabled => GeneralLoginItem::Enabled,
        LoginItemStatus::RequiresApproval => GeneralLoginItem::RequiresApproval,
        LoginItemStatus::NotFound => GeneralLoginItem::NotFound,
    }
}

/// The libraries Steno ships with, for the Acknowledgements dialog; the
/// Rust port's own list arrives with the Linux release (WP8).
pub fn libraries() -> Vec<GeneralAcknowledgement> {
    [
        (
            "Sparkle",
            "MIT",
            "https://github.com/sparkle-project/Sparkle",
        ),
        ("GRDB.swift", "MIT", "https://github.com/groue/GRDB.swift"),
        (
            "FluidAudio",
            "Apache-2.0",
            "https://github.com/FluidInference/FluidAudio",
        ),
        (
            "WhisperKit",
            "MIT",
            "https://github.com/argmaxinc/WhisperKit",
        ),
        (
            "SwiftNIO",
            "Apache-2.0",
            "https://github.com/apple/swift-nio",
        ),
        (
            "Swift Crypto",
            "Apache-2.0",
            "https://github.com/apple/swift-crypto",
        ),
        ("Speex", "BSD", "https://github.com/sbooth/CSpeex"),
    ]
    .into_iter()
    .map(|(name, licence, source)| GeneralAcknowledgement {
        group: GeneralAcknowledgementGroup::Libraries,
        name: name.to_owned(),
        licence: licence.to_owned(),
        source: source.to_owned(),
    })
    .collect()
}

/// Every speech model, named by the model store that holds it, then the
/// libraries.
pub fn acknowledgements(models: &dyn SpeechModels) -> Vec<GeneralAcknowledgement> {
    ModelAsset::ALL
        .iter()
        .map(|asset| GeneralAcknowledgement {
            group: GeneralAcknowledgementGroup::SpeechModels,
            name: models.display_name(*asset).to_owned(),
            licence: asset.licence().to_owned(),
            source: models.source_repo(*asset).to_owned(),
        })
        .chain(libraries())
        .collect()
}

/// Bytes received so far: whole chunks, never past the declared size.
#[must_use]
pub fn received_bytes(receipt: &HandoverReceipt) -> i64 {
    let received = i64::try_from(receipt.received_chunks.len())
        .unwrap_or(i64::MAX)
        .saturating_mul(receipt.chunk_size);
    received.min(receipt.byte_count)
}

#[must_use]
pub fn general(
    general: &GeneralSettingsViewModel,
    services: &Services,
    subtitle: &str,
    version: &str,
) -> GeneralSettingsSnapshot {
    let (outcome, detail) = match services.updater.last_outcome() {
        UpdateOutcome::NotChecked => (GeneralUpdatesOutcome::NotChecked, None),
        UpdateOutcome::UpToDate => (GeneralUpdatesOutcome::UpToDate, None),
        UpdateOutcome::Available(version) => (GeneralUpdatesOutcome::Available, Some(version)),
        UpdateOutcome::Failed(message) => (GeneralUpdatesOutcome::Failed, Some(message)),
    };
    GeneralSettingsSnapshot {
        subtitle: subtitle.to_owned(),
        version: version.to_owned(),
        login_item: login_item(general.login_item),
        detection_enabled: general.detection_enabled,
        default_template_id: general.default_template_id.clone(),
        templates: SummaryTemplate::bundled()
            .iter()
            .map(|template| GeneralTemplate {
                id: template.id.clone(),
                name: template.display_name.clone(),
                description: template.description.clone(),
            })
            .collect(),
        calendar_permission: general.calendar_permission,
        requesting_calendar: general.requesting_calendar,
        updates: GeneralUpdates {
            can_check: services.updater.can_check_for_updates(),
            automatically_checks: services.updater.automatically_checks(),
            automatically_downloads: services.updater.automatically_downloads(),
            last_check_at: services.updater.last_check_at(),
            outcome,
            detail,
        },
        acknowledgements: acknowledgements(services.speech_models.as_ref()),
        error: general.errors.error.clone(),
        error_details: general.errors.details.clone(),
    }
}

/// The picker's name for a chosen microphone the device list lacks.
pub const DISCONNECTED_INPUT: &str = "Microphone not connected";

/// The picker's devices: the list, then the chosen device when the list
/// lacks it (unplugged, or a UID saved on another computer), named
/// [`DISCONNECTED_INPUT`], so the picker shows it as not connected rather
/// than as a bare UID; a recording then records the default input on every
/// platform (see `CaptureBackend::start` in `steno-audio`). After a failed
/// list nothing is known to be connected, so no entry is added. Rust only:
/// the Swift picker shows no entry for it.
fn recording_devices(audio: &AudioSettingsViewModel) -> Vec<RecordingDevice> {
    let mut devices: Vec<RecordingDevice> = audio
        .devices
        .iter()
        .map(|device| RecordingDevice {
            uid: device.uid.clone(),
            name: device.name.clone(),
        })
        .collect();
    if let Some(uid) = &audio.input_device_uid
        && !audio.devices_failed
        && !devices.iter().any(|device| &device.uid == uid)
    {
        devices.push(RecordingDevice {
            uid: uid.clone(),
            name: DISCONNECTED_INPUT.to_owned(),
        });
    }
    devices
}

/// The warning under an audio folder on a drive that may lose recent
/// recordings in a power cut.
pub const AUDIO_FOLDER_WARNING: &str = "This drive may lose recent recordings in a power cut.";

#[must_use]
pub fn recording(
    audio: &AudioSettingsViewModel,
    subtitle: &str,
    platform: Platform,
) -> RecordingSettingsSnapshot {
    let (usage, bytes) = match audio.folder_usage {
        FolderUsageState::Measuring => (RecordingFolderUsage::Measuring, None),
        FolderUsageState::Bytes(measured) => (RecordingFolderUsage::Measured, Some(measured)),
        FolderUsageState::Unavailable => (RecordingFolderUsage::Unavailable, None),
    };
    RecordingSettingsSnapshot {
        subtitle: subtitle.to_owned(),
        devices: recording_devices(audio),
        input_device_uid: audio.input_device_uid.clone(),
        audio_folder_path: audio.audio_folder.to_string_lossy().into_owned(),
        audio_folder_name: audio.folder_name(),
        audio_folder_warning: audio
            .folder_may_lose_recent_writes
            .then(|| AUDIO_FOLDER_WARNING.to_owned()),
        folder_usage: usage,
        folder_usage_bytes: bytes,
        retention: RecordingRetention {
            mode: audio.retention_mode,
            days: audio.retention_days,
        },
        retention_footnote: audio.footnote(),
        kept_forever_count: audio.kept_forever,
        permissions: recording_permissions(platform)
            .map(|kind| RecordingPermission {
                kind,
                state: audio.state_of(kind),
                is_requesting: audio.requesting == Some(kind),
            })
            .collect(),
        error: audio.errors.error.clone(),
        error_details: audio.errors.details.clone(),
    }
}

/// The Transcription section; `models` says what size each asset is
/// expected to have.
#[must_use]
pub fn transcription(
    speech: &SpeechSettingsViewModel,
    models: &dyn SpeechModels,
    subtitle: &str,
) -> TranscriptionSettingsSnapshot {
    TranscriptionSettingsSnapshot {
        subtitle: subtitle.to_owned(),
        engine_id: speech.engine_id.as_str().to_owned(),
        engines: SpeechSettingsViewModel::engines()
            .iter()
            .map(|engine| TranscriptionEngine {
                id: engine.as_str().to_owned(),
                name: SpeechSettingsViewModel::engine_title(*engine),
            })
            .collect(),
        shows_engine_picker: SpeechSettingsViewModel::shows_engine_picker(),
        assets: speech
            .assets()
            .iter()
            .map(|asset| {
                let mut row = TranscriptionAsset {
                    id: asset.as_str().to_owned(),
                    name: SpeechSettingsViewModel::component_title(*asset).to_owned(),
                    detail: speech.status_text(*asset, models),
                    state: TranscriptionAssetState::Absent,
                    download_fraction: None,
                    download_phase: None,
                    installed_bytes: None,
                    failure: None,
                };
                match speech.state_of(*asset) {
                    AssetState::Absent => {}
                    AssetState::Downloading { fraction, phase } => {
                        row.state = TranscriptionAssetState::Downloading;
                        row.download_fraction = Some(fraction);
                        row.download_phase = Some(phase);
                    }
                    AssetState::Installed { bytes } => {
                        row.state = TranscriptionAssetState::Installed;
                        row.installed_bytes =
                            Some(bytes.unwrap_or_else(|| models.expected_bytes(*asset)));
                    }
                    AssetState::Failed(message) => {
                        row.state = TranscriptionAssetState::Failed;
                        row.failure = Some(message);
                    }
                }
                row
            })
            .collect(),
        all_installed: speech.all_installed(),
        error: speech.errors.error.clone(),
        error_details: speech.errors.details.clone(),
    }
}

#[must_use]
pub fn summaries(
    llm: &LlmSettingsViewModel,
    subtitle: &str,
    platform: Platform,
) -> SummariesSettingsSnapshot {
    let codex = (llm.preset == LlmPreset::Codex).then(|| {
        let (sign_in, detail) = match &llm.codex_status {
            CodexStatus::NotChecked => (SummariesCodexSignIn::NotChecked, None),
            CodexStatus::SignedIn(account) => {
                (SummariesCodexSignIn::SignedIn, Some(account.clone()))
            }
            CodexStatus::Unavailable(reason) => {
                (SummariesCodexSignIn::Unavailable, Some(reason.clone()))
            }
        };
        SummariesCodex {
            confirmed: llm.codex_confirmed,
            sign_in,
            sign_in_detail: detail,
            model: llm.codex_model.clone(),
            models: llm
                .codex_model_choices()
                .into_iter()
                .map(|model| SummariesCodexModel {
                    slug: model.slug,
                    name: model.display_name,
                })
                .collect(),
            is_loading_models: llm.is_loading_codex_models,
            models_error: llm.codex_models_error.clone(),
        }
    });
    SummariesSettingsSnapshot {
        subtitle: subtitle.to_owned(),
        presets: LlmPreset::ALL
            .iter()
            .map(|preset| SummariesPreset {
                id: preset.as_str().to_owned(),
                title: preset.title(platform).to_owned(),
                needs_api_key: preset.needs_api_key(),
                shows_server_field: preset.shows_server_field(),
                model_placeholder: preset.model_placeholder().to_owned(),
            })
            .collect(),
        preset_id: llm.preset.as_str().to_owned(),
        base_url: llm.base_url_text.clone(),
        model: llm.model.clone(),
        context_tokens: llm.context_tokens_text.clone(),
        default_context_tokens: LlmSettingsViewModel::DEFAULT_CONTEXT_TOKENS,
        has_api_key: llm.has_stored_api_key(),
        is_configured: llm.is_configured,
        is_testing: llm.is_testing,
        test_result: llm.test_result.as_ref().map(|result| match result {
            TestResult::Success(message) => SummariesTestResult {
                ok: true,
                message: message.clone(),
            },
            TestResult::Failure(message) => SummariesTestResult {
                ok: false,
                message: message.clone(),
            },
        }),
        validation_message: llm.validation_message().map(str::to_owned),
        codex,
        error: llm.errors.error.clone(),
        error_details: llm.errors.details.clone(),
        key_store: llm.key_store.map(|place| match place {
            SecretPlace::Keyring => SummariesKeyStore::Keyring,
            SecretPlace::File => SummariesKeyStore::File,
        }),
    }
}

#[must_use]
pub fn export(obsidian: &ObsidianSettingsViewModel, subtitle: &str) -> ExportSettingsSnapshot {
    let vault = obsidian.vault_url();
    ExportSettingsSnapshot {
        subtitle: subtitle.to_owned(),
        enabled: obsidian.enabled,
        vault_path: vault
            .as_ref()
            .map(|path| path.to_string_lossy().into_owned()),
        vault_name: vault.is_some().then(|| obsidian.vault_name()),
        people_folder: obsidian.people_folder.clone(),
        include_audio: obsidian.include_audio,
        task_tag: obsidian.task_tag.clone(),
        validation_message: obsidian.validation_message.clone(),
        saved: obsidian.saved,
        error: obsidian.errors.error.clone(),
        error_details: obsidian.errors.details.clone(),
    }
}

#[must_use]
pub fn phone(
    phones: &PhonesSettingsViewModel,
    services: &Services,
    subtitle: &str,
) -> PhoneSettingsSnapshot {
    let listener = if PhonesSettingsViewModel::is_available(services) {
        match &phones.listener {
            ListenerState::Stopped => PhoneListener {
                state: PhoneListenerState::Stopped,
                port: None,
                failure: None,
            },
            ListenerState::Listening(port) => PhoneListener {
                state: PhoneListenerState::Listening,
                port: Some(i64::from(*port)),
                failure: None,
            },
            ListenerState::Failed(message) => PhoneListener {
                state: PhoneListenerState::Failed,
                port: None,
                failure: Some(message.clone()),
            },
        }
    } else {
        PhoneListener {
            state: PhoneListenerState::Unavailable,
            port: None,
            failure: None,
        }
    };
    let pairing = match (&phones.pairing, &phones.qr_png_base64) {
        (Some(code), Some(png)) => Some(PhonePairing {
            expires_at: code.expires_at,
            qr_png_base64: png.clone(),
        }),
        _ => None,
    };
    PhoneSettingsSnapshot {
        subtitle: subtitle.to_owned(),
        mac_id: PhonesSettingsViewModel::mac_id(services),
        devices: phones
            .devices
            .iter()
            .map(|device| PhoneDevice {
                id: device.id,
                name: device.name.clone(),
                paired_at: device.paired_at,
                last_seen_at: device.last_seen_at,
            })
            .collect(),
        listener,
        pairing,
        receipts: phones
            .active_receipts()
            .into_iter()
            .map(|receipt| PhoneReceipt {
                device_id: receipt.device_id,
                recording_id: receipt.recording_id,
                received_bytes: received_bytes(receipt),
                total_bytes: Some(receipt.byte_count),
            })
            .collect(),
        error: phones.errors.error.clone(),
        error_details: phones.errors.details.clone(),
    }
}
