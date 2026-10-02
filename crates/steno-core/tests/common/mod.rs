//! A sample meeting with everything the store writes for it.

#![allow(dead_code)]

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use steno_core::*;
use uuid::Uuid;

pub fn date(text: &str) -> DateTime<Utc> {
    steno_core::json::parse_date(text).unwrap()
}

pub fn uuid(text: &str) -> Uuid {
    Uuid::parse_str(text).unwrap()
}

pub const MEETING_ID: &str = "516EADE8-40E5-4434-8AAF-9214A21A604E";
pub const PERSON_ID: &str = "0A4B6D1E-3C2F-4E5A-9B8C-7D6E5F4A3B2C";

pub fn meeting() -> Meeting {
    Meeting {
        id: uuid(MEETING_ID),
        title: "Call 2026-09-29 15:49".to_owned(),
        started_at: date("2026-09-29T13:49:11.135Z"),
        duration: 1234.5,
        language: Some("de".into()),
        source: MeetingSource::MacCall,
        calendar_event_id: None,
        tags: vec!["alpha".to_owned(), "beta/gamma".to_owned()],
        state: MeetingState::Ready,
        end_reason: Some(RecordingEndReason::CallEnded {
            app_name: Some("Zen".to_owned()),
        }),
        title_origin: TitleOrigin::Default,
        template_id: Meeting::DEFAULT_TEMPLATE_ID.to_owned(),
        summary: Some(SummaryDocument {
            template_id: "default".to_owned(),
            language: Some("de".into()),
            sections: vec![SummarySection {
                id: "overview".to_owned(),
                heading: "Überblick".to_owned(),
                bullets: vec![
                    SummaryBullet {
                        lead: "Ziel".to_owned(),
                        text: "Den Store portieren".to_owned(),
                    },
                    SummaryBullet {
                        lead: String::new(),
                        text: "Ohne Migration".to_owned(),
                    },
                ],
            }],
        }),
        scratchpad: "notes".to_owned(),
        llm_usage: Some(LlmUsage {
            prompt_tokens: 120,
            completion_tokens: 30,
            requests: 2,
        }),
        created_at: date("2026-09-29T14:29:23.685Z"),
        updated_at: date("2026-09-29T14:33:05.904Z"),
    }
}

pub fn person() -> Person {
    Person {
        id: uuid(PERSON_ID),
        display_name: "Jérôme".to_owned(),
        email: Some("jerome@example.com".to_owned()),
        embedding: Some(Embedding(vec![0.5; Embedding::DIMENSION])),
        sample_count: 1,
        created_at: date("2026-09-01T08:00:00.000Z"),
    }
}

pub fn speakers(meeting_id: Uuid) -> Vec<Speaker> {
    vec![
        Speaker {
            id: derived_uuid(meeting_id, "speaker-Speaker 1"),
            meeting_id,
            cluster_label: "Speaker 1".to_owned(),
            assignment: SpeakerAssignment::Suggested {
                person_id: uuid(PERSON_ID),
                similarity: 0.7,
            },
            embedding: Some(Embedding(vec![0.25; Embedding::DIMENSION])),
            sample_clip_range: Some(TimeRange {
                lower: 12.0,
                upper: 22.0,
            }),
            sample_clip_url: Some("file:///tmp/clip-1.wav".to_owned()),
            cluster_confidence: 0.9,
        },
        Speaker {
            id: derived_uuid(meeting_id, "speaker-Speaker 2"),
            meeting_id,
            cluster_label: "Speaker 2".to_owned(),
            assignment: SpeakerAssignment::Unknown,
            embedding: None,
            sample_clip_range: None,
            sample_clip_url: None,
            cluster_confidence: 0.6,
        },
    ]
}

