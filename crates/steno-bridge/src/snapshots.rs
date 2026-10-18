//! The snapshots the host publishes per topic; the type mapping is in the
//! crate doc.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use steno_core::json::{iso_time, iso_time_opt, uuid_text, uuid_text_opt};
use steno_core::string_enum;
use uuid::Uuid;

use crate::envelope::{
    BridgeTopic, CaptureMode, MeetingSource, MeetingState, PermissionKind, PermissionState,
    SettingsSection,
};
use crate::settings::SummariesSettingsSnapshot;

/// A payload the host publishes on a fixed topic; `EventSinkExt::publish`
/// derives the topic from the type.
pub trait Snapshot: Serialize {
    const TOPIC: BridgeTopic;
}

// app

/// Window-wide state: what the main window needs before any meeting loads.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppSnapshot {
    pub version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub setup_banner: Option<AppSetupBanner>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phone: Option<AppPhone>,
    /// Deep links: set once, consumed by the page, then cleared by the host.
    #[serde(
        rename = "requestedMeetingID",
        default,
        skip_serializing_if = "Option::is_none",
        with = "uuid_text_opt"
    )]
    pub requested_meeting_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requested_settings_section: Option<SettingsSection>,
}

impl Snapshot for AppSnapshot {
    const TOPIC: BridgeTopic = BridgeTopic::App;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppSetupBanner {
    pub title: String,
    pub body: String,
    pub offers_summaries: bool,
    pub offers_vault: bool,
    /// A one-time notice in the banner's place, which the page dismisses
    /// with OK rather than "Not now"; left out when false.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub is_notice: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppPhone {
    pub name: String,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "iso_time_opt"
    )]
    pub last_sync_at: Option<DateTime<Utc>>,
    pub is_reachable: bool,
}

// recording

string_enum! {
    pub enum RecordingState {
        Idle = "idle",
        Starting = "starting",
        Recording = "recording",
        Stopping = "stopping",
    }
}

/// RMS in 0...1 per lane; the page draws the meter, at most 20 Hz.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct RecordingLevel {
    pub mic: f64,
    pub system: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordingAutoStop {
    pub remaining_seconds: f64,
    pub total_seconds: f64,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordingSnapshot {
    pub state: RecordingState,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "iso_time_opt"
    )]
    pub started_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<CaptureMode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub call_app: Option<String>,
    #[serde(
        rename = "meetingID",
        default,
        skip_serializing_if = "Option::is_none",
        with = "uuid_text_opt"
    )]
    pub meeting_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub level: Option<RecordingLevel>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_stop: Option<RecordingAutoStop>,
    pub denied_permissions: Vec<PermissionKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub warning: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl Snapshot for RecordingSnapshot {
    const TOPIC: BridgeTopic = BridgeTopic::Recording;
}

