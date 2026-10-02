//! The store's behaviour above the encodings: insert, fetch, update state,
//! delete with cascades, settings.

mod common;

use steno_core::*;

use common::{PERSON_ID, date, populated, uuid};

fn count(store: &Store, table: &str) -> i64 {
    store
        .read(|connection| {
            Ok(
                connection.query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
                    row.get(0)
                })?,
            )
        })
        .unwrap()
}

#[test]
fn a_meeting_with_everything_reads_back_equal() {
    let (store, meeting) = populated();
    assert_eq!(store.meeting(meeting.id).unwrap().unwrap(), meeting);
    assert_eq!(store.meetings(10, 0).unwrap(), vec![meeting.clone()]);
    assert_eq!(
        store
            .meetings_in_states(&[MeetingStateKind::Ready])
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        store
            .meetings_in_states(&[MeetingStateKind::Queued])
            .unwrap(),
        Vec::new()
    );

    let speakers = common::speakers(meeting.id);
    assert_eq!(store.speakers(meeting.id).unwrap(), speakers);
    assert_eq!(
        store.segments(meeting.id).unwrap(),
        common::segments(meeting.id, &speakers)
    );
    let mut tasks = common::tasks(meeting.id);
    tasks.sort_by_key(|task| json::uuid_string(task.id));
    assert_eq!(store.tasks(meeting.id).unwrap(), tasks);
    assert_eq!(store.decisions(meeting.id).unwrap()[0].text, "Ship it");
    assert_eq!(
        store.decisions(meeting.id).unwrap()[0].id,
        derived_uuid(meeting.id, "decision-0")
    );
    assert_eq!(
        store.asset(meeting.id).unwrap().unwrap(),
        common::asset(meeting.id)
    );
    assert_eq!(store.persons().unwrap(), vec![common::person()]);
    assert_eq!(
        store.participants(meeting.id).unwrap()[0].display_name,
        "Me"
    );
    let suggestions = store.name_suggestions(meeting.id).unwrap();
    assert_eq!(suggestions.len(), 1);
    assert_eq!(suggestions[0].name.as_deref(), Some("Nicolai"));
    assert_eq!(
        store.deliveries(meeting.id).unwrap()[0].status,
        DeliveryStatus::Delivered
    );
}

#[test]
fn state_updates_touch_only_their_columns() {
    let (store, meeting) = populated();
    let now = date("2026-09-30T09:00:00.000Z");
    store
        .set_state(
            MeetingState::Failed {
                reason: "x".to_owned(),
            },
            meeting.id,
            now,
        )
        .unwrap();
    let read = store.meeting(meeting.id).unwrap().unwrap();
    assert_eq!(
        read.state,
        MeetingState::Failed {
            reason: "x".to_owned()
        }
    );
    assert_eq!(read.updated_at, now);
    assert_eq!(read.scratchpad, meeting.scratchpad);

    store.rename(meeting.id, "  Kickoff  ", now).unwrap();
    let read = store.meeting(meeting.id).unwrap().unwrap();
    assert_eq!(
        (read.title.as_str(), read.title_origin),
        ("Kickoff", TitleOrigin::User)
    );
    store.rename(meeting.id, "   ", now).unwrap();
    assert_eq!(store.meeting(meeting.id).unwrap().unwrap().title, "Kickoff");

    // A stage that read the meeting earlier must not revert the user's edits.
    let mut stale = meeting.clone();
    stale.state = MeetingState::Ready;
    stale.title = "From the model".to_owned();
    stale.title_origin = TitleOrigin::Summary;
    stale.scratchpad = "stale".to_owned();
    store.replace_transcript(&stale, &[], &[]).unwrap();
    let read = store.meeting(meeting.id).unwrap().unwrap();
    assert_eq!(read.title, "From the model");
    assert_eq!(read.scratchpad, meeting.scratchpad);
    assert_eq!(store.segments(meeting.id).unwrap(), Vec::new());

    let missing = uuid("00000000-0000-4000-8000-000000000000");
    assert!(matches!(
        store.set_state(MeetingState::Ready, missing, now),
        Err(StoreError::MeetingNotFound(id)) if id == missing
    ));
}

#[test]
fn interrupted_recordings_fail_at_launch() {
    let store = Store::in_memory().unwrap();
    let mut meeting = common::meeting();
    meeting.state = MeetingState::Recording;
    store.save_meeting(&meeting).unwrap();
    let now = date("2026-09-30T09:00:00.000Z");
    assert_eq!(
        store
            .fail_interrupted_recordings("interrupted", now)
            .unwrap(),
        vec![meeting.id]
    );
    let read = store.meeting(meeting.id).unwrap().unwrap();
    assert_eq!(
        read.state,
        MeetingState::Failed {
            reason: "interrupted".to_owned()
        }
    );
    assert_eq!(read.updated_at, now);
    assert_eq!(
        store
            .fail_interrupted_recordings("interrupted", now)
            .unwrap(),
        Vec::<uuid::Uuid>::new()
    );
}

