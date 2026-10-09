//! The store's behaviour above the encodings: insert, fetch, update state,
//! delete with cascades, settings.

mod common;

use rusqlite::{OptionalExtension as _, params};
use steno_core::store::convert::{DbDate, DbUuid};
use steno_core::*;

use common::{MEETING_ID, PERSON_ID, date, populated, uuid};

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
            meeting.id,
            MeetingState::Failed {
                reason: "x".to_owned(),
            },
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

    // A stage that read the meeting earlier must not revert the user's
    // edits, the title they typed included (Rust only: Swift writes the
    // stage's title over it).
    let mut stale = meeting.clone();
    stale.state = MeetingState::Ready;
    stale.title = "From the model".to_owned();
    stale.title_origin = TitleOrigin::Summary;
    stale.scratchpad = "stale".to_owned();
    store.replace_transcript(&stale, &[], &[]).unwrap();
    let read = store.meeting(meeting.id).unwrap().unwrap();
    assert_eq!(read.state, MeetingState::Ready);
    assert_eq!(
        (read.title.as_str(), read.title_origin),
        ("Kickoff", TitleOrigin::User)
    );
    assert_eq!(read.scratchpad, meeting.scratchpad);
    assert_eq!(store.segments(meeting.id).unwrap(), Vec::new());

    let missing = uuid("00000000-0000-4000-8000-000000000000");
    assert!(matches!(
        store.set_state(missing, MeetingState::Ready, now),
        Err(StoreError::MeetingNotFound(id)) if id == missing
    ));
}

/// Only the rows named, and only while they still record: a recording
/// left alone, or one started since, keeps recording.
#[test]
fn only_the_named_recordings_fail() {
    let store = Store::in_memory().unwrap();
    let now = date("2026-09-30T09:00:00.000Z");
    let recording = |id: &str| {
        let mut meeting = common::meeting();
        meeting.id = uuid(id);
        meeting.state = MeetingState::Recording;
        store.save_meeting(&meeting).unwrap();
        meeting.id
    };
    let failed = recording("00000000-0000-4000-8000-0000000000a1");
    let left_alone = recording("00000000-0000-4000-8000-0000000000a2");
    let mut queued = common::meeting();
    queued.id = uuid("00000000-0000-4000-8000-0000000000a3");
    queued.state = MeetingState::Queued;
    store.save_meeting(&queued).unwrap();
    let missing = uuid("00000000-0000-4000-8000-0000000000a4");

    assert_eq!(
        store
            .fail_recordings(&[failed, queued.id, missing], "interrupted", now)
            .unwrap(),
        vec![failed]
    );
    let state = |id| store.meeting(id).unwrap().unwrap().state;
    assert_eq!(
        state(failed),
        MeetingState::Failed {
            reason: "interrupted".to_owned()
        }
    );
    assert_eq!(state(left_alone), MeetingState::Recording);
    assert_eq!(state(queued.id), MeetingState::Queued);
}

/// The reason is Swift's default, read from the Swift source so the two
/// cannot drift apart.
#[test]
fn the_interrupted_recording_reason_is_the_swift_default() {
    let swift = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../Sources/StenoCore/Storage/MeetingStore.swift"),
    )
    .unwrap();
    let declaration = format!(
        "reason: String = \"{}\"",
        Store::INTERRUPTED_RECORDING_REASON
    );
    assert!(swift.contains(&declaration), "{declaration}");
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
    assert!(matches!(
        store.delete_meeting_left_recording(meeting.id),
        Err(StoreError::MeetingBusy(_, MeetingStateKind::Processing))
    ));
    assert_eq!(count(&store, "meeting"), 1);

    // A recording row: refused by the plain delete, removed by the one for
    // a meeting left `recording` (Rust only).
    meeting.state = MeetingState::Recording;
    store.save_meeting(&meeting).unwrap();
    assert!(matches!(
        store.delete_meeting(meeting.id),
        Err(StoreError::MeetingBusy(_, MeetingStateKind::Recording))
    ));
    assert_eq!(count(&store, "meeting"), 1);
    store.delete_meeting_left_recording(meeting.id).unwrap();
    assert_eq!(count(&store, "meeting"), 0);
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
        extra: serde_json::Map::new(),
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