// progress

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProgressEntry {
    #[serde(rename = "meetingID", with = "uuid_text")]
    pub meeting_id: Uuid,
    pub stage: String,
    pub title: String,
    pub fraction: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub estimated_remaining_seconds: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProgressSnapshot {
    pub entries: Vec<ProgressEntry>,
}

impl Snapshot for ProgressSnapshot {
    const TOPIC: BridgeTopic = BridgeTopic::Progress;
}

// meetings.list

string_enum! {
    pub enum ListFilter {
        All = "all",
        Processing = "processing",
        Ready = "ready",
        Failed = "failed",
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SpeakerChip {
    #[serde(with = "uuid_text")]
    pub id: Uuid,
    pub initial: String,
    /// Index into the page's fixed people palette; stable per person.
    pub color_index: i64,
    pub is_confirmed: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MeetingRow {
    #[serde(with = "uuid_text")]
    pub id: Uuid,
    pub title: String,
    #[serde(with = "iso_time")]
    pub started_at: DateTime<Utc>,
    pub duration_seconds: f64,
    pub source: MeetingSource,
    pub state: MeetingState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preview: Option<String>,
    pub has_summary: bool,
    pub speakers: Vec<SpeakerChip>,
    pub tags: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListCounts {
    pub all: i64,
    pub processing: i64,
    pub ready: i64,
    pub failed: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListTag {
    pub name: String,
    pub count: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ListDayGroup {
    /// Calendar day in the user's zone, `YYYY-MM-DD`; the page formats labels.
    pub day: String,
    pub meetings: Vec<MeetingRow>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MeetingsListSnapshot {
    pub filter: ListFilter,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tag_filter: Option<String>,
    pub query: String,
    pub counts: ListCounts,
    pub tags: Vec<ListTag>,
    pub groups: Vec<ListDayGroup>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "uuid_text_opt"
    )]
    pub selection: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl Snapshot for MeetingsListSnapshot {
    const TOPIC: BridgeTopic = BridgeTopic::MeetingsList;
}

// meeting.detail

string_enum! {
    pub enum DetailTab {
        Summary = "summary",
        Transcript = "transcript",
        Tasks = "tasks",
        Notes = "notes",
    }
}

string_enum! {
    pub enum DetailRetentionKind {
        Deleted = "deleted",
        DeletesOn = "deletesOn",
        KeptUntilExportSucceeds = "keptUntilExportSucceeds",
        KeptProcessingFailed = "keptProcessingFailed",
        KeptIncomplete = "keptIncomplete",
        KeptWhileProcessing = "keptWhileProcessing",
        KeptForever = "keptForever",
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DetailRetention {
    pub kind: DetailRetentionKind,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "iso_time_opt"
    )]
    pub deletes_at: Option<DateTime<Utc>>,
    pub keeps_audio: bool,
    pub shows_keep_toggle: bool,
    pub files_exist: bool,
}

pub use steno_core::SpeakerAssignmentKind;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DetailSpeaker {
    #[serde(with = "uuid_text")]
    pub id: Uuid,
    pub cluster_label: String,
    pub display_name: String,
    pub assignment: SpeakerAssignmentKind,
    #[serde(
        rename = "personID",
        default,
        skip_serializing_if = "Option::is_none",
        with = "uuid_text_opt"
    )]
    pub person_id: Option<Uuid>,
    /// The confirmed or suggested person's email, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suggestion_name: Option<String>,
    pub color_index: i64,
    pub has_clip: bool,
    pub is_playing: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DetailTemplate {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DetailSummaryBullet {
    pub lead: String,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DetailSummarySection {
    pub id: String,
    pub heading: String,
    pub bullets: Vec<DetailSummaryBullet>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DetailTurn {
    #[serde(with = "uuid_text")]
    pub id: Uuid,
    #[serde(
        rename = "speakerID",
        default,
        skip_serializing_if = "Option::is_none",
        with = "uuid_text_opt"
    )]
    pub speaker_id: Option<Uuid>,
    pub speaker_name: String,
    pub start_seconds: f64,
    pub end_seconds: f64,
    pub text: String,
}

pub use steno_core::TaskPriority;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DetailTask {
    #[serde(with = "uuid_text")]
    pub id: Uuid,
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assignee_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assignee_color_index: Option<i64>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "iso_time_opt"
    )]
    pub due_date: Option<DateTime<Utc>>,
    pub priority: TaskPriority,
    pub done: bool,
}

string_enum! {
    pub enum DetailExportStatus {
        NotConfigured = "notConfigured",
        Pending = "pending",
        Delivered = "delivered",
        Failed = "failed",
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DetailExport {
    pub status: DetailExportStatus,
    pub message: String,
    pub can_reexport: bool,
    pub can_reveal: bool,
}

string_enum! {
    pub enum DetailSummaryStatusKind {
        Pending = "pending",
        Present = "present",
        SkippedUnconfigured = "skippedUnconfigured",
        SkippedRunnable = "skippedRunnable",
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DetailSummaryStatus {
    pub kind: DetailSummaryStatusKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action_title: Option<String>,
}

/// The topic's payload is this or `null` when no meeting is selected.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MeetingDetailSnapshot {
    #[serde(with = "uuid_text")]
    pub id: Uuid,
    pub title: String,
    #[serde(with = "iso_time")]
    pub started_at: DateTime<Utc>,
    pub duration_seconds: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    pub source: MeetingSource,
    pub state: MeetingState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_reason: Option<String>,
    /// The warning that parts of the recording could not be decoded and
    /// were replaced by silence.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio_warning: Option<String>,
    pub tags: Vec<String>,
    pub tab: DetailTab,
    pub retention: DetailRetention,
    pub speakers: Vec<DetailSpeaker>,
    pub templates: Vec<DetailTemplate>,
    #[serde(rename = "templateID")]
    pub template_id: String,
    pub summary_status: DetailSummaryStatus,
    pub summary: Vec<DetailSummarySection>,
    pub transcript: Vec<DetailTurn>,
    pub tasks: Vec<DetailTask>,
    pub decisions: Vec<String>,
    pub notes: String,
    pub export: DetailExport,
    /// Whether the page shows "Process again".
    pub can_process_again: bool,
    pub can_rerun_summary: bool,
    pub is_busy: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl Snapshot for MeetingDetailSnapshot {
    const TOPIC: BridgeTopic = BridgeTopic::MeetingDetail;
}

/// No selection publishes `null` on `meeting.detail`.
impl Snapshot for Option<MeetingDetailSnapshot> {
    const TOPIC: BridgeTopic = BridgeTopic::MeetingDetail;
}

// onboarding

string_enum! {
    pub enum OnboardingPage {
        /// Before page 1, on the Mac's first launch after the update from
        /// the Swift app while its handover identity is not brought over.
        Import = "import",
        Permissions = "permissions",
        Setup = "setup",
    }
}

string_enum! {
    /// Where the import step stands.
    pub enum OnboardingImportState {
        /// Not run yet: the step explains the prompts and offers Continue.
        Pending = "pending",
        /// The keychain prompts are up.
        Importing = "importing",
        /// The export was denied, failed or skipped: phone handover waits.
        Waiting = "waiting",
        /// The identity is in place; the step is over.
        Done = "done",
    }
}

/// The import step: what the new app brings over from the Swift app's
/// keychain items, behind one login-password prompt each.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OnboardingImport {
    pub state: OnboardingImportState,
    /// How many login-password prompts the step may bring up.
    pub prompts: i64,
    /// Why the last export did not bring the identity over, in a sentence.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[allow(
    clippy::struct_excessive_bools,
    reason = "the wire format's flags, one key each, as the contract has them"
)]
pub struct OnboardingPermissionStep {
    pub kind: PermissionKind,
    pub state: PermissionState,
    /// Required steps gate page 1's Done; optional ones can be skipped.
    pub is_required: bool,
    pub is_requesting: bool,
    pub is_skipped: bool,
    /// As the Recording section's
    /// [`waits_for_recording`](crate::RecordingPermission::waits_for_recording).
    /// Left out when false.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub waits_for_recording: bool,
}

string_enum! {
    pub enum OnboardingSetupStepKind {
        Summaries = "summaries",
        Vault = "vault",
    }
}

string_enum! {
    pub enum OnboardingSetupStepState {
        Open = "open",
        Saved = "saved",
        Skipped = "skipped",
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OnboardingSetupStep {
    pub kind: OnboardingSetupStepKind,
    pub state: OnboardingSetupStepState,
    /// The collapsed row's line once saved (`Saved: <model> at <host>`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub saved_line: Option<String>,
}

/// The Obsidian vault row: the chosen folder (absent until chosen), why the
/// last save was refused, and the store's error if saving failed.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OnboardingVault {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub validation_message: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_details: Option<String>,
}

/// Page 1's speech models row, shown while the models a meeting needs were
/// missing when onboarding opened, and kept, as installed, once they
/// arrive. Download runs Settings' download once per id in `asset_ids`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OnboardingSpeechModels {
    pub state: crate::TranscriptionAssetState,
    /// The row's status line: "Not downloaded · 33 MB", "Downloading… 40%",
    /// "Installed".
    pub detail: String,
    /// The models Download or Retry starts; empty while downloading or done.
    #[serde(rename = "assetIDs")]
    pub asset_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub download_fraction: Option<f64>,
    /// Why the last download did not finish.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure: Option<String>,
}

string_enum! {
    /// Whether Steno may write to the stored vault.
    pub enum OnboardingVaultAccessState {
        /// The check runs; macOS may be asking for the folder.
        Checking = "checking",
        Writable = "writable",
        /// The folder refused the write, most often because macOS's Files
        /// and Folders setting is off for Steno.
        Refused = "refused",
        /// The vault cannot be used for a reason the system setting does
        /// not fix: the folder is gone or the check could not run.
        Unusable = "unusable",
    }
}

/// Page 1's vault row on the Mac: the stored vault is written to once here,
/// so a Files and Folders prompt comes up now rather than at the first
/// export. Absent without a stored vault, and off the Mac.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OnboardingVaultAccess {
    pub state: OnboardingVaultAccessState,
    /// The vault folder's name.
    pub name: String,
    /// Why the vault was refused or cannot be used, in the destination's
    /// words, as [`OnboardingVault::validation_message`] on page 2.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub validation_message: Option<String>,
}

/// The onboarding window: page 1's permission steps and page 2's two setup
/// rows.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OnboardingSnapshot {
    pub page: OnboardingPage,
    pub permissions: Vec<OnboardingPermissionStep>,
    /// Every required permission granted: page 1 offers Continue instead of Later.
    pub permissions_complete: bool,
    pub setup: Vec<OnboardingSetupStep>,
    pub can_save_summaries: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summaries: Option<SummariesSettingsSnapshot>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vault: Option<OnboardingVault>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retention_sentence: Option<String>,
    /// The import step, present while [`OnboardingPage::Import`] is or
    /// was current.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub swift_import: Option<OnboardingImport>,
    /// Page 1's speech models row.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub speech_models: Option<OnboardingSpeechModels>,
    /// Page 1's vault row.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vault_access: Option<OnboardingVaultAccess>,
    /// Set once onboarding is over; the page closes the window.
    pub finished: bool,
}

impl Snapshot for OnboardingSnapshot {
    const TOPIC: BridgeTopic = BridgeTopic::Onboarding;
}
