//! The meeting: its source, state, end reason and title origin.
//! Swift: `Sources/StenoCore/Model/Meeting.swift`.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use uuid::Uuid;

use super::{LanguageTag, LlmUsage, SummaryDocument};
use crate::json::{
    self,
    case_coding::{self, Case},
};
use crate::string_enum;

string_enum! {
    /// Where a recording came from. Decides which lanes exist and which lane
    /// is diarized.
    pub enum MeetingSource {
        /// Two lanes on the computer (the variant name and raw value are
        /// Swift's): `mic` is "me", `system` is "them", unless the tap
        /// carried no conversation (a phone on speaker next to the
        /// computer); then the mic lane is diarized like a room
        /// (`steno_pipeline::pipeline::diarized_lane_after_transcription`).
        MacCall = "macCall",
        /// One `mixed` room lane from the computer's microphone, fully diarized.
        MacInPerson = "macInPerson",
        /// One `mixed` lane recorded by the phone and handed over.
        Phone = "phone",
    }
}

string_enum! {
    /// The case names of [`MeetingState`], shared by `meeting.json` and the
    /// `meeting.state` column.
    pub enum MeetingStateKind {
        Recording = "recording",
        Queued = "queued",
        Processing = "processing",
        Ready = "ready",
        Failed = "failed",
    }
}

/// The processing state machine: `recording → queued → processing → ready`,
/// or `failed(reason)` from any processing stage.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum MeetingState {
    Recording,
    Queued,
    Processing,
    Ready,
    Failed { reason: String },
}

impl MeetingState {
    #[must_use]
    pub fn kind(&self) -> MeetingStateKind {
        match self {
            MeetingState::Recording => MeetingStateKind::Recording,
            MeetingState::Queued => MeetingStateKind::Queued,
            MeetingState::Processing => MeetingStateKind::Processing,
            MeetingState::Ready => MeetingStateKind::Ready,
            MeetingState::Failed { .. } => MeetingStateKind::Failed,
        }
    }

    #[must_use]
    pub fn is_failed(&self) -> bool {
        matches!(self, MeetingState::Failed { .. })
    }

    /// The `failed` payload; `None` for every other case.
    #[must_use]
    pub fn failure_reason(&self) -> Option<&str> {
        match self {
            MeetingState::Failed { reason } => Some(reason),
            _ => None,
        }
    }

    /// The state from its two columns, `state` and `failureReason`.
    #[must_use]
    pub fn from_columns(kind: MeetingStateKind, failure_reason: Option<String>) -> Self {
        match kind {
            MeetingStateKind::Recording => MeetingState::Recording,
            MeetingStateKind::Queued => MeetingState::Queued,
            MeetingStateKind::Processing => MeetingState::Processing,
            MeetingStateKind::Ready => MeetingState::Ready,
            MeetingStateKind::Failed => MeetingState::Failed {
                reason: failure_reason.unwrap_or_default(),
            },
        }
    }
}

impl Serialize for MeetingState {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            MeetingState::Failed { reason } => {
                case_coding::serialize_payload(self.kind().as_str(), reason, serializer)
            }
            _ => case_coding::serialize_bare(self.kind().as_str(), serializer),
        }
    }
}

impl<'de> Deserialize<'de> for MeetingState {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let (name, payload) = Case::deserialize(deserializer)?.split()?;
        let kind: MeetingStateKind = name.parse().map_err(serde::de::Error::custom)?;
        Ok(match kind {
            MeetingStateKind::Failed => MeetingState::Failed {
                reason: case_coding::payload(&name, payload)?,
            },
            other => MeetingState::from_columns(other, None),
        })
    }
}

string_enum! {
    /// The case names of [`RecordingEndReason`].
    pub enum RecordingEndReasonKind {
        Manual = "manual",
        CallEnded = "callEnded",
        DeviceLost = "deviceLost",
        Quit = "quit",
        Failed = "failed",
    }
}

/// Why a recording ended; `None` for meetings recorded before it was stored
/// and for phone recordings. One `StenoJSON` text per row: `"manual"`,
/// `{"callEnded":"Zen"}`; a nameless `callEnded` is the bare `"callEnded"`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum RecordingEndReason {
    /// The user stopped it from any surface.
    Manual,
    /// The call app closed the microphone and the grace period ran out.
    CallEnded { app_name: Option<String> },
    /// An audio device disappeared and did not come back within the retries.
    DeviceLost,
    /// Steno quit while recording.
    Quit,
    /// The capture failed for another reason; the recording so far was kept.
    Failed,
}

impl RecordingEndReason {
    #[must_use]
    pub fn kind(&self) -> RecordingEndReasonKind {
        match self {
            RecordingEndReason::Manual => RecordingEndReasonKind::Manual,
            RecordingEndReason::CallEnded { .. } => RecordingEndReasonKind::CallEnded,
            RecordingEndReason::DeviceLost => RecordingEndReasonKind::DeviceLost,
            RecordingEndReason::Quit => RecordingEndReasonKind::Quit,
            RecordingEndReason::Failed => RecordingEndReasonKind::Failed,
        }
    }
}