fn setting_row(store: &Store, key: &str) -> Option<String> {
    store
        .read(|connection| {
            Ok(connection
                .query_row("SELECT value FROM setting WHERE key = ?1", [key], |row| {
                    row.get(0)
                })
                .optional()?)
        })
        .unwrap()
}

fn put_setting_row(store: &Store, key: &str, value: &str) {
    store
        .write(|transaction| {
            transaction.execute(
                "INSERT OR REPLACE INTO setting (key, value) VALUES (?1, ?2)",
                params![key, value],
            )?;
            Ok(())
        })
        .unwrap();
}

/// The Swift app and a newer build share the file: a key this build does
/// not know is no reason to drop it.
#[test]
fn saving_settings_keeps_rows_for_unknown_keys() {
    let store = Store::in_memory().unwrap();
    let defaults = Settings::defaults(&StenoPaths::new("/support"));
    put_setting_row(&store, "aFutureSetting", r#"{"on":true}"#);
    let mut settings = store.settings_with_defaults(&defaults).unwrap();
    settings.launch_at_login = false;
    store.save_settings(&settings).unwrap();
    assert_eq!(
        setting_row(&store, "aFutureSetting").as_deref(),
        Some(r#"{"on":true}"#)
    );
    assert_eq!(
        setting_row(&store, "launchAtLogin").as_deref(),
        Some("false")
    );
    assert_eq!(store.settings_with_defaults(&defaults).unwrap(), settings);
}

#[test]
fn a_known_property_set_to_none_loses_its_row() {
    let store = Store::in_memory().unwrap();
    let defaults = Settings::defaults(&StenoPaths::new("/support"));
    let mut settings = defaults.clone();
    settings.llm_model = Some("gpt".to_owned());
    settings.obsidian = Some(ObsidianSettings {
        vault_path: "/vault".to_owned(),
        people_folder: None,
        include_audio: false,
        task_tag: None,
        extra: serde_json::Map::new(),
    });
    store.save_settings(&settings).unwrap();
    assert!(setting_row(&store, "llmModel").is_some());
    assert!(setting_row(&store, "obsidian").is_some());

    settings.llm_model = None;
    settings.obsidian = None;
    store.save_settings(&settings).unwrap();
    assert_eq!(setting_row(&store, "llmModel"), None);
    assert_eq!(setting_row(&store, "obsidian"), None);
    assert_eq!(store.settings_with_defaults(&defaults).unwrap(), settings);
}

/// A field inside the Obsidian value that this build does not know comes
/// back on save, next to the edit.
#[test]
fn an_unknown_obsidian_field_survives_a_load_edit_save() {
    let store = Store::in_memory().unwrap();
    let defaults = Settings::defaults(&StenoPaths::new("/support"));
    put_setting_row(
        &store,
        "obsidian",
        r#"{"futureFolder":"Daily","includeAudio":false,"vaultPath":"/vault"}"#,
    );
    let mut settings = store.settings_with_defaults(&defaults).unwrap();
    let obsidian = settings.obsidian.as_mut().unwrap();
    assert_eq!(obsidian.extra["futureFolder"], "Daily");
    obsidian.include_audio = true;
    store.save_settings(&settings).unwrap();
    assert_eq!(
        setting_row(&store, "obsidian").as_deref(),
        Some(r#"{"futureFolder":"Daily","includeAudio":true,"vaultPath":"/vault"}"#)
    );
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

#[test]
fn a_malformed_embedding_blob_is_a_read_error() {
    let store = Store::in_memory().unwrap();
    let person = common::person();
    store.save_person(&person).unwrap();
    store
        .write(|transaction| {
            transaction.execute(
                "UPDATE person SET embedding = ?1",
                [rusqlite::types::Value::Blob(vec![0, 1, 2, 3, 4])],
            )?;
            Ok(())
        })
        .unwrap();
    let error = store.person(person.id).unwrap_err();
    assert!(
        matches!(
            error,
            StoreError::Sqlite(rusqlite::Error::FromSqlConversionFailure(..))
        ),
        "{error}"
    );
    assert!(error.to_string().contains("out of 5 byte blob"), "{error}");
    assert!(!error.is_busy());
}

#[test]
fn is_busy_names_the_lock_errors() {
    let busy = |code| {
        StoreError::Sqlite(rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(code),
            None,
        ))
    };
    assert!(busy(rusqlite::ffi::SQLITE_BUSY).is_busy());
    assert!(busy(rusqlite::ffi::SQLITE_BUSY_SNAPSHOT).is_busy());
    assert!(busy(rusqlite::ffi::SQLITE_LOCKED).is_busy());
    assert!(!busy(rusqlite::ffi::SQLITE_CONSTRAINT).is_busy());
    assert!(!StoreError::MeetingNotFound(uuid(PERSON_ID)).is_busy());
}

/// A closure that returns `Err` leaves nothing behind: the transaction is
/// rolled back, the lock released, and the next write goes through.
#[test]
fn a_failed_write_rolls_back() {
    let store = Store::in_memory().unwrap();
    let person = common::person();
    let missing = uuid("00000000-0000-4000-8000-000000000000");
    let error = store
        .write(|transaction| {
            transaction.execute(
                "INSERT INTO person (id, displayName, createdAt) VALUES (?1, ?2, ?3)",
                params![
                    DbUuid(person.id),
                    person.display_name,
                    DbDate(person.created_at)
                ],
            )?;
            Err::<(), _>(StoreError::MeetingNotFound(missing))
        })
        .unwrap_err();
    assert!(matches!(error, StoreError::MeetingNotFound(id) if id == missing));
    assert_eq!(count(&store, "person"), 0, "the insert was rolled back");
    store.save_person(&person).unwrap();
    assert_eq!(count(&store, "person"), 1);
    assert_eq!(store.person(person.id).unwrap(), Some(person));
}

/// The same strictness on `speaker.embedding`: five bytes are not an
/// `f32` vector, and the meeting's speakers fail to read rather than lose
/// one embedding silently.
#[test]
fn a_malformed_speaker_embedding_is_a_read_error() {
    let (store, meeting) = populated();
    store
        .write(|transaction| {
            transaction.execute(
                "UPDATE speaker SET embedding = X'0102030405' WHERE meetingID = ?1",
                [DbUuid(meeting.id)],
            )?;
            Ok(())
        })
        .unwrap();
    let error = store.speakers(meeting.id).unwrap_err();
    assert!(
        matches!(
            error,
            StoreError::Sqlite(rusqlite::Error::FromSqlConversionFailure(..))
        ),
        "{error}"
    );
    assert!(error.to_string().contains("out of 5 byte blob"), "{error}");
}

/// `Store::read` is one snapshot: a write landing on the file from another
/// connection halfway through a read is not seen by the read's later
/// queries, as GRDB's `reader.read` guaranteed.
#[test]
fn a_read_sees_one_snapshot_while_another_store_writes() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("steno.sqlite");
    let reader = Store::open(&path).unwrap();
    let writer = Store::open(&path).unwrap();
    let meeting = common::populate(&reader);
    let (before, after) = reader
        .read(|connection| {
            let count = |connection: &rusqlite::Connection| -> steno_core::store::Result<i64> {
                Ok(connection.query_row("SELECT count(*) FROM meeting", [], |row| row.get(0))?)
            };
            let before = count(connection)?;
            let mut other = meeting.clone();
            other.id = uuid::Uuid::new_v4();
            writer.save_meeting(&other).unwrap();
            let after = count(connection)?;
            Ok((before, after))
        })
        .unwrap();
    assert_eq!(before, after, "the read's snapshot holds");
    assert_eq!(
        reader.all_meetings().unwrap().len(),
        usize::try_from(before).unwrap() + 1,
        "the next read sees the write"
    );
}

/// A second sample replaces the stored rate, not only its count: the fold
/// sees the first row and the upsert keeps what the fold returned.
#[test]
fn a_stage_rate_folds_every_sample_into_one_row() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path().join("steno.sqlite")).unwrap();
    let first = store
        .update_stage_rate(
            PipelineStage::Transcribe,
            "engine",
            date("2026-10-01T10:00:00.000Z"),
            |stored| {
                assert_eq!(stored, None);
                StageRate {
                    seconds_per_unit: 0.5,
                    samples: 1,
                }
            },
        )
        .unwrap();
    assert_eq!(first.seconds_per_unit, 0.5);
    store
        .update_stage_rate(
            PipelineStage::Transcribe,
            "engine",
            date("2026-10-01T10:01:00.000Z"),
            |stored| {
                assert_eq!(stored, Some(first));
                StageRate {
                    seconds_per_unit: 0.25,
                    samples: 2,
                }
            },
        )
        .unwrap();
    let row = store
        .stage_rate(PipelineStage::Transcribe, "engine")
        .unwrap()
        .unwrap();
    assert_eq!(
        row.rate,
        StageRate {
            seconds_per_unit: 0.25,
            samples: 2
        }
    );
    assert_eq!(row.updated_at, date("2026-10-01T10:01:00.000Z"));
    assert_eq!(count(&store, "stageRate"), 1);
}

