//! The user's answers about voices besides a name: No to a suggestion,
//! Not a person, and Remove name (schema v6).

mod common;

use common::{PERSON_ID, date, populated, uuid};
use steno_core::store::migrator;
use steno_core::*;

fn now() -> chrono::DateTime<chrono::Utc> {
    date("2026-10-01T00:00:00.000Z")
}

/// The sample's speakers come back from processing again, unchanged.
fn process_again(store: &Store, meeting: &Meeting) {
    let speakers = common::speakers(meeting.id);
    store
        .replace_transcript(meeting, &common::segments(meeting.id, &speakers), &speakers)
        .unwrap();
}

#[test]
fn no_to_a_suggestion_is_kept_and_never_touches_the_voice() {
    let (store, meeting) = populated();
    let suggested = store.speakers(meeting.id).unwrap()[0].clone();
    let jerome = store.person(uuid(PERSON_ID)).unwrap().unwrap();
    assert!(matches!(
        suggested.assignment,
        SpeakerAssignment::Suggested { person_id, .. } if person_id == jerome.id
    ));

    store.reject_suggestion(suggested.id, jerome.id).unwrap();
    let speaker = &store.speakers(meeting.id).unwrap()[0];
    assert_eq!(speaker.assignment, SpeakerAssignment::Unknown);
    assert_eq!(
        store.suggestion_rejections(meeting.id).unwrap(),
        [(suggested.id, [jerome.id].into())].into()
    );
    assert_eq!(
        store.person(jerome.id).unwrap().unwrap(),
        jerome,
        "a No leaves the voice as it was"
    );
    // Answering twice is one row.
    store.reject_suggestion(suggested.id, jerome.id).unwrap();
    assert_eq!(store.suggestion_rejections(meeting.id).unwrap().len(), 1);

    // Processing again suggests Jérôme for Speaker 1 once more: dropped.
    process_again(&store, &meeting);
    let speakers = store.speakers(meeting.id).unwrap();
    assert_eq!(speakers[0].assignment, SpeakerAssignment::Unknown);
    assert_eq!(
        store.suggestion_rejections(meeting.id).unwrap(),
        [(suggested.id, [jerome.id].into())].into(),
        "the No survives processing again"
    );

    // Naming the voice as that person after all answers the No.
    store.confirm_speaker(suggested.id, &jerome).unwrap();
    assert!(store.suggestion_rejections(meeting.id).unwrap().is_empty());

    assert!(matches!(
        store.reject_suggestion(uuid::Uuid::new_v4(), jerome.id),
        Err(StoreError::SpeakerNotFound(_))
    ));
    assert!(matches!(
        store.reject_suggestion(suggested.id, uuid::Uuid::new_v4()),
        Err(StoreError::PersonNotFound(_))
    ));
}

#[test]
fn a_no_to_someone_else_leaves_the_suggestion() {
    let (store, meeting) = populated();
    let suggested = store.speakers(meeting.id).unwrap()[0].clone();
    let anna = store.resolve_person("Anna", None, now()).unwrap();
    store.save_person(&anna).unwrap();
    store.reject_suggestion(suggested.id, anna.id).unwrap();
    assert_eq!(
        store.speakers(meeting.id).unwrap()[0].assignment,
        suggested.assignment
    );
}

#[test]
fn not_a_person_leaves_voice_memory_and_keeps_the_text() {
    let (store, meeting) = populated();
    let speaker = store.speakers(meeting.id).unwrap()[0].clone();
    let anna = store.resolve_person("Anna", None, now()).unwrap();
    store.confirm_speaker(speaker.id, &anna).unwrap();
    assert_eq!(store.person(anna.id).unwrap().unwrap().sample_count, 1);
    let segments_before = store.export(meeting.id).unwrap().segments;

    store.set_speaker_dismissed(speaker.id, true).unwrap();
    let dismissed = &store.speakers(meeting.id).unwrap()[0];
    assert!(dismissed.dismissed);
    assert_eq!(dismissed.assignment, SpeakerAssignment::Unknown);
    let voice = store.person(anna.id).unwrap().unwrap();
    assert_eq!(voice.sample_count, 0, "the voice no longer learns from it");
    assert_eq!(voice.embedding, None);
    assert_eq!(
        store.export(meeting.id).unwrap().segments,
        segments_before,
        "the segments keep their text and their speaker"
    );
    assert!(
        store.recent_persons().unwrap().is_empty(),
        "Anna owns no voice any more"
    );

    // Processing again keeps the dismissal; the voice match is dropped.
    process_again(&store, &meeting);
    let again = &store.speakers(meeting.id).unwrap()[0];
    assert!(again.dismissed);
    assert_eq!(again.assignment, SpeakerAssignment::Unknown);

    store.set_speaker_dismissed(speaker.id, false).unwrap();
    let restored = &store.speakers(meeting.id).unwrap()[0];
    assert!(!restored.dismissed);
    assert_eq!(restored.assignment, SpeakerAssignment::Unknown);

    // Naming a dismissed voice makes it a person again.
    store.set_speaker_dismissed(speaker.id, true).unwrap();
    store.confirm_speaker(speaker.id, &anna).unwrap();
    let named = &store.speakers(meeting.id).unwrap()[0];
    assert!(!named.dismissed);
    assert_eq!(
        named.assignment,
        SpeakerAssignment::Confirmed { person_id: anna.id }
    );
    assert!(matches!(
        store.set_speaker_dismissed(uuid::Uuid::new_v4(), true),
        Err(StoreError::SpeakerNotFound(_))
    ));
}

