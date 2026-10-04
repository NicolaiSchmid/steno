//! The queries the windows ask for: the export, full-text search, the
//! speaker picker's people lists, resolving a typed name, confirming a
//! speaker (with the merge a second confirmation triggers) and removing a
//! participant.

mod common;

use common::{PERSON_ID, date, populated, uuid};
use steno_core::*;

#[test]
fn the_export_carries_the_meeting_and_the_persons_it_points_at() {
    let (store, meeting) = populated();
    let export = store.export(meeting.id).unwrap();
    assert_eq!(export.meeting, meeting);
    assert_eq!(export.participants.len(), 1);
    assert_eq!(export.speakers.len(), 2);
    assert_eq!(export.segments.len(), 2);
    assert_eq!(export.tasks.len(), 2);
    assert_eq!(export.decisions.len(), 1);
    assert_eq!(export.persons.len(), 1, "the suggested speaker's person");
    assert_eq!(export.persons[0].id, uuid(PERSON_ID));
    assert!(export.audio.is_some());
    assert_eq!(export.schema_version, MeetingExport::CURRENT_SCHEMA_VERSION);
    assert!(matches!(
        store.export(uuid::Uuid::new_v4()),
        Err(StoreError::MeetingNotFound(_))
    ));
    assert_eq!(store.all_meetings().unwrap(), vec![meeting]);
}

/// `delete_participant` removes one row; a missing id is not an error.
/// Swift: `MeetingStore.deleteParticipant(id:)`.
#[test]
fn deleting_a_participant_removes_that_row_only() {
    let (store, meeting) = populated();
    let participant = store.participants(meeting.id).unwrap()[0].clone();
    store.delete_participant(uuid::Uuid::new_v4()).unwrap();
    assert_eq!(
        store.participants(meeting.id).unwrap(),
        std::slice::from_ref(&participant)
    );
    store.delete_participant(participant.id).unwrap();
    assert_eq!(store.participants(meeting.id).unwrap(), []);
    assert_eq!(store.export(meeting.id).unwrap().meeting, meeting);
}

#[test]
fn search_matches_every_token_across_segments_and_titles() {
    let (store, meeting) = populated();
    let hits = store.search("hallo zusammen", 50).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].meeting_id, meeting.id);
    assert!(hits[0].segment_id.is_some());
    assert!(hits[0].snippet.contains("[Hallo]"), "{}", hits[0].snippet);
    let title = store.search("call", 50).unwrap();
    assert!(
        title.iter().any(|hit| hit.segment_id.is_none()),
        "{title:?}"
    );
    let summary = store.search("store portieren", 50).unwrap();
    assert_eq!(summary.len(), 1);
    assert_eq!(store.search("   ", 50).unwrap(), Vec::new());
    assert_eq!(store.search("nothingmatches", 50).unwrap(), Vec::new());
    assert_eq!(
        store.search("hallo OR hi", 50).unwrap(),
        Vec::new(),
        "every token must match"
    );
}

#[test]
fn speakers_for_meetings_group_by_meeting_in_cluster_order() {
    let (store, meeting) = populated();
    let grouped = store
        .speakers_for_meetings(&[meeting.id, uuid::Uuid::new_v4()])
        .unwrap();
    assert_eq!(grouped.len(), 1);
    let labels: Vec<&str> = grouped[&meeting.id]
        .iter()
        .map(|speaker| speaker.cluster_label.as_str())
        .collect();
    assert_eq!(labels, ["Speaker 1", "Speaker 2"]);
    assert!(store.speakers_for_meetings(&[]).unwrap().is_empty());
}

#[test]
fn resolve_person_matches_names_ignoring_case_and_accents() {
    let (store, _) = populated();
    let existing = store
        .resolve_person(" jerome ", None, date("2026-10-01T00:00:00.000Z"))
        .unwrap();
    assert_eq!(existing.id, uuid(PERSON_ID));
    let fresh = store
        .resolve_person(
            " Anna ",
            Some("anna@example.com"),
            date("2026-10-01T00:00:00.000Z"),
        )
        .unwrap();
    assert_eq!(fresh.display_name, "Anna");
    assert_eq!(fresh.email.as_deref(), Some("anna@example.com"));
    assert_eq!(fresh.sample_count, 0);
    assert_eq!(
        store.persons().unwrap().len(),
        1,
        "resolving writes nothing"
    );
    assert!(matches!(
        store.resolve_person("  ", None, date("2026-10-01T00:00:00.000Z")),
        Err(StoreError::BlankPersonName)
    ));
}

#[test]
fn confirming_sets_the_assignment_saves_the_person_and_refreshes_the_voice() {
    let (store, meeting) = populated();
    let speakers = store.speakers(meeting.id).unwrap();
    let suggested = &speakers[0];
    let anna = store
        .resolve_person("Anna", None, date("2026-10-01T00:00:00.000Z"))
        .unwrap();
    assert!(
        store
            .name_suggestions(meeting.id)
            .unwrap()
            .iter()
            .all(|s| s.speaker_id != suggested.id)
    );

    let clips = store.confirm_speaker(suggested.id, &anna).unwrap();
    assert_eq!(
        clips,
        vec!["file:///tmp/clip-1.wav".to_owned()],
        "the master is not on disk, so the clip goes"
    );
    let speakers = store.speakers(meeting.id).unwrap();
    assert_eq!(
        speakers[0].assignment,
        SpeakerAssignment::Confirmed { person_id: anna.id }
    );
    assert_eq!(speakers[0].sample_clip_url, None);
    let stored = store.person(anna.id).unwrap().unwrap();
    assert_eq!(
        stored.sample_count, 1,
        "the voice is the speaker's embedding"
    );
    assert!(stored.embedding.is_some());
    assert_eq!(store.persons().unwrap().len(), 2);

    // The same person again changes nothing.
    assert_eq!(
        store.confirm_speaker(suggested.id, &anna).unwrap(),
        Vec::<String>::new()
    );
    assert!(matches!(
        store.confirm_speaker(uuid::Uuid::new_v4(), &anna),
        Err(StoreError::SpeakerNotFound(_))
    ));
}

#[test]
fn confirming_a_person_who_owns_another_speaker_merges_the_two() {
    let (store, meeting) = populated();
    let speakers = store.speakers(meeting.id).unwrap();
    let jerome = store.person(uuid(PERSON_ID)).unwrap().unwrap();
    store.confirm_speaker(speakers[0].id, &jerome).unwrap();
    // Speaker 2 (unknown, a name suggestion "Nicolai") also becomes Jérôme.
    store.confirm_speaker(speakers[1].id, &jerome).unwrap();
    let export = store.export(meeting.id).unwrap();
    assert_eq!(export.speakers.len(), 1, "merged into Speaker 1");
    assert_eq!(export.speakers[0].id, speakers[0].id);
    assert!(
        export
            .segments
            .iter()
            .all(|segment| segment.speaker_id.is_none()
                || segment.speaker_id == Some(speakers[0].id))
    );
    assert_eq!(store.name_suggestions(meeting.id).unwrap(), Vec::new());
    let recent = store.recent_persons().unwrap();
    assert_eq!(
        recent.iter().map(|person| person.id).collect::<Vec<_>>(),
        [jerome.id]
    );
}
