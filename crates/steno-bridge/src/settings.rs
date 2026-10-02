//! One snapshot per Settings section, after
//! `Sources/StenoBridge/SettingsSnapshots.swift`. Each carries the section's
//! subtitle for the Settings sidebar so the page never derives status text.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::envelope::{BridgeTopic, PermissionKind, PermissionState};
use crate::snapshots::Snapshot;
use steno_core::string_enum;

// settings.general

string_enum! {
    /// Swift: `GeneralSettingsSnapshot.LoginItem`.
    pub enum GeneralLoginItem {
        NotRegistered = "notRegistered",
        Enabled = "enabled",
        RequiresApproval = "requiresApproval",
        NotFound = "notFound",
    }
}

string_enum! {
    /// Swift: `GeneralSettingsSnapshot.Updates.Outcome`.
    pub enum GeneralUpdatesOutcome {
        NotChecked = "notChecked",
        UpToDate = "upToDate",
        Available = "available",
        Failed = "failed",
    }
}

/// Swift: `GeneralSettingsSnapshot.Updates`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GeneralUpdates {
    pub can_check: bool,
    pub automatically_checks: bool,
    pub automatically_downloads: bool,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "steno_core::json::iso_time_opt"
    )]
    pub last_check_at: Option<DateTime<Utc>>,
    pub outcome: GeneralUpdatesOutcome,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// A summary template with the sentence shown under the picker.
/// Swift: `GeneralSettingsSnapshot.Template`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GeneralTemplate {
    pub id: String,
    pub name: String,
    pub description: String,
}

string_enum! {
    /// Swift: `GeneralSettingsSnapshot.Acknowledgement.Group`.
    pub enum GeneralAcknowledgementGroup {
        SpeechModels = "speechModels",
        Libraries = "libraries",
    }
}

/// One line of the Acknowledgements dialog. Swift:
/// `GeneralSettingsSnapshot.Acknowledgement`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GeneralAcknowledgement {
    pub group: GeneralAcknowledgementGroup,
    pub name: String,
    pub licence: String,
    pub source: String,
}

/// Swift: `GeneralSettingsSnapshot`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GeneralSettingsSnapshot {
    pub subtitle: String,
    pub version: String,
    pub login_item: GeneralLoginItem,
    pub detection_enabled: bool,
    #[serde(rename = "defaultTemplateID")]
    pub default_template_id: String,
    pub templates: Vec<GeneralTemplate>,
    pub calendar_permission: PermissionState,
    pub requesting_calendar: bool,
    pub updates: GeneralUpdates,
    pub acknowledgements: Vec<GeneralAcknowledgement>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_details: Option<String>,
}

impl Snapshot for GeneralSettingsSnapshot {
    const TOPIC: BridgeTopic = BridgeTopic::SettingsGeneral;
}

// settings.recording

/// Swift: `RecordingSettingsSnapshot.Device`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecordingDevice {
    pub uid: String,
    pub name: String,
}

string_enum! {
    /// Swift: `RecordingSettingsSnapshot.Retention.Mode`, an alias of
    /// `StenoCore.AudioRetention.Kind`; hence the core name, no topic word.
    pub enum RetentionMode {
        DeleteAfterProcessing = "deleteAfterProcessing",
        KeepDays = "keepDays",
        KeepForever = "keepForever",
    }
}

/// Swift: `RecordingSettingsSnapshot.Retention`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecordingRetention {
    pub mode: RetentionMode,
    pub days: i64,
}

/// Swift: `RecordingSettingsSnapshot.Permission`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordingPermission {
    pub kind: PermissionKind,
    pub state: PermissionState,
    pub is_requesting: bool,
}

string_enum! {
    /// Whether `folder_usage_bytes` is a figure, still being measured, or
    /// could not be measured. Swift: `RecordingSettingsSnapshot.FolderUsage`.
    pub enum RecordingFolderUsage {
        Measuring = "measuring",
        Measured = "measured",
        Unavailable = "unavailable",
    }
}

/// Swift: `RecordingSettingsSnapshot`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordingSettingsSnapshot {
    pub subtitle: String,
    pub devices: Vec<RecordingDevice>,
    /// Absent means the system default input.
    #[serde(
        rename = "inputDeviceUID",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub input_device_uid: Option<String>,
    pub audio_folder_path: String,
    pub audio_folder_name: String,
    pub folder_usage: RecordingFolderUsage,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub folder_usage_bytes: Option<i64>,
    pub retention: RecordingRetention,
    /// The sentence under the retention picker for the current rule.
    pub retention_footnote: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kept_forever_count: Option<i64>,
    pub permissions: Vec<RecordingPermission>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_details: Option<String>,
}

impl Snapshot for RecordingSettingsSnapshot {
    const TOPIC: BridgeTopic = BridgeTopic::SettingsRecording;
}

// settings.transcription

/// Swift: `TranscriptionSettingsSnapshot.Engine`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TranscriptionEngine {
    pub id: String,
    pub name: String,
}

string_enum! {
    /// Swift: `TranscriptionSettingsSnapshot.Asset.State`.
    pub enum TranscriptionAssetState {
        Absent = "absent",
        Downloading = "downloading",
        Installed = "installed",
        Failed = "failed",
    }
}

/// Swift: `TranscriptionSettingsSnapshot.Asset`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TranscriptionAsset {
    pub id: String,
    pub name: String,
    pub detail: String,
    pub state: TranscriptionAssetState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub download_fraction: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub download_phase: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub installed_bytes: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure: Option<String>,
}

