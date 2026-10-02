//! The snapshots the host publishes per topic, after
//! `Sources/StenoBridge/Snapshots.swift`; the type mapping is in the crate doc.

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
/// Swift: `AppSnapshot`.
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

/// Swift: `AppSnapshot.SetupBanner`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppSetupBanner {
    pub title: String,
    pub body: String,
    pub offers_summaries: bool,
    pub offers_vault: bool,
}

/// Swift: `AppSnapshot.Phone`.
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
    /// Swift: `RecordingSnapshot.State`.
    pub enum RecordingState {
        Idle = "idle",
        Starting = "starting",
        Recording = "recording",
        Stopping = "stopping",
    }
}

/// RMS in 0...1 per lane; the page draws the meter, at most 20 Hz.
/// Swift: `RecordingSnapshot.Level`.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct RecordingLevel {
    pub mic: f64,
    pub system: f64,
}

/// Swift: `RecordingSnapshot.AutoStop`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordingAutoStop {
    pub remaining_seconds: f64,
    pub total_seconds: f64,
    pub reason: String,
}

/// Swift: `RecordingSnapshot`.
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

/// Swift: `ProgressSnapshot.Entry`.
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

/// Swift: `ProgressSnapshot`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProgressSnapshot {
    pub entries: Vec<ProgressEntry>,
}

impl Snapshot for ProgressSnapshot {
    const TOPIC: BridgeTopic = BridgeTopic::Progress;
}

// meetings.list

string_enum! {
    /// Swift: `BridgeListFilter`.
    pub enum ListFilter {
        All = "all",
        Processing = "processing",
        Ready = "ready",
        Failed = "failed",
    }
}

/// Swift: `SpeakerChip`.
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

/// Swift: `MeetingRow`.
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

/// Swift: `MeetingsListSnapshot.Counts`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListCounts {
    pub all: i64,
    pub processing: i64,
    pub ready: i64,
    pub failed: i64,
}

/// Swift: `MeetingsListSnapshot.Tag`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListTag {
    pub name: String,
    pub count: i64,
}

/// Swift: `MeetingsListSnapshot.DayGroup`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ListDayGroup {
    /// Calendar day in the user's zone, `YYYY-MM-DD`; the page formats labels.
    pub day: String,
    pub meetings: Vec<MeetingRow>,
}

/// Swift: `MeetingsListSnapshot`.
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
    /// Swift: `MeetingDetailSnapshot.Tab`.
    pub enum DetailTab {
        Summary = "summary",
        Transcript = "transcript",
        Tasks = "tasks",
        Notes = "notes",
    }
}

string_enum! {
    /// Swift: `MeetingDetailSnapshot.Retention.Kind`.
    pub enum DetailRetentionKind {
        Deleted = "deleted",
        DeletesOn = "deletesOn",
        KeptUntilExportSucceeds = "keptUntilExportSucceeds",
        KeptProcessingFailed = "keptProcessingFailed",
        KeptWhileProcessing = "keptWhileProcessing",
        KeptForever = "keptForever",
    }
}

/// Swift: `MeetingDetailSnapshot.Retention`.
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

/// Swift: `MeetingDetailSnapshot.Speaker.Assignment`, an alias of
/// `StenoCore.SpeakerAssignment.Kind`; hence the core type itself.
pub use steno_core::SpeakerAssignmentKind;

/// Swift: `MeetingDetailSnapshot.Speaker`.
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

/// Swift: `MeetingDetailSnapshot.Template`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DetailTemplate {
    pub id: String,
    pub name: String,
}

/// Swift: `MeetingDetailSnapshot.SummarySection.Bullet`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DetailSummaryBullet {
    pub lead: String,
    pub text: String,
}

/// Swift: `MeetingDetailSnapshot.SummarySection`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DetailSummarySection {
    pub id: String,
    pub heading: String,
    pub bullets: Vec<DetailSummaryBullet>,
}

/// Swift: `MeetingDetailSnapshot.Turn`.
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

/// Swift: `MeetingDetailSnapshot.Task.Priority`, an alias of
/// `StenoCore.TaskPriority`; hence the core type itself.
pub use steno_core::TaskPriority;

/// Swift: `MeetingDetailSnapshot.Task`.
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
    /// Swift: `MeetingDetailSnapshot.Export.Status`.
    pub enum DetailExportStatus {
        NotConfigured = "notConfigured",
        Pending = "pending",
        Delivered = "delivered",
        Failed = "failed",
    }
}

/// Swift: `MeetingDetailSnapshot.Export`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DetailExport {
    pub status: DetailExportStatus,
    pub message: String,
    pub can_reexport: bool,
    pub can_reveal: bool,
}

string_enum! {
    /// Swift: `MeetingDetailSnapshot.SummaryStatus.Kind`.
    pub enum DetailSummaryStatusKind {
        Pending = "pending",
        Present = "present",
        SkippedUnconfigured = "skippedUnconfigured",
        SkippedRunnable = "skippedRunnable",
    }
}

/// Swift: `MeetingDetailSnapshot.SummaryStatus`.
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

/// Swift: `MeetingDetailSnapshot`. The topic's payload is this or `null`
/// when no meeting is selected.
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
    /// Swift: `OnboardingSnapshot.Page`.
    pub enum OnboardingPage {
        Permissions = "permissions",
        Setup = "setup",
    }
}

/// Swift: `OnboardingSnapshot.PermissionStep`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OnboardingPermissionStep {
    pub kind: PermissionKind,
    pub state: PermissionState,
    /// Required steps gate page 1's Done; optional ones can be skipped.
    pub is_required: bool,
    pub is_requesting: bool,
    pub is_skipped: bool,
}

string_enum! {
    /// Swift: `OnboardingSnapshot.SetupStep.Kind`.
    pub enum OnboardingSetupStepKind {
        Summaries = "summaries",
        Vault = "vault",
    }
}

string_enum! {
    /// Swift: `OnboardingSnapshot.SetupStep.State`.
    pub enum OnboardingSetupStepState {
        Open = "open",
        Saved = "saved",
        Skipped = "skipped",
    }
}

/// Swift: `OnboardingSnapshot.SetupStep`.
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
/// Swift: `OnboardingSnapshot.Vault`.
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

/// The onboarding window: page 1's permission steps and page 2's two setup
/// rows. Swift: `OnboardingSnapshot`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OnboardingSnapshot {
    pub page: OnboardingPage,
    pub permissions: Vec<OnboardingPermissionStep>,
    /// Every required permission granted: page 1 offers Done instead of Later.
    pub permissions_complete: bool,
    pub setup: Vec<OnboardingSetupStep>,
    pub can_save_summaries: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summaries: Option<SummariesSettingsSnapshot>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vault: Option<OnboardingVault>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retention_sentence: Option<String>,
    /// Set once onboarding is over; the page closes the window.
    pub finished: bool,
}

impl Snapshot for OnboardingSnapshot {
    const TOPIC: BridgeTopic = BridgeTopic::Onboarding;
}