/// A re-run's transcript keeps what the user confirmed: a speaker whose id
/// comes back stays confirmed, with the re-run's embedding, and the voices
/// of the persons involved are recomputed from what is stored now.
/// `.plans/2026-09-29-speaker-calibration.md`, decision 5.
#[test]
fn replacing_the_transcript_keeps_confirmed_speakers_and_refreshes_voices() {
    let (store, meeting) = populated();
    let jerome = common::person();
    let speakers = common::speakers(meeting.id);
    store.confirm_speaker(speakers[0].id, &jerome).unwrap();
    let voice = |store: &Store| store.person(jerome.id).unwrap().unwrap();
    assert_eq!(voice(&store).sample_count, 1);

    // The re-run diarizes "Speaker 1" again with another voice, unconfirmed.
    let mut rerun = speakers.clone();
    rerun[0].assignment = SpeakerAssignment::Unknown;
    rerun[0].embedding = Some(Embedding(vec![1.0; Embedding::DIMENSION]));
    store
        .replace_transcript(&meeting, &common::segments(meeting.id, &rerun), &rerun)
        .unwrap();
    let stored = store.speakers(meeting.id).unwrap();
    assert_eq!(
        stored[0].assignment,
        SpeakerAssignment::Confirmed {
            person_id: jerome.id
        }
    );
    assert_eq!(stored[0].embedding, rerun[0].embedding);
    assert_eq!(stored[1].assignment, SpeakerAssignment::Unknown);
    assert_eq!(
        voice(&store).embedding,
        Embedding::mean(&[rerun[0].embedding.clone().unwrap()])
    );
    assert_eq!(voice(&store).sample_count, 1);

    // A re-run in which the speaker no longer appears drops it, and the
    // person's voice no longer carries its embedding.
    store
        .replace_transcript(&meeting, &[], &rerun[1..])
        .unwrap();
    assert_eq!(store.speakers(meeting.id).unwrap().len(), 1);
    assert_eq!(voice(&store).embedding, None);
    assert_eq!(voice(&store).sample_count, 0);
}

