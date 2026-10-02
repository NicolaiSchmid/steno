use chrono::{DateTime, Utc};
use rusqlite::{Connection, OptionalExtension, Row, params, params_from_iter};
use uuid::Uuid;

use super::convert::{DbDate, DbEnum, DbJson, DbUuid, Unwrap as _};
use super::{Result, Store, StoreError, assets, people, tasks, transcript};
use crate::model::{
    AudioAsset, Decision, LanguageTag, Meeting, MeetingState, MeetingStateKind, MeetingTask,
    Participant, Speaker, SpeakerNameSuggestion, TranscriptSegment, derived_uuid,
};

const COLUMNS: &str = "id, title, startedAt, duration, language, source, calendarEventID, tags, \
     state, failureReason, templateID, summary, summaryText, scratchpad, llmUsage, createdAt, \
     updatedAt, endReason, titleOrigin";

pub(super) fn from_row(row: &Row<'_>) -> rusqlite::Result<Meeting> {
    let kind: DbEnum<MeetingStateKind> = row.get("state")?;
    let failure_reason: Option<String> = row.get("failureReason")?;
    Ok(Meeting {
        id: row.get::<_, DbUuid>("id")?.0,
        title: row.get("title")?,
        started_at: row.get::<_, DbDate>("startedAt")?.0,
        duration: row.get("duration")?,
        language: row.get::<_, Option<String>>("language")?.map(Into::into),
        source: row.get::<_, DbEnum<_>>("source")?.0,
        calendar_event_id: row.get("calendarEventID")?,
        tags: row.get::<_, DbJson<Vec<String>>>("tags")?.0,
        state: MeetingState::from_columns(kind.0, failure_reason),
        end_reason: row.get::<_, Option<DbJson<_>>>("endReason")?.unwrap_db(),
        title_origin: row.get::<_, DbEnum<_>>("titleOrigin")?.0,
        template_id: row.get("templateID")?,
        summary: row.get::<_, Option<DbJson<_>>>("summary")?.unwrap_db(),
        scratchpad: row.get("scratchpad")?,
        llm_usage: row.get::<_, Option<DbJson<_>>>("llmUsage")?.unwrap_db(),
        created_at: row.get::<_, DbDate>("createdAt")?.0,
        updated_at: row.get::<_, DbDate>("updatedAt")?.0,
    })
}

/// GRDB's `save`: the row is replaced in place when it exists (the FTS
/// update trigger fires), inserted otherwise.
pub(super) fn save(connection: &Connection, meeting: &Meeting) -> Result<()> {
    connection.execute(
        "INSERT INTO meeting (id, title, startedAt, duration, language, source, calendarEventID, \
         tags, state, failureReason, templateID, summary, summaryText, scratchpad, llmUsage, \
         createdAt, updatedAt, endReason, titleOrigin) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19) \
         ON CONFLICT(id) DO UPDATE SET title = excluded.title, startedAt = excluded.startedAt, \
         duration = excluded.duration, language = excluded.language, source = excluded.source, \
         calendarEventID = excluded.calendarEventID, tags = excluded.tags, state = excluded.state, \
         failureReason = excluded.failureReason, templateID = excluded.templateID, \
         summary = excluded.summary, summaryText = excluded.summaryText, \
         scratchpad = excluded.scratchpad, llmUsage = excluded.llmUsage, \
         createdAt = excluded.createdAt, updatedAt = excluded.updatedAt, \
         endReason = excluded.endReason, titleOrigin = excluded.titleOrigin",
        params![
            DbUuid(meeting.id),
            meeting.title,
            DbDate(meeting.started_at),
            meeting.duration,
            meeting.language.as_ref().map(LanguageTag::as_str),
            DbEnum(meeting.source),
            meeting.calendar_event_id,
            DbJson(&meeting.tags),
            DbEnum(meeting.state.kind()),
            meeting.state.failure_reason(),
            meeting.template_id,
            meeting.summary.as_ref().map(DbJson),
            meeting.summary.as_ref().map(SummaryDocumentExt::plain_text).unwrap_or_default(),
            meeting.scratchpad,
            meeting.llm_usage.as_ref().map(DbJson),
            DbDate(meeting.created_at),
            DbDate(meeting.updated_at),
            meeting.end_reason.as_ref().map(DbJson),
            DbEnum(meeting.title_origin),
        ],
    )?;
    Ok(())
}

trait SummaryDocumentExt {
    fn plain_text(&self) -> String;
}

impl SummaryDocumentExt for crate::model::SummaryDocument {
    fn plain_text(&self) -> String {
        crate::model::SummaryDocument::plain_text(self)
    }
}

pub(super) fn fetch(connection: &Connection, id: Uuid) -> Result<Option<Meeting>> {
    Ok(connection
        .query_row(
            &format!("SELECT {COLUMNS} FROM meeting WHERE id = ?1"),
            [DbUuid(id)],
            from_row,
        )
        .optional()?)
}