pub fn segments(meeting_id: Uuid, speakers: &[Speaker]) -> Vec<TranscriptSegment> {
    vec![
        TranscriptSegment {
            id: derived_uuid(meeting_id, "segment-mic-0"),
            meeting_id,
            start: 0.0,
            end: 2.5,
            speaker_id: Some(speakers[0].id),
            lane: AudioLane::Mic,
            text: "Hallo zusammen.".to_owned(),
            raw_text: "hallo zusammen".to_owned(),
        },
        TranscriptSegment {
            id: derived_uuid(meeting_id, "segment-system-0"),
            meeting_id,
            start: 2.5,
            end: 5.0,
            speaker_id: None,
            lane: AudioLane::System,
            text: "Hi.".to_owned(),
            raw_text: "hi".to_owned(),
        },
    ]
}

pub fn tasks(meeting_id: Uuid) -> Vec<MeetingTask> {
    vec![
        MeetingTask {
            id: derived_uuid(meeting_id, "task-0"),
            meeting_id,
            text: "Port the store".to_owned(),
            assignee_person_id: Some(uuid(PERSON_ID)),
            assignee_name: Some("Jérôme".to_owned()),
            priority: TaskPriority::High,
            due_date: Some(date("2026-10-03T00:00:00.000Z")),
            done: true,
        },
        MeetingTask {
            id: derived_uuid(meeting_id, "task-1"),
            meeting_id,
            text: "Open the PR".to_owned(),
            assignee_person_id: None,
            assignee_name: None,
            priority: TaskPriority::Normal,
            due_date: None,
            done: false,
        },
    ]
}

pub fn asset(meeting_id: Uuid) -> AudioAsset {
    AudioAsset {
        id: derived_uuid(meeting_id, "asset"),
        meeting_id,
        url: format!("file:///tmp/Audio/{MEETING_ID}/master.caf"),
        format: AudioFormat::Caf48kFloat32,
        lanes: vec![AudioLane::Mic, AudioLane::System],
        sidecars_16k: BTreeMap::from([
            (
                AudioLane::System,
                format!("file:///tmp/Audio/{MEETING_ID}/system.wav"),
            ),
            (
                AudioLane::Mic,
                format!("file:///tmp/Audio/{MEETING_ID}/mic.wav"),
            ),
        ]),
        mixdown_url: None,
        retention: AudioRetention::KeepDays(30),
        expires_at: Some(date("2026-10-29T14:33:05.904Z")),
    }
}

/// Writes the whole sample into `store`: person, meeting with a participant
/// and an asset, transcript, summary with tasks and decisions, a delivery.
pub fn populate(store: &Store) -> Meeting {
    let meeting = meeting();
    store.save_person(&person()).unwrap();
    let participant = Participant {
        id: derived_uuid(meeting.id, "participant-me"),
        meeting_id: meeting.id,
        person_id: None,
        display_name: "Me".to_owned(),
        role: ParticipantRole::Me,
        email: None,
    };
    store
        .save_meeting_with_participants(&meeting, &[participant])
        .unwrap();
    store.save_asset(&asset(meeting.id)).unwrap();
    let speakers = speakers(meeting.id);
    store
        .replace_transcript(&meeting, &segments(meeting.id, &speakers), &speakers)
        .unwrap();
    store
        .replace_summary(
            &meeting,
            &tasks(meeting.id),
            &["Ship it".to_owned()],
            &[SpeakerNameSuggestion {
                speaker_id: speakers[1].id,
                name: Some("Nicolai".to_owned()),
                confidence: 0.8,
                evidence: "introduced himself".to_owned(),
            }],
        )
        .unwrap();
    store
        .save_delivery(&Delivery {
            id: Delivery::id_for(meeting.id, "obsidian"),
            meeting_id: meeting.id,
            destination_id: "obsidian".to_owned(),
            status: DeliveryStatus::Delivered,
            last_attempt_at: Some(date("2026-09-29T14:34:00.000Z")),
            receipt: Some(DeliveryReceipt {
                root: "/vault".to_owned(),
                folder: "Meetings/2026-09-29".to_owned(),
                files: vec![DeliveredFile {
                    relative_path: "note.md".to_owned(),
                    ownership: FileOwnership::Owned,
                    sha256: vec![1, 2, 3],
                }],
                renderer_version: 3,
            }),
        })
        .unwrap();
    meeting
}