/// A confirmed speaker whose id comes back without a clip keeps the clip
/// and range its row named, so the voice the user confirmed stays
/// playable and no sweep takes the file for a leftover; one that comes
/// back with a clip takes the new one, and an unconfirmed speaker takes
/// what the re-run gave it, no clip included.
#[test]
fn replacing_the_transcript_keeps_the_earlier_clip_of_a_confirmed_speaker_given_none() {
    let (store, meeting) = populated();
    let speakers = common::speakers(meeting.id);
    store
        .confirm_speaker(speakers[0].id, &common::person())
        .unwrap();
    let earlier = store.speakers(meeting.id).unwrap();

    let mut rerun = speakers.clone();
    for speaker in &mut rerun {
        speaker.assignment = SpeakerAssignment::Unknown;
        speaker.sample_clip_range = None;
        speaker.sample_clip_url = None;
    }
    store.replace_transcript(&meeting, &[], &rerun).unwrap();
    let stored = store.speakers(meeting.id).unwrap();
    assert!(stored[0].assignment.is_confirmed());
    assert_eq!(stored[0].sample_clip_url, earlier[0].sample_clip_url);
    assert_eq!(stored[0].sample_clip_range, earlier[0].sample_clip_range);
    assert_eq!(stored[1].sample_clip_url, None);

    rerun[0].sample_clip_range = Some(TimeRange {
        lower: 1.0,
        upper: 2.0,
    });
    rerun[0].sample_clip_url = Some("file:///tmp/clip-1-again.wav".to_owned());
    store.replace_transcript(&meeting, &[], &rerun).unwrap();
    let stored = store.speakers(meeting.id).unwrap();
    assert_eq!(stored[0].sample_clip_url, rerun[0].sample_clip_url);
    assert_eq!(stored[0].sample_clip_range, rerun[0].sample_clip_range);
}

