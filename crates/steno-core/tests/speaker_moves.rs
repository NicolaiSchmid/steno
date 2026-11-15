//! Paragraph moves and undoable combining (schema v7): a turn moves to a
//! person's voice, a combined voice keeps its own row and Separate again
//! restores it.

mod common;

use common::{PERSON_ID, date, populated, uuid};
use steno_core::store::migrator;
use steno_core::*;
use uuid::Uuid;

fn now() -> chrono::DateTime<chrono::Utc> {
    date("2026-10-01T00:00:00.000Z")
}

fn segment(
    meeting_id: Uuid,
    name: &str,
    start: f64,
    speaker_id: Option<Uuid>,
) -> TranscriptSegment {
    TranscriptSegment {
        id: derived_uuid(meeting_id, name),
        meeting_id,
        start,
        end: start + 2.0,
        speaker_id,
        lane: AudioLane::System,
        text: format!("{name}."),
        raw_text: name.to_owned(),
    }
}

/// The sample with Speaker 1 (suggested Jérôme, a clip and an embedding),
/// Speaker 2 (unknown, no embedding) and Speaker 3 (unknown, an embedding),
/// each leading paragraphs, and one paragraph without a voice.
struct Meeting3 {
    store: Store,
    meeting: Meeting,
    speakers: Vec<Speaker>,
    jerome: Person,
}

impl Meeting3 {
    fn new() -> Self {
        let (store, meeting) = populated();
        let mut speakers = common::speakers(meeting.id);
        speakers.push(Speaker {
            id: derived_uuid(meeting.id, "speaker-Speaker 3"),
            meeting_id: meeting.id,
            cluster_label: "Speaker 3".to_owned(),
            assignment: SpeakerAssignment::Unknown,
            embedding: Some(Embedding(vec![0.5; Embedding::DIMENSION])),
            sample_clip_range: Some(TimeRange {
                lower: 20.0,
                upper: 22.0,
            }),
            sample_clip_url: Some("file:///tmp/clip-3.wav".to_owned()),
            cluster_confidence: 0.8,
            dismissed: false,
        });
        let segments = vec![
            segment(meeting.id, "a", 0.0, Some(speakers[0].id)),
            segment(meeting.id, "b", 10.0, Some(speakers[1].id)),
            segment(meeting.id, "c", 12.0, Some(speakers[1].id)),
            segment(meeting.id, "d", 20.0, Some(speakers[2].id)),
            segment(meeting.id, "e", 30.0, None),
        ];
        store
            .replace_transcript(&meeting, &segments, &speakers)
            .unwrap();
        let jerome = store.person(uuid(PERSON_ID)).unwrap().unwrap();
        let speakers = store.speakers(meeting.id).unwrap();
        Meeting3 {
            store,
            meeting,
            speakers,
            jerome,
        }
    }

    fn id(&self, name: &str) -> Uuid {
        derived_uuid(self.meeting.id, name)
    }

    fn generation(&self) -> i64 {
        self.store.transcript_generation(self.meeting.id).unwrap()
    }

    /// Who each segment reads as, in time order.
    fn shown(&self) -> Vec<Option<Uuid>> {
        self.store
            .segments(self.meeting.id)
            .unwrap()
            .iter()
            .map(|segment| segment.speaker_id)
            .collect()
    }

    fn anna(&self) -> Person {
        self.store.resolve_person("Anna", None, now()).unwrap()
    }
}

#[test]
fn a_turn_moves_to_the_voice_the_person_already_has() {
    let m = Meeting3::new();
    m.store
        .confirm_speaker(m.speakers[0].id, &m.jerome)
        .unwrap();
    let voice_before = m.store.person(m.jerome.id).unwrap().unwrap();
    let speaker_2 = m.speakers[1].id;

    let clips = m
        .store
        .move_turn(
            m.meeting.id,
            m.generation(),
            &[(m.id("b"), Some(speaker_2))],
            &m.jerome,
        )
        .unwrap();
    assert_eq!(clips, Vec::<String>::new());
    assert_eq!(
        m.shown(),
        [
            Some(m.speakers[0].id),
            Some(m.speakers[0].id),
            Some(speaker_2),
            Some(m.speakers[2].id),
            None
        ]
    );
    assert_eq!(
        m.store.person(m.jerome.id).unwrap().unwrap(),
        voice_before,
        "a move teaches no voice"
    );
    assert_eq!(m.store.speakers(m.meeting.id).unwrap().len(), 3);
}

