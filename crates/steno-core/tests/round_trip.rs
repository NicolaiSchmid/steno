//! Byte-level compatibility with GRDB: what the Rust store writes reads as
//! GRDB's encodings, and what GRDB wrote reads back as the same values.

mod common;

use std::process::Command;

use rusqlite::Connection;
use steno_core::*;

use common::{MEETING_ID, PERSON_ID, date, populate, uuid};

/// Raw column text through `sqlite3` when the CLI is available (the
/// independent reader), else through a second rusqlite connection.
fn raw(path: &std::path::Path, sql: &str) -> String {
    match Command::new("sqlite3")
        .arg("-batch")
        .arg("-noheader")
        .arg(path)
        .arg(sql)
        .output()
    {
        Ok(output) if output.status.success() => String::from_utf8(output.stdout)
            .unwrap()
            .trim_end()
            .replace("\r\n", "\n"),
        Ok(output) => panic!(
            "sqlite3 failed: {}",
            String::from_utf8_lossy(&output.stderr)
        ),
        Err(error) => {
            eprintln!("sqlite3 CLI not available ({error}); reading raw columns through rusqlite");
            let connection = Connection::open(path).unwrap();
            let mut statement = connection.prepare(sql).unwrap();
            let columns = statement.column_count();
            let rows = statement
                .query_map([], |row| {
                    (0..columns)
                        .map(|index| {
                            row.get::<_, rusqlite::types::Value>(index)
                                .map(|value| match value {
                                    rusqlite::types::Value::Null => String::new(),
                                    rusqlite::types::Value::Integer(i) => i.to_string(),
                                    rusqlite::types::Value::Real(f) if f.fract() == 0.0 => {
                                        format!("{f:.1}")
                                    }
                                    rusqlite::types::Value::Real(f) => f.to_string(),
                                    rusqlite::types::Value::Text(t) => t,
                                    rusqlite::types::Value::Blob(b) => {
                                        format!("<{} bytes>", b.len())
                                    }
                                })
                        })
                        .collect::<rusqlite::Result<Vec<_>>>()
                        .map(|cells| cells.join("|"))
                })
                .unwrap();
            rows.collect::<rusqlite::Result<Vec<_>>>()
                .unwrap()
                .join("\n")
        }
    }
}

#[test]
fn rows_the_rust_store_writes_use_grdb_encodings() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("steno.sqlite");
    let store = Store::open(&path).unwrap();
    let meeting = populate(&store);
    drop(store);

    assert_eq!(
        raw(
            &path,
            "SELECT id, startedAt, createdAt, typeof(startedAt), tags, endReason, llmUsage, titleOrigin, state, failureReason IS NULL, language, summaryText FROM meeting"
        ),
        format!(
            "{MEETING_ID}|2026-09-29 13:49:11.135|2026-09-29 14:29:23.685|text|[\"alpha\",\"beta/gamma\"]|{{\"callEnded\":\"Zen\"}}|{{\"completionTokens\":30,\"promptTokens\":120,\"requests\":2}}|default|ready|1|de|Ziel: Den Store portieren\nOhne Migration"
        )
    );
    assert_eq!(
        raw(&path, "SELECT substr(summary, 1, 60) FROM meeting"),
        r#"{"language":"de","sections":[{"bullets":[{"lead":"Ziel","tex"#
    );
    assert_eq!(
        raw(
            &path,
            "SELECT done, typeof(done), dueDate, priority, assigneePersonID FROM meetingTask ORDER BY done DESC"
        ),
        format!("1|integer|2026-10-03 00:00:00.000|high|{PERSON_ID}\n0|integer||normal|")
    );
    assert_eq!(
        raw(
            &path,
            "SELECT lanes, sidecars16k, retention, retentionDays, expiresAt FROM audioAsset"
        ),
        format!(
            "[\"mic\",\"system\"]|{{\"mic\":\"file:///tmp/Audio/{MEETING_ID}/mic.wav\",\"system\":\"file:///tmp/Audio/{MEETING_ID}/system.wav\"}}|keepDays|30|2026-10-29 14:33:05.904"
        )
    );
    assert_eq!(
        raw(
            &path,
            "SELECT assignment, personID, round(similarity, 6), sampleClipStart, sampleClipEnd, length(embedding), round(clusterConfidence, 6) FROM speaker ORDER BY clusterLabel"
        ),
        format!("suggested|{PERSON_ID}|0.7|12.0|22.0|1024|0.9\nunknown||||||0.6")
    );
    assert_eq!(
        raw(&path, "SELECT status, lastAttemptAt, receipt FROM delivery"),
        r#"delivered|2026-09-29 14:34:00.000|{"files":[{"ownership":"owned","relativePath":"note.md","sha256":"AQID"}],"folder":"Meetings/2026-09-29","renderer_version":3,"root":"/vault"}"#
            .replace("renderer_version", "rendererVersion")
    );
    assert_eq!(
        raw(&path, "SELECT length(embedding), sampleCount FROM person"),
        "1024|1"
    );
    assert_eq!(
        raw(
            &path,
            "SELECT count(*) FROM transcriptSegment_ft WHERE transcriptSegment_ft MATCH 'hallo'"
        ),
        "1",
        "the FTS triggers index what the Rust store inserts"
    );
    assert_eq!(
        raw(
            &path,
            "SELECT rowid FROM meeting_ft WHERE meeting_ft MATCH 'portieren'"
        ),
        "1"
    );
    assert_eq!(meeting.id, uuid(MEETING_ID));
}