/// A re-run's transcript keeps the model's name suggestion for a speaker
/// whose id comes back, so a run without a summarizer keeps it too; a
/// speaker that does not come back takes its suggestion with it.
#[test]
fn replacing_the_transcript_keeps_the_name_suggestions_of_returning_speakers() {
    let (store, meeting) = populated();
    let speakers = common::speakers(meeting.id);
    let before = store.name_suggestions(meeting.id).unwrap();
    assert_eq!(before.len(), 1);
    assert_eq!(before[0].speaker_id, speakers[1].id);
    store
        .replace_transcript(
            &meeting,
            &common::segments(meeting.id, &speakers),
            &speakers,
        )
        .unwrap();
    assert_eq!(store.name_suggestions(meeting.id).unwrap(), before);

    store
        .replace_transcript(&meeting, &[], &speakers[..1])
        .unwrap();
    assert_eq!(store.name_suggestions(meeting.id).unwrap(), Vec::new());
}

/// The cleanup pass's write: the text of each stored segment by id, the
/// speakers and every other column as they are.
#[test]
fn updating_segment_texts_leaves_the_speakers_and_other_columns_alone() {
    let (store, meeting) = populated();
    let speakers = common::speakers(meeting.id);
    store
        .confirm_speaker(speakers[1].id, &common::person())
        .unwrap();
    let before = store.speakers(meeting.id).unwrap();
    let mut cleaned = common::segments(meeting.id, &speakers);
    for segment in &mut cleaned {
        segment.text = format!("{} (cleaned)", segment.raw_text);
        // A stale copy's speaker is not written back.
        segment.speaker_id = None;
    }
    let mut gone = cleaned[0].clone();
    gone.id = uuid("00000000-0000-4000-8000-0000000000aa");
    cleaned.push(gone);
    store.update_segment_texts(&meeting, &cleaned).unwrap();

    assert_eq!(store.speakers(meeting.id).unwrap(), before);
    let stored = store.segments(meeting.id).unwrap();
    let original = common::segments(meeting.id, &speakers);
    assert_eq!(
        stored.len(),
        original.len(),
        "a segment no longer stored is not added"
    );
    for (stored, original) in stored.iter().zip(&original) {
        assert_eq!(stored.text, format!("{} (cleaned)", original.raw_text));
        assert_eq!(stored.speaker_id, original.speaker_id);
        assert_eq!(stored.raw_text, original.raw_text);
    }
}