#[test]
fn a_turn_for_someone_without_a_voice_here_gets_a_new_voice() {
    let m = Meeting3::new();
    let anna = m.anna();
    let speaker_2 = m.speakers[1].id;
    m.store
        .move_turn(
            m.meeting.id,
            m.generation(),
            &[(m.id("b"), Some(speaker_2)), (m.id("c"), Some(speaker_2))],
            &anna,
        )
        .unwrap();
    let speakers = m.store.speakers(m.meeting.id).unwrap();
    let new = speakers
        .iter()
        .find(|speaker| speaker.person_id() == Some(anna.id))
        .expect("a voice for Anna");
    assert_eq!(new.cluster_label, "Speaker 4");
    assert_eq!(
        new.assignment,
        SpeakerAssignment::Confirmed { person_id: anna.id }
    );
    assert_eq!(new.embedding, None);
    assert_eq!(new.sample_clip_url, None);
    assert_eq!(new.sample_clip_range, None);
    assert!(new.cluster_confidence.abs() < f32::EPSILON);
    let anna = m.store.person(anna.id).unwrap().unwrap();
    assert_eq!(anna.sample_count, 0, "the new voice joins no voice memory");
    assert_eq!(anna.embedding, None);
    assert!(
        speakers.iter().all(|speaker| speaker.id != speaker_2),
        "Speaker 2 lost every paragraph and went"
    );

    // A turn without a voice moves the same way.
    m.store
        .move_turn(m.meeting.id, m.generation(), &[(m.id("e"), None)], &anna)
        .unwrap();
    assert_eq!(m.shown()[4], Some(new.id));
}

#[test]
fn an_emptied_voice_goes_with_its_clip() {
    let m = Meeting3::new();
    let speaker_3 = m.speakers[2].id;
    let clips = m
        .store
        .move_turn(
            m.meeting.id,
            m.generation(),
            &[(m.id("d"), Some(speaker_3))],
            &m.jerome,
        )
        .unwrap();
    assert_eq!(clips, ["file:///tmp/clip-3.wav"]);
    assert!(
        m.store
            .speakers(m.meeting.id)
            .unwrap()
            .iter()
            .all(|speaker| speaker.id != speaker_3)
    );
    assert!(
        !m.store
            .sample_clip_urls()
            .unwrap()
            .contains(&"file:///tmp/clip-3.wav".to_owned())
    );
}

#[test]
fn an_emptied_voice_leaves_its_persons_voice() {
    let m = Meeting3::new();
    let anna = m.anna();
    let speaker_3 = m.speakers[2].id;
    m.store.confirm_speaker(speaker_3, &anna).unwrap();
    assert_eq!(m.store.person(anna.id).unwrap().unwrap().sample_count, 1);
    m.store
        .move_turn(
            m.meeting.id,
            m.generation(),
            &[(m.id("d"), Some(speaker_3))],
            &m.jerome,
        )
        .unwrap();
    let anna = m.store.person(anna.id).unwrap().unwrap();
    assert_eq!(anna.sample_count, 0, "its contribution left Anna's voice");
    assert_eq!(anna.embedding, None);
}

#[test]
fn a_move_off_a_dismissed_voice_leaves_it_dismissed() {
    let m = Meeting3::new();
    let speaker_2 = m.speakers[1].id;
    m.store.set_speaker_dismissed(speaker_2, true).unwrap();
    m.store
        .move_turn(
            m.meeting.id,
            m.generation(),
            &[(m.id("b"), Some(speaker_2))],
            &m.jerome,
        )
        .unwrap();
    let speaker = m
        .store
        .speakers(m.meeting.id)
        .unwrap()
        .into_iter()
        .find(|speaker| speaker.id == speaker_2)
        .unwrap();
    assert!(speaker.dismissed);
}