pub(super) fn current(connection: &Connection, id: Uuid) -> Result<Meeting> {
    fetch(connection, id)?.ok_or(StoreError::MeetingNotFound(id))
}

/// Reads the row and overlays `results`' processing columns, so a stage
/// that started minutes ago never writes back the scratchpad or tags it
/// read then.
fn write_processing_results(connection: &Connection, results: &Meeting) -> Result<()> {
    let mut meeting = current(connection, results.id)?;
    meeting.apply_processing_results(results);
    save(connection, &meeting)
}

/// What `delete_meeting` leaves for the caller: the files the rows pointed
/// at, which the caller removes once the transaction has committed (the
/// folder-versus-files rule lives with the recording layout, WP6).
#[derive(Debug, Clone, PartialEq)]
pub struct DeletedMeeting {
    pub assets: Vec<AudioAsset>,
    /// The speakers' sample clip URLs.
    pub clips: Vec<String>,
}

impl Store {
    pub fn save_meeting(&self, meeting: &Meeting) -> Result<()> {
        self.write(|transaction| save(transaction, meeting))
    }

    /// The meeting and its asset in one transaction (`enqueue`).
    pub fn save_meeting_with_asset(&self, meeting: &Meeting, asset: &AudioAsset) -> Result<()> {
        self.write(|transaction| {
            save(transaction, meeting)?;
            assets::save(transaction, asset)
        })
    }

    /// The meeting and its participants in one transaction (recording
    /// start); every participant is re-pointed at the meeting.
    pub fn save_meeting_with_participants(
        &self,
        meeting: &Meeting,
        participants: &[Participant],
    ) -> Result<()> {
        self.write(|transaction| {
            save(transaction, meeting)?;
            for participant in participants {
                let mut participant = participant.clone();
                participant.meeting_id = meeting.id;
                people::save_participant(transaction, &participant)?;
            }
            Ok(())
        })
    }

    pub fn meeting(&self, id: Uuid) -> Result<Option<Meeting>> {
        self.read(|connection| fetch(connection, id))
    }

    /// Newest first by `startedAt`.
    pub fn meetings(&self, limit: i64, offset: i64) -> Result<Vec<Meeting>> {
        self.read(|connection| {
            let mut statement = connection.prepare(&format!(
                "SELECT {COLUMNS} FROM meeting ORDER BY startedAt DESC, id LIMIT ?1 OFFSET ?2"
            ))?;
            let rows = statement.query_map(params![limit, offset], from_row)?;
            Ok(rows.collect::<rusqlite::Result<_>>()?)
        })
    }

    /// Every meeting in one of `kinds`, oldest first by `startedAt`: the
    /// order a queue is worked off in.
    pub fn meetings_in_states(&self, kinds: &[MeetingStateKind]) -> Result<Vec<Meeting>> {
        if kinds.is_empty() {
            return Ok(Vec::new());
        }
        self.read(|connection| {
            let placeholders = vec!["?"; kinds.len()].join(", ");
            let mut statement = connection.prepare(&format!(
                "SELECT {COLUMNS} FROM meeting WHERE state IN ({placeholders}) \
                 ORDER BY startedAt, id"
            ))?;
            let rows = statement.query_map(
                params_from_iter(kinds.iter().map(|kind| kind.as_str())),
                from_row,
            )?;
            Ok(rows.collect::<rusqlite::Result<_>>()?)
        })
    }

    /// Launch reconciliation: every meeting still `recording` belongs to a
    /// process that died mid-meeting. One transaction marks them
    /// `failed(reason)` with `updatedAt = now` and returns their ids,
    /// oldest first.
    pub fn fail_interrupted_recordings(
        &self,
        reason: &str,
        now: DateTime<Utc>,
    ) -> Result<Vec<Uuid>> {
        self.write(|transaction| {
            let rows: Vec<Meeting> = {
                let mut statement = transaction.prepare(&format!(
                    "SELECT {COLUMNS} FROM meeting WHERE state = ?1 ORDER BY startedAt, id"
                ))?;
                let rows = statement.query_map([MeetingStateKind::Recording.as_str()], from_row)?;
                rows.collect::<rusqlite::Result<_>>()?
            };
            for mut meeting in rows.iter().cloned() {
                meeting.state = MeetingState::Failed {
                    reason: reason.to_owned(),
                };
                meeting.updated_at = now;
                save(transaction, &meeting)?;
            }
            Ok(rows.into_iter().map(|meeting| meeting.id).collect())
        })
    }

    /// Read-modify-write in one transaction: `mutate` sees the row as it is
    /// now, not as a caller read it earlier. `updatedAt` is set to `now`.
    /// Returns the meeting as written.
    pub fn update_meeting(
        &self,
        id: Uuid,
        now: DateTime<Utc>,
        mutate: impl FnOnce(&mut Meeting) -> Result<()>,
    ) -> Result<Meeting> {
        self.write(|transaction| {
            let mut meeting = current(transaction, id)?;
            mutate(&mut meeting)?;
            meeting.updated_at = now;
            save(transaction, &meeting)?;
            Ok(meeting)
        })
    }

