//! Fixed values the fakes and their tests share. After
//! `Sources/StenoCore/Testing/SampleData.swift`, with its own title, ids
//! and dates.

use chrono::{DateTime, TimeZone, Utc};
use uuid::Uuid;

use crate::{Embedding, Meeting, MeetingExport, MeetingSource, MeetingState, Person, TitleOrigin};

/// The sample meeting's id.
#[must_use]
pub fn meeting_id() -> Uuid {
    Uuid::parse_str("516EADE8-40E5-4434-8AAF-9214A21A604E").unwrap()
}

/// `2026-09-29T13:49:11.135Z`.
#[must_use]
pub fn started_at() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 9, 29, 13, 49, 11).unwrap() + chrono::Duration::milliseconds(135)
}

/// A unit embedding along `axis` (modulo the dimension), so fakes hand out
/// voices that are orthogonal to each other.
#[must_use]
pub fn embedding(axis: usize) -> Embedding {
    let mut values = vec![0.0; Embedding::DIMENSION];
    values[axis % Embedding::DIMENSION] = 1.0;
    Embedding(values)
}

/// A ready meeting in German with the default template and no summary.
#[must_use]
pub fn meeting() -> Meeting {
    Meeting {
        id: meeting_id(),
        title: "Call 2026-09-29 15:49".to_owned(),
        started_at: started_at(),
        duration: 1234.5,
        language: Some("de".into()),
        source: MeetingSource::MacCall,
        calendar_event_id: None,
        tags: Vec::new(),
        state: MeetingState::Ready,
        end_reason: None,
        title_origin: TitleOrigin::Default,
        template_id: Meeting::DEFAULT_TEMPLATE_ID.to_owned(),
        summary: None,
        scratchpad: String::new(),
        llm_usage: None,
        created_at: started_at(),
        updated_at: started_at(),
    }
}

/// A person with the unit voice along `axis` and one confirmed sample.
#[must_use]
pub fn person(axis: usize, name: &str) -> Person {
    Person {
        id: crate::derived_uuid(meeting_id(), &format!("person-{axis}")),
        display_name: name.to_owned(),
        email: None,
        embedding: Some(embedding(axis)),
        sample_count: 1,
        created_at: started_at(),
    }
}

/// An export of [`meeting`] with nothing else in it.
#[must_use]
pub fn export() -> MeetingExport {
    MeetingExport {
        schema_version: MeetingExport::CURRENT_SCHEMA_VERSION,
        meeting: meeting(),
        participants: Vec::new(),
        speakers: Vec::new(),
        persons: Vec::new(),
        segments: Vec::new(),
        tasks: Vec::new(),
        decisions: Vec::new(),
        audio: None,
    }
}