#[test]
fn a_stale_move_changes_nothing() {
    let m = Meeting3::new();
    let before = m.shown();
    let generation = m.generation();
    let speaker_2 = m.speakers[1].id;
    for (generation, segments) in [
        // Another voice than the one the segment is under.
        (generation, vec![(m.id("b"), Some(m.speakers[0].id))]),
        (generation, vec![(m.id("e"), Some(speaker_2))]),
        // Processed again since.
        (generation - 1, vec![(m.id("b"), Some(speaker_2))]),
        // A segment that is gone, and no segment at all.
        (generation, vec![(Uuid::new_v4(), Some(speaker_2))]),
        (generation, vec![]),
    ] {
        assert!(matches!(
            m.store
                .move_turn(m.meeting.id, generation, &segments, &m.jerome),
            Err(StoreError::StaleTranscript)
        ));
    }
    assert_eq!(m.shown(), before);
    assert_eq!(m.store.speakers(m.meeting.id).unwrap(), m.speakers);
}

#[test]
fn processing_again_raises_the_generation_and_drops_combines() {
    let m = Meeting3::new();
    let generation = m.generation();
    let anna = m.anna();
    m.store.confirm_speaker(m.speakers[1].id, &anna).unwrap();
    m.store.confirm_speaker(m.speakers[2].id, &anna).unwrap();
    assert_eq!(m.store.merged_speakers(m.meeting.id).unwrap().len(), 1);

    let speakers = common::speakers(m.meeting.id);
    m.store
        .replace_transcript(
            &m.meeting,
            &common::segments(m.meeting.id, &speakers),
            &speakers,
        )
        .unwrap();
    assert_eq!(m.generation(), generation + 1);
    assert_eq!(
        m.store.merged_speakers(m.meeting.id).unwrap(),
        Vec::<MergedSpeaker>::new()
    );
    assert!(matches!(
        m.store.move_turn(
            m.meeting.id,
            generation,
            &[(m.id("a"), Some(m.speakers[0].id))],
            &anna
        ),
        Err(StoreError::StaleTranscript)
    ));
}

/// Every column of every row of the tables a combine could touch.
fn snapshot(store: &Store) -> Vec<Vec<String>> {
    store
        .read(|connection| {
            let mut rows = Vec::new();
            for sql in [
                "SELECT * FROM speaker ORDER BY id",
                "SELECT * FROM transcriptSegment ORDER BY id",
                "SELECT * FROM speakerNameSuggestion ORDER BY speakerID",
                "SELECT * FROM speakerSuggestionRejection ORDER BY speakerID, personID",
                "SELECT * FROM participant ORDER BY id",
                "SELECT * FROM person ORDER BY id",
            ] {
                let mut statement = connection.prepare(sql)?;
                let columns = statement.column_count();
                let table = statement.query_map([], |row| {
                    (0..columns)
                        .map(|index| row.get_ref(index).map(|value| format!("{sql}: {value:?}")))
                        .collect::<rusqlite::Result<Vec<_>>>()
                })?;
                for row in table {
                    rows.push(row?);
                }
            }
            Ok(rows)
        })
        .unwrap()
}

