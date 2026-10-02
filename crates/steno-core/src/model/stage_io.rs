//! The pipeline's stage boundaries: what the cleaner and the summarizer
//! receive and return, and the export every adapter receives.
//! Swift: `Sources/StenoCore/Model/StageIO.swift`.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::{
    AudioAsset, Decision, LanguageTag, LlmUsage, Meeting, MeetingTask, Participant, Person,
    Speaker, SpeakerNameSuggestion, SummaryDocument, SummaryTemplate, TranscriptSegment,
};

/// Input of the cleanup stage: the merged transcript plus everything that
/// helps the model fix names.
#[derive(Debug, Clone, PartialEq)]
pub struct CleanupInput {
    pub segments: Vec<TranscriptSegment>,
    pub language: Option<LanguageTag>,
    pub participants: Vec<Participant>,
    pub speakers: Vec<Speaker>,
    pub known_people: Vec<Person>,
}

/// Output of the cleanup stage: the same segments in the same order with
/// `text` rewritten; `failed_chunks` lists chunk indexes left as raw text.
#[derive(Debug, Clone, PartialEq)]
pub struct CleanupOutput {
    pub segments: Vec<TranscriptSegment>,
    pub failed_chunks: Vec<usize>,
    pub usage: LlmUsage,
}

/// Input of the summarize stage.
#[derive(Debug, Clone, PartialEq)]
pub struct SummaryInput {
    pub meeting: Meeting,
    pub segments: Vec<TranscriptSegment>,
    pub speakers: Vec<Speaker>,
    pub participants: Vec<Participant>,
    pub known_people: Vec<Person>,
    pub template: SummaryTemplate,
}

/// Output of the summarize stage, in the meeting language.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SummaryOutput {
    pub title: String,
    pub summary: SummaryDocument,
    pub decisions: Vec<String>,
    pub tasks: Vec<MeetingTask>,
    pub speaker_names: Vec<SpeakerNameSuggestion>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<LanguageTag>,
    pub usage: LlmUsage,
}

/// Everything an adapter receives; its `StenoJSON` encoding is
/// `meeting.json` everywhere.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MeetingExport {
    pub schema_version: i64,
    pub meeting: Meeting,
    pub participants: Vec<Participant>,
    pub speakers: Vec<Speaker>,
    pub persons: Vec<Person>,
    pub segments: Vec<TranscriptSegment>,
    pub tasks: Vec<MeetingTask>,
    pub decisions: Vec<Decision>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio: Option<AudioAsset>,
}

impl MeetingExport {
    /// The `schemaVersion` this crate writes.
    pub const CURRENT_SCHEMA_VERSION: i64 = 1;

    #[must_use]
    pub fn speaker(&self, id: Uuid) -> Option<&Speaker> {
        self.speakers.iter().find(|speaker| speaker.id == id)
    }

    #[must_use]
    pub fn person(&self, id: Uuid) -> Option<&Person> {
        self.persons.iter().find(|person| person.id == id)
    }

    /// The confirmed or suggested person's name, else the cluster label;
    /// "Unknown" for a speaker id the export does not carry.
    #[must_use]
    pub fn display_name_for_speaker(&self, id: Uuid) -> String {
        let Some(speaker) = self.speaker(id) else {
            return "Unknown".to_owned();
        };
        speaker
            .person_id()
            .and_then(|person_id| self.person(person_id))
            .map_or_else(
                || speaker.cluster_label.clone(),
                |person| person.display_name.clone(),
            )
    }
}