    pub fn set_state(&self, state: MeetingState, id: Uuid, now: DateTime<Utc>) -> Result<()> {
        self.update_meeting(id, now, |meeting| {
            meeting.state = state;
            Ok(())
        })
        .map(drop)
    }

    /// The user's rename: the trimmed title with `titleOrigin = user`. A
    /// blank title leaves the row alone.
    pub fn rename(&self, id: Uuid, title: &str, now: DateTime<Utc>) -> Result<()> {
        let trimmed = title.trim();
        if trimmed.is_empty() {
            return Ok(());
        }
        self.update_meeting(id, now, |meeting| {
            trimmed.clone_into(&mut meeting.title);
            meeting.title_origin = crate::model::TitleOrigin::User;
            Ok(())
        })
        .map(drop)
    }

    /// One transaction: the meeting's processing columns plus every speaker
    /// and segment of the meeting, replaced. The merge and cleanup stages
    /// call this.
    pub fn replace_transcript(
        &self,
        meeting: &Meeting,
        segments: &[TranscriptSegment],
        speakers: &[Speaker],
    ) -> Result<()> {
        self.write(|transaction| {
            write_processing_results(transaction, meeting)?;
            transaction.execute(
                "DELETE FROM transcriptSegment WHERE meetingID = ?1",
                [DbUuid(meeting.id)],
            )?;
            transaction.execute(
                "DELETE FROM speaker WHERE meetingID = ?1",
                [DbUuid(meeting.id)],
            )?;
            for speaker in speakers {
                let mut speaker = speaker.clone();
                speaker.meeting_id = meeting.id;
                people::insert_speaker(transaction, &speaker)?;
            }
            for segment in segments {
                let mut segment = segment.clone();
                segment.meeting_id = meeting.id;
                transcript::insert_segment(transaction, &segment)?;
            }
            Ok(())
        })
    }

    /// One transaction: the meeting's processing columns plus its tasks,
    /// decisions and speaker name suggestions, replaced. Decision ids derive
    /// from the meeting id so re-runs are stable. Of `speaker_names`, only
    /// suggestions that carry a name and point at one of the meeting's
    /// speakers are kept, the strongest per speaker.
    pub fn replace_summary(
        &self,
        meeting: &Meeting,
        tasks: &[MeetingTask],
        decisions: &[String],
        speaker_names: &[SpeakerNameSuggestion],
    ) -> Result<()> {
        self.write(|transaction| {
            write_processing_results(transaction, meeting)?;
            people::replace_name_suggestions(transaction, meeting.id, speaker_names)?;
            transaction.execute(
                "DELETE FROM meetingTask WHERE meetingID = ?1",
                [DbUuid(meeting.id)],
            )?;
            for task in tasks {
                let mut task = task.clone();
                task.meeting_id = meeting.id;
                tasks::insert_task(transaction, &task)?;
            }
            transaction.execute(
                "DELETE FROM decision WHERE meetingID = ?1",
                [DbUuid(meeting.id)],
            )?;
            for (index, text) in decisions.iter().enumerate() {
                let decision = Decision {
                    id: derived_uuid(meeting.id, &format!("decision-{index}")),
                    meeting_id: meeting.id,
                    text: text.clone(),
                };
                tasks::insert_decision(transaction, &decision)?;
            }
            Ok(())
        })
    }

    /// Removes the meeting and everything that hangs off it through the
    /// cascades (participants, speakers, segments, tasks, decisions, assets,
    /// deliveries, their FTS rows through the triggers) and the handover
    /// receipt that admitted it. Persons stay. Fails with `MeetingBusy`
    /// while the meeting is `recording` or `processing`. Returns the files
    /// the rows named, for the caller to remove.
    pub fn delete_meeting(&self, id: Uuid) -> Result<DeletedMeeting> {
        self.write(|transaction| {
            let meeting = current(transaction, id)?;
            match meeting.state.kind() {
                kind @ (MeetingStateKind::Recording | MeetingStateKind::Processing) => {
                    return Err(StoreError::MeetingBusy(id, kind));
                }
                MeetingStateKind::Queued | MeetingStateKind::Ready | MeetingStateKind::Failed => {}
            }
            let assets = assets::for_meeting(transaction, id)?;
            let clips = people::speakers(transaction, id)?
                .into_iter()
                .filter_map(|speaker| speaker.sample_clip_url)
                .collect();
            transaction.execute(
                "DELETE FROM handoverReceipt WHERE meetingID = ?1",
                [DbUuid(id)],
            )?;
            transaction.execute("DELETE FROM meeting WHERE id = ?1", [DbUuid(id)])?;
            Ok(DeletedMeeting { assets, clips })
        })
    }
}
