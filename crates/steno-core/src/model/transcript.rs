//! Transcript segments and the lane they came from.
//! Swift: `Sources/StenoCore/Model/Transcript.swift`.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::json;
use crate::string_enum;

string_enum! {
    /// Which capture lane a segment or file belongs to.
    pub enum AudioLane {
        /// The Mac microphone in a call: "me".
        Mic = "mic",
        /// The process tap in a call: "them".
        System = "system",
        /// One room lane, fully diarized.
        Mixed = "mixed",
    }
}

/// One line of the transcript after merge and cleanup. `raw_text` keeps the
/// STT output; `text` is the cleaned version shown and exported.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TranscriptSegment {
    #[serde(with = "json::uuid_text")]
    pub id: Uuid,
    #[serde(rename = "meetingID", with = "json::uuid_text")]
    pub meeting_id: Uuid,
    pub start: f64,
    pub end: f64,
    #[serde(
        rename = "speakerID",
        default,
        with = "json::uuid_text_opt",
        skip_serializing_if = "Option::is_none"
    )]
    pub speaker_id: Option<Uuid>,
    pub lane: AudioLane,
    pub text: String,
    pub raw_text: String,
}

impl TranscriptSegment {
    #[must_use]
    pub fn duration(&self) -> f64 {
        (self.end - self.start).max(0.0)
    }
}