/// Without a summarizer the summarize stage writes only the processing
/// columns: the summary, tasks and decisions stay.
#[test]
fn saving_processing_results_keeps_the_summary_tasks_and_decisions() {
    let (store, meeting) = populated();
    let tasks = store.tasks(meeting.id).unwrap();
    let decisions = store.decisions(meeting.id).unwrap();
    let mut results = meeting.clone();
    results.state = MeetingState::Ready;
    results.updated_at = date("2026-09-30T09:00:00.000Z");
    store.save_processing_results(&results).unwrap();
    let read = store.meeting(meeting.id).unwrap().unwrap();
    assert_eq!(read.summary, meeting.summary);
    assert_eq!(read.updated_at, results.updated_at);
    assert_eq!(store.tasks(meeting.id).unwrap(), tasks);
    assert_eq!(store.decisions(meeting.id).unwrap(), decisions);
}

/// A stage that read the meeting before the user picked another template
/// or typed a title does not write the old ones back: the template is the
/// user's, and so is a title whose stored origin is `user`.
#[test]
fn processing_results_leave_the_template_and_a_typed_title_to_the_user() {
    let (store, meeting) = populated();
    let now = date("2026-09-30T09:00:00.000Z");
    store
        .update_meeting(meeting.id, now, |stored| {
            "interview".clone_into(&mut stored.template_id);
            "Typed by the user".clone_into(&mut stored.title);
            stored.title_origin = TitleOrigin::User;
            Ok(())
        })
        .unwrap();
    let mut stale = meeting.clone();
    assert_eq!(stale.template_id, Meeting::DEFAULT_TEMPLATE_ID);
    assert_ne!(stale.title_origin, TitleOrigin::User);
    "The model's title".clone_into(&mut stale.title);
    stale.title_origin = TitleOrigin::Summary;
    store.replace_transcript(&stale, &[], &[]).unwrap();
    store.update_segment_texts(&stale, &[]).unwrap();
    store.save_processing_results(&stale).unwrap();
    store.replace_summary(&stale, &[], &[], &[]).unwrap();
    let stored = store.meeting(meeting.id).unwrap().unwrap();
    assert_eq!(stored.template_id, "interview");
    assert_eq!(stored.title, "Typed by the user");
    assert_eq!(stored.title_origin, TitleOrigin::User);
}

/// A checkpoint syncs with `F_FULLFSYNC` on Apple platforms, as Apple's
/// system SQLite under GRDB does by default, so no checkpoint can undo a
/// durable commit; the bundled SQLite defaults it off.
#[test]
fn an_opened_store_checkpoints_with_fullfsync() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path().join("steno.sqlite")).unwrap();
    let checkpoint_fullfsync: bool = store
        .read(|connection| {
            Ok(connection.query_row("PRAGMA checkpoint_fullfsync", [], |row| row.get(0))?)
        })
        .unwrap();
    assert!(checkpoint_fullfsync);
}

/// `synchronous` and `fullfsync` as the connection has them now.
fn sync_levels(connection: &rusqlite::Connection) -> steno_core::store::Result<(i64, bool)> {
    let synchronous = connection.query_row("PRAGMA synchronous", [], |row| row.get(0))?;
    let fullfsync = connection.query_row("PRAGMA fullfsync", [], |row| row.get(0))?;
    Ok((synchronous, fullfsync))
}

