//! A `MeetingExport` (`meeting.json`) carries everything both passes need,
//! so the CLI and the fixtures feed the cleaner and summarizer from one
//! file.
//! Swift: `Sources/StenoLLM/Transcript/MeetingExport+StageInputs.swift`.

use steno_core::{CleanupInput, MeetingExport, SummaryInput, SummaryTemplate};

#[must_use]
pub fn cleanup_input(export: &MeetingExport) -> CleanupInput {
    CleanupInput {
        segments: export.segments.clone(),
        language: export.meeting.language.clone(),
        participants: export.participants.clone(),
        speakers: export.speakers.clone(),
        known_people: export.persons.clone(),
    }
}

/// `template` defaults to the meeting's template, then to `default`.
#[must_use]
pub fn summary_input(export: &MeetingExport, template: Option<&SummaryTemplate>) -> SummaryInput {
    let template = template
        .or_else(|| SummaryTemplate::bundled_with_id(&export.meeting.template_id))
        .unwrap_or(&SummaryTemplate::bundled()[0]);
    SummaryInput {
        meeting: export.meeting.clone(),
        segments: export.segments.clone(),
        speakers: export.speakers.clone(),
        participants: export.participants.clone(),
        known_people: export.persons.clone(),
        template: template.clone(),
    }
}
