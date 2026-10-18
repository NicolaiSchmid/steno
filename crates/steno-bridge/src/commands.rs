//! Params of the page's method calls and the reply values: one type per
//! `BridgeMethod` that takes arguments; methods without one take `null`.

use serde::{Deserialize, Serialize};
use steno_core::json::{uuid_text, uuid_text_opt};
use steno_core::string_enum;
use uuid::Uuid;

use crate::envelope::{BridgeWindow, CaptureMode, PermissionKind, SettingsSection};
use crate::settings::RecordingRetention;
use crate::snapshots::{DetailTab, ListFilter, OnboardingSetupStepKind};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PageLayoutParams {
    pub window: BridgeWindow,
    pub width: f64,
    pub height: f64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SetFilterParams {
    pub filter: ListFilter,
}

/// `None` clears the tag filter.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct SetTagFilterParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tag: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SetQueryParams {
    pub query: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct MeetingIdParams {
    #[serde(rename = "meetingID", with = "uuid_text")]
    pub meeting_id: Uuid,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SetTabParams {
    pub tab: DetailTab,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SetTagsParams {
    pub tags: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SetTemplateParams {
    #[serde(rename = "templateID")]
    pub template_id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SetBoolParams {
    pub value: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SetStringParams {
    pub value: String,
}

/// Notes carry their meeting so a save that arrives after the selection
/// moved on lands on the meeting it was typed for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SaveNotesParams {
    #[serde(rename = "meetingID", with = "uuid_text")]
    pub meeting_id: Uuid,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpeakerOptionsParams {
    #[serde(rename = "speakerID", with = "uuid_text")]
    pub speaker_id: Uuid,
    pub query: String,
}

string_enum! {
    pub enum SpeakerOptionKind {
        /// An existing person; `person_id` is set.
        Person = "person",
        /// Create a person named `label`.
        Create = "create",
        /// Mark the speaker as unknown.
        Unknown = "unknown",
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpeakerOption {
    pub kind: SpeakerOptionKind,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(
        rename = "personID",
        default,
        skip_serializing_if = "Option::is_none",
        with = "uuid_text_opt"
    )]
    pub person_id: Option<Uuid>,
}

/// `speakers.options` reply: what the speaker picker offers for one speaker.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpeakerOptionsReply {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prefill: Option<String>,
    pub options: Vec<SpeakerOption>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SelectSpeakerParams {
    #[serde(rename = "speakerID", with = "uuid_text")]
    pub speaker_id: Uuid,
    pub option: SpeakerOption,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpeakerIdParams {
    #[serde(rename = "speakerID", with = "uuid_text")]
    pub speaker_id: Uuid,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct StartRecordingParams {
    pub mode: CaptureMode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SetRetentionParams {
    pub retention: RecordingRetention,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PermissionKindParams {
    pub kind: PermissionKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssetIdParams {
    #[serde(rename = "assetID")]
    pub asset_id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetAutomaticUpdatesParams {
    pub automatically_checks: bool,
    pub automatically_downloads: bool,
}

/// Partial update of the Summaries form; absent fields are left alone.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SummariesUpdateParams {
    #[serde(rename = "baseURL", default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_tokens: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
}

/// Partial update of the Export form; absent fields are left alone.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportUpdateParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub people_folder: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub include_audio: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_tag: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceIdParams {
    #[serde(rename = "deviceID", with = "uuid_text")]
    pub device_id: Uuid,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SetupStepParams {
    pub step: OnboardingSetupStepKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpenUrlParams {
    pub url: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct WindowParams {
    pub window: BridgeWindow,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub section: Option<SettingsSection>,
    #[serde(
        rename = "meetingID",
        default,
        skip_serializing_if = "Option::is_none",
        with = "uuid_text_opt"
    )]
    pub meeting_id: Option<Uuid>,
}

/// `ui.confirmDestructive`: the host shows a native alert; the reply says
/// whether the user confirmed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfirmDestructiveParams {
    pub title: String,
    pub message: String,
    pub confirm_title: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConfirmReply {
    pub confirmed: bool,
}

/// `settings.recording.chooseFolder`, `settings.export.chooseVault` and
/// `onboarding.chooseVault` return the chosen path, or none when the panel
/// was cancelled.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ChosenPathReply {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}