/// A durable write commits under `FULL` with `fullfsync` (2, on), and the
/// connection is back at `NORMAL` (1, off) after a commit, a failed body and a
/// panic in the body; a plain write never sees `FULL`. That the commit then
/// survives a power loss is SQLite's and cannot be tested. Swift:
/// `aDurableWriteCommitsUnderFullAndSetsNormalBack`.
#[test]
fn a_durable_write_commits_under_full_and_sets_normal_back_on_every_path() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path().join("steno.sqlite")).unwrap();
    let normal = (1, false);
    assert_eq!(store.read(sync_levels).unwrap(), normal);

    let inside = store
        .write_durably(|transaction| {
            // A change of a page, so the commit writes a frame to the WAL.
            transaction.execute_batch("CREATE TABLE probe(x)")?;
            sync_levels(transaction)
        })
        .unwrap();
    assert_eq!(inside, (2, true), "the transaction runs under FULL");
    assert_eq!(store.read(sync_levels).unwrap(), normal, "after a commit");
    assert_eq!(
        store.write(|transaction| sync_levels(transaction)).unwrap(),
        normal,
        "a plain write stays NORMAL"
    );

    let missing = uuid(MEETING_ID);
    let error = store
        .write_durably(|_| -> steno_core::store::Result<()> {
            Err(StoreError::MeetingNotFound(missing))
        })
        .unwrap_err();
    assert!(matches!(error, StoreError::MeetingNotFound(id) if id == missing));
    assert_eq!(store.read(sync_levels).unwrap(), normal, "after a failure");

    let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        store.write_durably(|_| -> steno_core::store::Result<()> { panic!("in the body") })
    }));
    assert!(panicked.is_err());
    assert_eq!(store.read(sync_levels).unwrap(), normal, "after a panic");
    assert_eq!(
        store.write(|transaction| sync_levels(transaction)).unwrap(),
        normal,
        "the lock the panic poisoned is reused at NORMAL"
    );
}

/// A durable checkpoint leaves every commit in the database file itself:
/// a copy of that file without its WAL holds the last commit. It runs
/// under `FULL` and sets `NORMAL` back, like a durable write. Swift:
/// `aDurableCheckpointCopiesEveryCommitIntoTheDatabaseFile`.
#[test]
fn a_durable_checkpoint_copies_every_commit_into_the_database_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("steno.sqlite");
    let store = Store::open(&path).unwrap();
    store
        .write(|transaction| {
            transaction.execute_batch("CREATE TABLE probe(x); INSERT INTO probe VALUES (42);")?;
            Ok(())
        })
        .unwrap();

    store.checkpoint_durably().unwrap();
    assert_eq!(store.read(sync_levels).unwrap(), (1, false));

    let copy = dir.path().join("copy.sqlite");
    std::fs::copy(&path, &copy).unwrap();
    let probed: i64 = rusqlite::Connection::open(&copy)
        .unwrap()
        .query_row("SELECT x FROM probe", [], |row| row.get(0))
        .unwrap();
    assert_eq!(probed, 42);
}

/// A checkpoint another connection blocks past the busy timeout (here
/// none, so the test does not wait) fails as busy, as GRDB's does, and
/// sets `NORMAL` back; once the other connection lets go it succeeds.
/// Swift: `aCheckpointAnotherConnectionBlocksThrowsBusy`.
#[test]
fn a_checkpoint_another_connection_blocks_fails_as_busy() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("steno.sqlite");
    let store = Store::open(&path).unwrap();
    store
        .read(|connection| Ok(connection.busy_timeout(std::time::Duration::ZERO)?))
        .unwrap();
    let writer = rusqlite::Connection::open(&path).unwrap();
    writer.execute_batch("BEGIN IMMEDIATE").unwrap();

    let error = store.checkpoint_durably().unwrap_err();
    assert!(error.is_busy(), "{error}");
    assert_eq!(store.read(sync_levels).unwrap(), (1, false));

    writer.execute_batch("ROLLBACK").unwrap();
    store.checkpoint_durably().unwrap();
}