#[test]
fn rows_grdb_wrote_read_back_as_values() {
    let store = Store::in_memory().unwrap();
    let meeting = common::meeting();
    store
        .write(|transaction| {
            transaction.execute_batch(&format!(
                r#"INSERT INTO person (id, displayName, email, embedding, sampleCount, createdAt)
                   VALUES ('{PERSON_ID}', 'Jérôme', NULL, X'0000803F', 0, '2026-09-01 08:00:00.000');
                   INSERT INTO meeting (id, title, startedAt, duration, language, source, calendarEventID, tags, state, failureReason, templateID, summary, summaryText, scratchpad, llmUsage, createdAt, updatedAt, endReason, titleOrigin)
                   VALUES ('{MEETING_ID}', 'Call', '2026-09-29 13:49:11.135', 1234.5, 'de', 'macCall', 'cal-1', '["alpha","beta/gamma"]', 'failed', 'disk full', 'default', NULL, '', '', '{{"completionTokens":30,"promptTokens":120,"requests":2}}', '2026-09-29 14:29:23.685', '2026-09-29 14:33:05.904', '"manual"', 'user');
                   INSERT INTO speaker (id, meetingID, clusterLabel, assignment, personID, similarity, embedding, sampleClipStart, sampleClipEnd, sampleClipURL, clusterConfidence)
                   VALUES ('6091570E-A744-46C5-BD1E-ECCB841949C5', '{MEETING_ID}', 'Speaker 1', 'confirmed', '{PERSON_ID}', NULL, NULL, 1.0, 3.0, 'file:///tmp/clip.wav', 0.800000011920929);
                   INSERT INTO transcriptSegment (id, meetingID, start, end, speakerID, lane, text, rawText)
                   VALUES ('7091570E-A744-46C5-BD1E-ECCB841949C5', '{MEETING_ID}', 0.0, 1.5, '6091570E-A744-46C5-BD1E-ECCB841949C5', 'mixed', 'Hallo', 'hallo');
                   INSERT INTO meetingTask (id, meetingID, text, assigneePersonID, assigneeName, priority, dueDate, done)
                   VALUES ('8091570E-A744-46C5-BD1E-ECCB841949C5', '{MEETING_ID}', 'Do it', NULL, NULL, 'low', NULL, 1);
                   INSERT INTO audioAsset (id, meetingID, url, format, lanes, sidecars16k, mixdownURL, retention, retentionDays, expiresAt)
                   VALUES ('9091570E-A744-46C5-BD1E-ECCB841949C5', '{MEETING_ID}', 'file:///tmp/master.caf', 'caf48kFloat32', '["mic","system"]', '{{}}', NULL, 'keepForever', NULL, NULL);
                   INSERT INTO setting (key, value) VALUES ('defaultRetention', '{{"keepDays":30}}');
                   INSERT INTO setting (key, value) VALUES ('llmModel', '"gpt"');
                   INSERT INTO setting (key, value) VALUES ('futureKnob', '42');"#
            ))?;
            Ok(())
        })
        .unwrap();

    let read = store.meeting(meeting.id).unwrap().unwrap();
    assert_eq!(read.id, meeting.id);
    assert_eq!(read.started_at, meeting.started_at);
    assert_eq!(read.created_at, date("2026-09-29T14:29:23.685Z"));
    assert_eq!(read.tags, meeting.tags);
    assert_eq!(
        read.state,
        MeetingState::Failed {
            reason: "disk full".to_owned()
        }
    );
    assert_eq!(read.end_reason, Some(RecordingEndReason::Manual));
    assert_eq!(read.title_origin, TitleOrigin::User);
    assert_eq!(read.calendar_event_id.as_deref(), Some("cal-1"));
    assert_eq!(read.llm_usage, meeting.llm_usage);
    assert_eq!(read.summary, None);
    assert_eq!(read.language, Some("de".into()));

    let speakers = store.speakers(meeting.id).unwrap();
    assert_eq!(speakers.len(), 1);
    assert_eq!(
        speakers[0].assignment,
        SpeakerAssignment::Confirmed {
            person_id: uuid(PERSON_ID)
        }
    );
    assert_eq!(
        speakers[0].sample_clip_range,
        Some(TimeRange {
            lower: 1.0,
            upper: 3.0
        })
    );
    assert_eq!(speakers[0].cluster_confidence, 0.8);
    let segments = store.segments(meeting.id).unwrap();
    assert_eq!(segments[0].speaker_id, Some(speakers[0].id));
    assert_eq!(segments[0].lane, AudioLane::Mixed);
    let tasks = store.tasks(meeting.id).unwrap();
    assert!(tasks[0].done);
    assert_eq!(tasks[0].priority, TaskPriority::Low);
    assert_eq!(
        store.asset(meeting.id).unwrap().unwrap().retention,
        AudioRetention::KeepForever
    );
    assert_eq!(
        store.person(uuid(PERSON_ID)).unwrap().unwrap().embedding,
        Some(Embedding(vec![1.0]))
    );

    let defaults = Settings::defaults(&StenoPaths::new("/support"));
    let settings = store.settings_with_defaults(&defaults).unwrap();
    assert_eq!(
        settings.default_retention,
        AudioRetention::KeepDays(30),
        "a stored row wins"
    );
    assert_eq!(settings.llm_model.as_deref(), Some("gpt"));
    assert_eq!(
        settings.llm_context_tokens, 32_000,
        "a missing row is the default"
    );
    assert_eq!(settings.audio_folder, "file:///support/Audio/");
}