#[test]
fn deleting_a_meeting_cascades_and_keeps_persons() {
    let (store, meeting) = populated();
    for table in [
        "participant",
        "speaker",
        "transcriptSegment",
        "meetingTask",
        "decision",
        "audioAsset",
        "delivery",
        "speakerNameSuggestion",
    ] {
        assert!(count(&store, table) > 0, "{table} is populated");
    }

    let deleted = store.delete_meeting(meeting.id).unwrap();
    assert_eq!(deleted.assets, vec![common::asset(meeting.id)]);
    assert_eq!(deleted.clips, vec!["file:///tmp/clip-1.wav".to_owned()]);
    for table in [
        "meeting",
        "participant",
        "speaker",
        "transcriptSegment",
        "meetingTask",
        "decision",
        "audioAsset",
        "delivery",
        "speakerNameSuggestion",
    ] {
        assert_eq!(count(&store, table), 0, "{table} cascaded");
    }
    assert_eq!(count(&store, "person"), 1);
    assert_eq!(
        count(
            &store,
            "transcriptSegment_ft WHERE transcriptSegment_ft MATCH 'hallo'"
        ),
        0,
        "the FTS delete triggers fired"
    );
    assert!(matches!(
        store.delete_meeting(meeting.id),
        Err(StoreError::MeetingNotFound(_))
    ));
}

#[test]
fn a_busy_meeting_cannot_be_deleted() {
    let store = Store::in_memory().unwrap();
    let mut meeting = common::meeting();
    meeting.state = MeetingState::Processing;
    store.save_meeting(&meeting).unwrap();
    assert!(matches!(
        store.delete_meeting(meeting.id),
        Err(StoreError::MeetingBusy(_, MeetingStateKind::Processing))
    ));
    assert_eq!(count(&store, "meeting"), 1);
}

#[test]
fn settings_round_trip_one_row_per_property() {
    let store = Store::in_memory().unwrap();
    let defaults = Settings::defaults(&StenoPaths::new("/support"));
    assert_eq!(store.settings_with_defaults(&defaults).unwrap(), defaults);

    let mut settings = defaults.clone();
    settings.default_retention = AudioRetention::KeepDays(7);
    settings.launch_at_login = false;
    settings.llm_model = Some("gpt".to_owned());
    settings.llm_context_tokens = 16_000;
    settings.codex_confirmed_at = Some(date("2026-09-29T10:00:00.000Z"));
    settings.obsidian = Some(ObsidianSettings {
        vault_path: "/vault".to_owned(),
        people_folder: None,
        include_audio: true,
        task_tag: Some("#steno".to_owned()),
    });
    store.save_settings(&settings).unwrap();
    assert_eq!(store.settings_with_defaults(&defaults).unwrap(), settings);

    let rows: Vec<(String, String)> = store
        .read(|connection| {
            let mut statement =
                connection.prepare("SELECT key, value FROM setting ORDER BY key")?;
            let rows = statement.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
            Ok(rows.collect::<rusqlite::Result<_>>()?)
        })
        .unwrap();
    let row = |key: &str| rows.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str());
    assert_eq!(row("defaultRetention"), Some(r#"{"keepDays":7}"#));
    assert_eq!(row("launchAtLogin"), Some("false"));
    assert_eq!(row("llmModel"), Some(r#""gpt""#));
    assert_eq!(row("llmContextTokens"), Some("16000"));
    assert_eq!(row("speakerMatchThreshold"), Some("0.6"));
    assert_eq!(
        row("codexConfirmedAt"),
        Some(r#""2026-09-29T10:00:00.000Z""#)
    );
    assert_eq!(row("audioFolder"), Some(r#""file:///support/Audio/""#));
    assert_eq!(
        row("obsidian"),
        Some(r##"{"includeAudio":true,"taskTag":"#steno","vaultPath":"/vault"}"##)
    );
    assert_eq!(row("inputDeviceUID"), None, "a nil property has no row");
    assert_eq!(rows.len(), 13, "one row per non-nil property");
}

#[test]
fn persons_are_saved_and_listed_by_name() {
    let store = Store::in_memory().unwrap();
    let mut person = common::person();
    store.save_person(&person).unwrap();
    person.display_name = "Anna".to_owned();
    person.embedding = None;
    store.save_person(&person).unwrap();
    assert_eq!(store.persons().unwrap(), vec![person.clone()]);
    assert_eq!(store.person(uuid(PERSON_ID)).unwrap(), Some(person));
}