#[test]
fn separate_again_restores_the_combined_row_exactly() {
    let m = Meeting3::new();
    // Speaker 3 carries a No and a dismissal before it is combined.
    let anna = m.anna();
    m.store.save_person(&anna).unwrap();
    m.store
        .confirm_speaker(m.speakers[0].id, &m.jerome)
        .unwrap();
    let source = m.speakers[2].id;
    m.store.reject_suggestion(source, anna.id).unwrap();
    m.store.set_speaker_dismissed(source, true).unwrap();
    let before = snapshot(&m.store);
    let voice_before = m.store.person(m.jerome.id).unwrap().unwrap();

    // Combining: Speaker 3 reads as Jérôme's voice, its row kept.
    m.store.confirm_speaker(source, &m.jerome).unwrap();
    let speakers = m.store.speakers(m.meeting.id).unwrap();
    assert!(speakers.iter().all(|speaker| speaker.id != source));
    let merged = m.store.merged_speakers(m.meeting.id).unwrap();
    assert_eq!(merged.len(), 1);
    assert_eq!(merged[0].speaker.id, source);
    assert_eq!(merged[0].into, m.speakers[0].id);
    assert_eq!(merged[0].root, m.speakers[0].id);
    assert!(merged[0].speaker.dismissed, "the row is kept as it was");
    assert_eq!(m.shown()[3], Some(m.speakers[0].id));
    let voice = m.store.person(m.jerome.id).unwrap().unwrap();
    assert_eq!(voice.sample_count, 2, "both voices count for Jérôme");
    assert_eq!(
        m.store.sample_clip_urls().unwrap(),
        ["file:///tmp/clip-3.wav"],
        "the combined voice keeps its clip"
    );

    m.store.separate_speaker(source).unwrap();
    assert_eq!(
        snapshot(&m.store),
        before,
        "every row as before the combine"
    );
    assert_eq!(m.store.person(m.jerome.id).unwrap().unwrap(), voice_before);
    assert_eq!(
        m.store.merged_speakers(m.meeting.id).unwrap(),
        Vec::<MergedSpeaker>::new()
    );

    assert!(matches!(
        m.store.separate_speaker(source),
        Err(StoreError::NotCombined(_))
    ));
    assert!(matches!(
        m.store.separate_speaker(Uuid::new_v4()),
        Err(StoreError::NotCombined(_))
    ));
}

#[test]
fn separating_me_names_the_me_participant_by_its_own_answer() {
    let (store, meeting) = populated();
    let me = me_speaker_id(meeting.id);
    let mut speakers = common::speakers(meeting.id);
    speakers.push(Speaker {
        id: me,
        meeting_id: meeting.id,
        cluster_label: "Me".to_owned(),
        assignment: SpeakerAssignment::Unknown,
        embedding: Some(Embedding(vec![0.1; Embedding::DIMENSION])),
        sample_clip_range: None,
        sample_clip_url: None,
        cluster_confidence: 1.0,
        dismissed: false,
    });
    let segments = vec![
        segment(meeting.id, "a", 0.0, Some(speakers[0].id)),
        segment(meeting.id, "m", 5.0, Some(me)),
    ];
    store
        .replace_transcript(&meeting, &segments, &speakers)
        .unwrap();
    let jerome = store.person(uuid(PERSON_ID)).unwrap().unwrap();
    store.confirm_speaker(speakers[0].id, &jerome).unwrap();
    let participant = |store: &Store| {
        store
            .participants(meeting.id)
            .unwrap()
            .into_iter()
            .find(|participant| participant.role == ParticipantRole::Me)
            .unwrap()
    };
    let unnamed = participant(&store);

    store.confirm_speaker(me, &jerome).unwrap();
    assert_eq!(participant(&store).person_id, Some(jerome.id));
    store.separate_speaker(me).unwrap();
    assert_eq!(participant(&store), unnamed, "Me is unnamed again");
}

#[test]
fn separate_again_keeps_a_later_move_and_refuses_when_nothing_is_left() {
    let m = Meeting3::new();
    let anna = m.anna();
    m.store
        .confirm_speaker(m.speakers[0].id, &m.jerome)
        .unwrap();
    let source = m.speakers[1].id;
    m.store.confirm_speaker(source, &m.jerome).unwrap();
    let root = Some(m.speakers[0].id);
    // Paragraph c, Speaker 2's, moves on to Anna after the combine.
    m.store
        .move_turn(m.meeting.id, m.generation(), &[(m.id("c"), root)], &anna)
        .unwrap();
    let anna_voice = m
        .store
        .speakers(m.meeting.id)
        .unwrap()
        .into_iter()
        .find(|speaker| speaker.person_id() == Some(anna.id))
        .unwrap()
        .id;
    m.store.separate_speaker(source).unwrap();
    assert_eq!(
        m.shown(),
        [
            root,
            Some(source),
            Some(anna_voice),
            Some(m.speakers[2].id),
            None
        ],
        "b goes back, c stays with Anna"
    );

    // Combined again, then its last paragraph moves away: nothing to
    // separate, and the emptied row goes.
    m.store.confirm_speaker(source, &m.jerome).unwrap();
    let clips = m
        .store
        .move_turn(m.meeting.id, m.generation(), &[(m.id("b"), root)], &anna)
        .unwrap();
    assert!(clips.is_empty(), "Speaker 2 had no clip");
    assert_eq!(
        m.store.merged_speakers(m.meeting.id).unwrap(),
        Vec::<MergedSpeaker>::new()
    );
    assert!(matches!(
        m.store.separate_speaker(source),
        Err(StoreError::NotCombined(_))
    ));
}