/// Swift: `TranscriptionSettingsSnapshot`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TranscriptionSettingsSnapshot {
    pub subtitle: String,
    #[serde(rename = "engineID")]
    pub engine_id: String,
    pub engines: Vec<TranscriptionEngine>,
    pub shows_engine_picker: bool,
    pub assets: Vec<TranscriptionAsset>,
    pub all_installed: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_details: Option<String>,
}

impl Snapshot for TranscriptionSettingsSnapshot {
    const TOPIC: BridgeTopic = BridgeTopic::SettingsTranscription;
}

// settings.summaries

/// Swift: `SummariesSettingsSnapshot.Preset`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SummariesPreset {
    pub id: String,
    pub title: String,
    #[serde(rename = "needsAPIKey")]
    pub needs_api_key: bool,
    pub shows_server_field: bool,
    pub model_placeholder: String,
}

/// Swift: `SummariesSettingsSnapshot.TestResult`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SummariesTestResult {
    pub ok: bool,
    pub message: String,
}

string_enum! {
    /// Swift: `SummariesSettingsSnapshot.Codex.SignIn`.
    pub enum SummariesCodexSignIn {
        NotChecked = "notChecked",
        SignedIn = "signedIn",
        Unavailable = "unavailable",
    }
}

/// Swift: `SummariesSettingsSnapshot.Codex.Model`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SummariesCodexModel {
    pub slug: String,
    pub name: String,
}

/// The Codex preset, which summarises through the Codex sign-in on this Mac
/// instead of an API key: what that sign-in looks like, whether the user has
/// confirmed using it, and the model list once confirmed. Set only while
/// that preset is selected. Swift: `SummariesSettingsSnapshot.Codex`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SummariesCodex {
    pub confirmed: bool,
    pub sign_in: SummariesCodexSignIn,
    /// The account line while signed in; the reason while unavailable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sign_in_detail: Option<String>,
    /// The picked model slug; empty until the list arrives or the user picks.
    pub model: String,
    pub models: Vec<SummariesCodexModel>,
    pub is_loading_models: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub models_error: Option<String>,
}

/// Swift: `SummariesSettingsSnapshot`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SummariesSettingsSnapshot {
    pub subtitle: String,
    pub presets: Vec<SummariesPreset>,
    #[serde(rename = "presetID")]
    pub preset_id: String,
    #[serde(rename = "baseURL")]
    pub base_url: String,
    pub model: String,
    pub context_tokens: String,
    pub default_context_tokens: i64,
    #[serde(rename = "hasAPIKey")]
    pub has_api_key: bool,
    pub is_configured: bool,
    pub is_testing: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub test_result: Option<SummariesTestResult>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub validation_message: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub codex: Option<SummariesCodex>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_details: Option<String>,
}

impl Snapshot for SummariesSettingsSnapshot {
    const TOPIC: BridgeTopic = BridgeTopic::SettingsSummaries;
}

// settings.export

/// Swift: `ExportSettingsSnapshot`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportSettingsSnapshot {
    pub subtitle: String,
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vault_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vault_name: Option<String>,
    pub people_folder: String,
    pub include_audio: bool,
    pub task_tag: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub validation_message: Option<String>,
    pub saved: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_details: Option<String>,
}

impl Snapshot for ExportSettingsSnapshot {
    const TOPIC: BridgeTopic = BridgeTopic::SettingsExport;
}

// settings.iphone

/// Swift: `PhoneSettingsSnapshot.Device`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PhoneDevice {
    #[serde(with = "steno_core::json::uuid_text")]
    pub id: Uuid,
    pub name: String,
    #[serde(with = "steno_core::json::iso_time")]
    pub paired_at: DateTime<Utc>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "steno_core::json::iso_time_opt"
    )]
    pub last_seen_at: Option<DateTime<Utc>>,
}

string_enum! {
    /// Swift: `PhoneSettingsSnapshot.Listener.State`.
    pub enum PhoneListenerState {
        Unavailable = "unavailable",
        Stopped = "stopped",
        Starting = "starting",
        Listening = "listening",
        Failed = "failed",
    }
}

/// Swift: `PhoneSettingsSnapshot.Listener`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PhoneListener {
    pub state: PhoneListenerState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure: Option<String>,
}

/// Swift: `PhoneSettingsSnapshot.Pairing`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PhonePairing {
    #[serde(with = "steno_core::json::iso_time")]
    pub expires_at: DateTime<Utc>,
    /// The QR code as a base64 PNG; the page draws it in an `img`.
    #[serde(rename = "qrPNGBase64")]
    pub qr_png_base64: String,
}

/// Swift: `PhoneSettingsSnapshot.Receipt`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PhoneReceipt {
    #[serde(rename = "deviceID", with = "steno_core::json::uuid_text")]
    pub device_id: Uuid,
    #[serde(rename = "recordingID", with = "steno_core::json::uuid_text")]
    pub recording_id: Uuid,
    pub received_bytes: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_bytes: Option<i64>,
}

/// Swift: `PhoneSettingsSnapshot`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PhoneSettingsSnapshot {
    pub subtitle: String,
    #[serde(rename = "macID", default, skip_serializing_if = "Option::is_none")]
    pub mac_id: Option<String>,
    pub devices: Vec<PhoneDevice>,
    pub listener: PhoneListener,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pairing: Option<PhonePairing>,
    pub receipts: Vec<PhoneReceipt>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_details: Option<String>,
}

impl Snapshot for PhoneSettingsSnapshot {
    const TOPIC: BridgeTopic = BridgeTopic::SettingsPhone;
}