/// The WAL file of the database at `database`.
fn wal_file(database: &std::path::Path) -> Vec<u8> {
    let mut wal = database.as_os_str().to_owned();
    wal.push("-wal");
    std::fs::read(wal).unwrap()
}

/// The salt in the WAL file's header (bytes 16 to 24), which every valid
/// frame repeats: recovery replays only frames under the header's salt.
fn wal_salt(database: &std::path::Path) -> Vec<u8> {
    wal_file(database)
        .get(16..24)
        .expect("the WAL file has a header")
        .to_vec()
}

/// A durable checkpoint starts the WAL over: the file was truncated, so it
/// holds only the frames written since, under a header with a new salt, and
/// recovery after a power loss replays none of the frames before it. The
/// write that restarts it leaves the schema and the applied migrations as
/// they were. Swift: `aDurableCheckpointRestartsTheWAL`.
#[test]
fn a_durable_checkpoint_restarts_the_wal() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("steno.sqlite");
    let store = Store::open(&path).unwrap();
    store
        .write(|transaction| {
            transaction.execute_batch("CREATE TABLE probe(x); INSERT INTO probe VALUES (42);")?;
            Ok(())
        })
        .unwrap();
    let salt = wal_salt(&path);
    let schema = store.schema_dump().unwrap();

    store.checkpoint_durably().unwrap();

    assert_ne!(wal_salt(&path), salt, "the WAL restarted");
    // A passive checkpoint answers with the frames in the WAL and leaves
    // the file as it is.
    let (frames, page_size): (i64, i64) = store
        .read(|connection| {
            let frames =
                connection.query_row("PRAGMA wal_checkpoint(PASSIVE)", [], |row| row.get(1))?;
            let page_size = connection.query_row("PRAGMA page_size", [], |row| row.get(0))?;
            Ok((frames, page_size))
        })
        .unwrap();
    assert_eq!(
        i64::try_from(wal_file(&path).len()).unwrap(),
        32 + frames * (24 + page_size),
        "the WAL file holds the header and the write's frames only: it was truncated first"
    );
    assert_eq!(store.schema_dump().unwrap(), schema);
    assert_eq!(store.read(sync_levels).unwrap(), (1, false));
}

/// A reader still in the WAL blocks the checkpoint, on an older snapshot
/// and on the newest one alike: the WAL cannot restart under it. The
/// checkpoint fails as busy (no busy timeout here, so the test does not
/// wait) and succeeds once the reader has ended. Swift:
/// `aReaderInTheWALBlocksTheCheckpoint`.
#[test]
fn a_reader_in_the_wal_blocks_the_checkpoint() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("steno.sqlite");
    let store = Store::open(&path).unwrap();
    store
        .read(|connection| Ok(connection.busy_timeout(std::time::Duration::ZERO)?))
        .unwrap();
    let insert = |value: i64| {
        store
            .write(|transaction| {
                transaction.execute("INSERT INTO probe VALUES (?1)", [value])?;
                Ok(())
            })
            .unwrap();
    };
    store
        .write(|transaction| Ok(transaction.execute_batch("CREATE TABLE probe(x)")?))
        .unwrap();
    let reader = rusqlite::Connection::open(&path).unwrap();
    let begin_reading = || {
        reader.execute_batch("BEGIN").unwrap();
        let _: i64 = reader
            .query_row("SELECT count(*) FROM setting", [], |row| row.get(0))
            .unwrap();
    };
    insert(1);

    begin_reading();
    insert(2);
    let error = store.checkpoint_durably().unwrap_err();
    assert!(error.is_busy(), "older snapshot: {error}");
    reader.execute_batch("COMMIT").unwrap();

    insert(3);
    begin_reading();
    let error = store.checkpoint_durably().unwrap_err();
    assert!(error.is_busy(), "newest snapshot: {error}");
    reader.execute_batch("COMMIT").unwrap();

    store.checkpoint_durably().unwrap();
}