/// A combined voice whose paragraphs all went elsewhere by another build
/// (which knows no `mergedInto` and moves segments by hand) has nothing to
/// separate.
#[test]
fn a_combined_voice_with_its_paragraphs_gone_is_stale() {
    let m = Meeting3::new();
    m.store
        .confirm_speaker(m.speakers[0].id, &m.jerome)
        .unwrap();
    let source = m.speakers[2].id;
    m.store.confirm_speaker(source, &m.jerome).unwrap();
    m.store
        .write(|transaction| {
            transaction.execute(
                "UPDATE transcriptSegment SET speakerID = ?1 WHERE id = ?2",
                [
                    m.speakers[0].id.to_string().to_uppercase(),
                    m.id("d").to_string().to_uppercase(),
                ],
            )?;
            Ok(())
        })
        .unwrap();
    assert!(matches!(
        m.store.separate_speaker(source),
        Err(StoreError::StaleTranscript)
    ));
}

#[test]
fn a_v6_database_has_no_combined_voices() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("steno.sqlite");
    let mut connection = rusqlite::Connection::open(&path).unwrap();
    let transaction = connection.transaction().unwrap();
    transaction
        .execute_batch("CREATE TABLE grdb_migrations (identifier TEXT NOT NULL PRIMARY KEY)")
        .unwrap();
    let older = &migrator::MIGRATIONS[..6];
    assert_eq!(older.last().unwrap().identifier, "v6");
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
    let meeting_id = Uuid::new_v4();
    let speaker_id = Uuid::new_v4();
    let segment_id = Uuid::new_v4();
    // A v6 merge left one row and its segments retargeted.
    connection
        .execute_batch(&format!(
            "INSERT INTO meeting (id, title, startedAt, duration, source, tags, state, \
             templateID, summaryText, scratchpad, createdAt, updatedAt) VALUES \
             ('{meeting_id}', 'Old', '2026-09-29 14:00:00.000', 60, 'local', '[]', \
             'ready', 'default', '', '', '2026-09-29 14:00:00.000', \
             '2026-09-29 14:00:00.000');
             INSERT INTO speaker (id, meetingID, clusterLabel, assignment, clusterConfidence) \
             VALUES ('{speaker_id}', '{meeting_id}', 'Speaker 1', 'unknown', 0.9);
             INSERT INTO transcriptSegment (id, meetingID, start, end, speakerID, lane, text, \
             rawText) VALUES ('{segment_id}', '{meeting_id}', 0, 1, '{speaker_id}', 'mixed', \
             'Hallo.', 'hallo');",
            meeting_id = meeting_id.to_string().to_uppercase(),
            speaker_id = speaker_id.to_string().to_uppercase(),
            segment_id = segment_id.to_string().to_uppercase(),
        ))
        .unwrap();
    drop(connection);
    let store = Store::open(&path).unwrap();
    assert_eq!(
        store.merged_speakers(meeting_id).unwrap(),
        Vec::<MergedSpeaker>::new()
    );
    assert_eq!(store.transcript_generation(meeting_id).unwrap(), 0);
    assert_eq!(store.speakers(meeting_id).unwrap().len(), 1);
    assert_eq!(
        store.segments(meeting_id).unwrap()[0].speaker_id,
        Some(speaker_id)
    );
}
