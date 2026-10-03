//! Speaker ids to the cluster labels the model sees, and the transcript as
//! the model reads it.
//! Swift: `Sources/StenoLLM/Transcript/SpeakerLabels.swift`.

use std::collections::HashMap;

use steno_core::{Speaker, TranscriptSegment};
use uuid::Uuid;

/// Speaker ids to the cluster labels the model sees ("Speaker 1"), and
/// back. Labels are what prompts and answers carry; the core's renderer
/// swaps them for names later, so the model never needs to know a person's
/// id.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SpeakerLabels {
    labels_by_id: HashMap<Uuid, String>,
    ids_by_label: HashMap<String, Uuid>,
}

impl SpeakerLabels {
    pub const UNKNOWN: &'static str = "Unknown speaker";

    #[must_use]
    pub fn new(speakers: &[Speaker]) -> Self {
        let mut labels = SpeakerLabels::default();
        for speaker in speakers {
            labels
                .labels_by_id
                .insert(speaker.id, speaker.cluster_label.clone());
            labels
                .ids_by_label
                .insert(speaker.cluster_label.to_lowercase(), speaker.id);
        }
        labels
    }

    #[must_use]
    pub fn label(&self, speaker_id: Option<Uuid>) -> &str {
        speaker_id
            .and_then(|id| self.labels_by_id.get(&id))
            .map_or(Self::UNKNOWN, String::as_str)
    }

    /// The speaker whose label matches, case-insensitively and ignoring
    /// surrounding whitespace; `None` for unknown labels.
    #[must_use]
    pub fn speaker_id(&self, label: &str) -> Option<Uuid> {
        self.ids_by_label.get(&label.trim().to_lowercase()).copied()
    }
}

/// The transcript as the model reads it: `[n] Speaker 1: text`, one line
/// per segment, `n` counting from 0.
#[must_use]
pub fn render_lines(segments: &[TranscriptSegment], labels: &SpeakerLabels) -> String {
    segments
        .iter()
        .enumerate()
        .map(|(offset, segment)| {
            format!(
                "[{offset}] {}: {}",
                labels.label(segment.speaker_id),
                segment.text
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Lines without indices for the summary pass, where nothing is mapped
/// back by position: `Speaker 1: text`.
#[must_use]
pub fn render_plain_lines(segments: &[TranscriptSegment], labels: &SpeakerLabels) -> String {
    segments
        .iter()
        .map(|segment| format!("{}: {}", labels.label(segment.speaker_id), segment.text))
        .collect::<Vec<_>>()
        .join("\n")
}