#[test]
fn remove_name_unnames_the_voice_and_recomputes_the_person() {
    let (store, meeting) = populated();
    let speaker = store.speakers(meeting.id).unwrap()[0].clone();
    let anna = store.resolve_person("Anna", None, now()).unwrap();
    store.confirm_speaker(speaker.id, &anna).unwrap();

    store.clear_speaker(speaker.id).unwrap();
    let cleared = &store.speakers(meeting.id).unwrap()[0];
    assert_eq!(cleared.assignment, SpeakerAssignment::Unknown);
    assert!(!cleared.dismissed);
    let person = store.person(anna.id).unwrap().unwrap();
    assert_eq!(person.display_name, "Anna", "the person keeps their name");
    assert_eq!(person.sample_count, 0);
    assert!(store.suggestion_rejections(meeting.id).unwrap().is_empty());

    // An unknown voice stays unknown; a missing one is an error.
    store.clear_speaker(speaker.id).unwrap();
    assert!(matches!(
        store.clear_speaker(uuid::Uuid::new_v4()),
        Err(StoreError::SpeakerNotFound(_))
    ));
}

#[test]
fn remove_name_on_me_unnames_the_me_participant_only() {
    let (store, meeting) = populated();
    let me_participant = Participant {
        id: uuid::Uuid::new_v4(),
        meeting_id: meeting.id,
        person_id: None,
        display_name: "Me".to_owned(),
        role: ParticipantRole::Me,
        email: None,
    };
    store.save_participant(&me_participant).unwrap();
    let me = Speaker {
        id: me_speaker_id(meeting.id),
        meeting_id: meeting.id,
        cluster_label: "Me".to_owned(),
        assignment: SpeakerAssignment::Unknown,
        embedding: None,
        sample_clip_range: None,
        sample_clip_url: None,
        cluster_confidence: 1.0,
        dismissed: false,
    };
    store.save_speaker(&me).unwrap();
    let nicolai = store.resolve_person("Nicolai", None, now()).unwrap();
    let mut settings = store.settings().unwrap();
    settings.self_person_id = Some(nicolai.id);
    store.save_settings(&settings).unwrap();
    store.confirm_speaker(me.id, &nicolai).unwrap();
    let named = store
        .participants(meeting.id)
        .unwrap()
        .into_iter()
        .find(|participant| participant.role == ParticipantRole::Me)
        .unwrap();
    assert_eq!(named.person_id, Some(nicolai.id));
    assert_eq!(named.display_name, "Nicolai");

    store.clear_speaker(me.id).unwrap();
    let unnamed = store
        .participants(meeting.id)
        .unwrap()
        .into_iter()
        .find(|participant| participant.role == ParticipantRole::Me)
        .unwrap();
    assert_eq!(unnamed.person_id, None);
    assert_eq!(unnamed.display_name, "Me");
    assert_eq!(
        store.settings().unwrap().self_person_id,
        Some(nicolai.id),
        "who you are is changed in Settings"
    );
    assert!(store.person(nicolai.id).unwrap().is_some());
}

/// A database from before v6 opens with every speaker a person and no
/// answers recorded.
#[test]
fn a_v5_database_reads_every_voice_as_not_dismissed() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("steno.sqlite");
    let mut connection = rusqlite::Connection::open(&path).unwrap();
    let transaction = connection.transaction().unwrap();
    transaction
        .execute_batch("CREATE TABLE grdb_migrations (identifier TEXT NOT NULL PRIMARY KEY)")
        .unwrap();
    let (latest, older) = migrator::MIGRATIONS.split_last().unwrap();
    assert_eq!(latest.identifier, "v6");
    for migration in older {
        transaction.execute_batch(migration.sql).unwrap();
        transaction
            .execute(
                "INSERT INTO grdb_migrations (identifier) VALUES (?1)",
                [migration.identifier],
            )
            .unwrap();
    }
    transaction.commit().unwrap();
    let meeting_id = uuid::Uuid::new_v4();
    let speaker_id = uuid::Uuid::new_v4();
    connection
        .execute_batch(&format!(
            "INSERT INTO meeting (id, title, startedAt, duration, source, tags, state, \
             templateID, summaryText, scratchpad, createdAt, updatedAt) VALUES \
             ('{meeting_id}', 'Old', '2026-09-29 14:00:00.000', 60, 'local', '[]', \
             'ready', 'default', '', '', '2026-09-29 14:00:00.000', \
             '2026-09-29 14:00:00.000');
             INSERT INTO speaker (id, meetingID, clusterLabel, assignment, clusterConfidence) \
             VALUES ('{speaker_id}', '{meeting_id}', 'Speaker 1', 'unknown', 0.9);",
            meeting_id = meeting_id.to_string().to_uppercase(),
            speaker_id = speaker_id.to_string().to_uppercase(),
        ))
        .unwrap();
    drop(connection);
    let store = Store::open(&path).unwrap();
    let speakers = store.speakers(meeting_id).unwrap();
    assert_eq!(speakers.len(), 1);
    assert!(!speakers[0].dismissed);
    assert!(store.suggestion_rejections(meeting_id).unwrap().is_empty());
}