impl Serialize for RecordingEndReason {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            RecordingEndReason::CallEnded {
                app_name: Some(name),
            } => case_coding::serialize_payload(self.kind().as_str(), name, serializer),
            _ => case_coding::serialize_bare(self.kind().as_str(), serializer),
        }
    }
}

impl<'de> Deserialize<'de> for RecordingEndReason {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let (name, payload) = Case::deserialize(deserializer)?.split()?;
        let kind: RecordingEndReasonKind = name.parse().map_err(serde::de::Error::custom)?;
        Ok(match kind {
            RecordingEndReasonKind::Manual => RecordingEndReason::Manual,
            RecordingEndReasonKind::CallEnded => {
                let app_name = match payload {
                    None | Some(serde_json::Value::Null) => None,
                    Some(value) => Some(case_coding::payload(&name, Some(value))?),
                };
                RecordingEndReason::CallEnded { app_name }
            }
            RecordingEndReasonKind::DeviceLost => RecordingEndReason::DeviceLost,
            RecordingEndReasonKind::Quit => RecordingEndReason::Quit,
            RecordingEndReasonKind::Failed => RecordingEndReason::Failed,
        })
    }
}

string_enum! {
    /// Where `Meeting.title` came from.
    pub enum TitleOrigin {
        /// The intake's default title.
        Default = "default",
        /// The overlapping calendar event's title.
        Calendar = "calendar",
        /// The model's title from the summarize stage.
        Summary = "summary",
        /// Typed by the user.
        User = "user",
    }
}

impl TitleOrigin {
    #[must_use]
    pub fn is_default(&self) -> bool {
        *self == TitleOrigin::Default
    }
}

// The derive would need `#[default]` inside `string_enum!`; one impl is clearer.
#[allow(clippy::derivable_impls)]
impl Default for TitleOrigin {
    fn default() -> Self {
        TitleOrigin::Default
    }
}

/// One meeting: the row every other row hangs off. The JSON form omits
/// `titleOrigin` when it is the default, as Swift does.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Meeting {
    #[serde(with = "json::uuid_text")]
    pub id: Uuid,
    pub title: String,
    #[serde(with = "json::iso_time")]
    pub started_at: DateTime<Utc>,
    /// Length of the recording in seconds.
    pub duration: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<LanguageTag>,
    pub source: MeetingSource,
    #[serde(
        rename = "calendarEventID",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub calendar_event_id: Option<String>,
    pub tags: Vec<String>,
    pub state: MeetingState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_reason: Option<RecordingEndReason>,
    #[serde(default, skip_serializing_if = "TitleOrigin::is_default")]
    pub title_origin: TitleOrigin,
    #[serde(rename = "templateID")]
    pub template_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<SummaryDocument>,
    /// Free text typed by the user; the one editable text of a meeting.
    pub scratchpad: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub llm_usage: Option<LlmUsage>,
    #[serde(with = "json::iso_time")]
    pub created_at: DateTime<Utc>,
    #[serde(with = "json::iso_time")]
    pub updated_at: DateTime<Utc>,
}

impl Meeting {
    /// The id of the bundled default summary template.
    pub const DEFAULT_TEMPLATE_ID: &'static str = "default";

    /// Copies the columns the pipeline owns from `results`: title with its
    /// origin, language, state, summary, usage and `updatedAt`. Everything
    /// the user or the app owns stays as stored, the template included: the
    /// user picks it, so a run that read the meeting before a pick never
    /// writes the old one back, and only a summary re-run stores the
    /// template it ran with (`Store::replace_summary_with_template`). Rust
    /// only: Swift's `writeProcessingResults` copies the template too.
    pub fn apply_processing_results(&mut self, results: &Meeting) {
        self.title.clone_from(&results.title);
        self.title_origin = results.title_origin;
        self.language.clone_from(&results.language);
        self.state.clone_from(&results.state);
        self.summary.clone_from(&results.summary);
        self.llm_usage = results.llm_usage;
        self.updated_at = results.updated_at;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::json::to_column_string;

    #[test]
    fn payload_enums_use_the_case_coding_shape() {
        assert_eq!(
            to_column_string(&MeetingState::Ready).unwrap(),
            r#""ready""#
        );
        assert_eq!(
            to_column_string(&MeetingState::Failed { reason: "x".into() }).unwrap(),
            r#"{"failed":"x"}"#
        );
        assert_eq!(
            to_column_string(&RecordingEndReason::CallEnded {
                app_name: Some("Zen".into())
            })
            .unwrap(),
            r#"{"callEnded":"Zen"}"#
        );
        assert_eq!(
            to_column_string(&RecordingEndReason::CallEnded { app_name: None }).unwrap(),
            r#""callEnded""#
        );
        let parsed: RecordingEndReason = serde_json::from_str(r#"{"callEnded":null}"#).unwrap();
        assert_eq!(parsed, RecordingEndReason::CallEnded { app_name: None });
        assert!(serde_json::from_str::<MeetingState>(r#""paused""#).is_err());
        assert!(serde_json::from_str::<MeetingState>(r#""failed""#).is_err());
    }
}
