//! `meeting` rows and the transactions that write a meeting together with
//! what hangs off it.
//! Swift: `Sources/StenoCore/Storage/MeetingStore.swift`.

use chrono::{DateTime, Utc};
use rusqlite::{Connection, OptionalExtension, Row, params, params_from_iter};
use uuid::Uuid;

use super::convert::{DbDate, DbEnum, DbJson, DbUuid, RowExt as _};
use super::{Result, Store, StoreError, assets, people, query_all, tasks, transcript, upsert_sql};
use crate::model::{
    AudioAsset, Decision, LanguageTag, Meeting, MeetingState, MeetingStateKind, MeetingTask,
    Participant, Speaker, SpeakerNameSuggestion, SummaryDocument, TitleOrigin, TranscriptSegment,
    derived_uuid,
};

const COLUMNS: &str = "id, title, startedAt, duration, language, source, calendarEventID, tags, \
     state, failureReason, templateID, summary, summaryText, scratchpad, llmUsage, createdAt, \
     updatedAt, endReason, titleOrigin";

pub(super) fn from_row(row: &Row<'_>) -> rusqlite::Result<Meeting> {
    let kind = row.col::<DbEnum<_>>("state")?;
    let failure_reason = row.get("failureReason")?;
    Ok(Meeting {
        id: row.col::<DbUuid>("id")?,
        title: row.get("title")?,
        started_at: row.col::<DbDate>("startedAt")?,
        duration: row.get("duration")?,
        language: row.get::<_, Option<String>>("language")?.map(Into::into),
        source: row.col::<DbEnum<_>>("source")?,
        calendar_event_id: row.get("calendarEventID")?,
        tags: row.col::<DbJson<_>>("tags")?,
        state: MeetingState::from_columns(kind, failure_reason),
        end_reason: row.col::<Option<DbJson<_>>>("endReason")?,
        title_origin: row.col::<DbEnum<_>>("titleOrigin")?,
        template_id: row.get("templateID")?,
        summary: row.col::<Option<DbJson<_>>>("summary")?,
        scratchpad: row.get("scratchpad")?,
        llm_usage: row.col::<Option<DbJson<_>>>("llmUsage")?,
        created_at: row.col::<DbDate>("createdAt")?,
        updated_at: row.col::<DbDate>("updatedAt")?,
    })
}

/// GRDB's `save`; `summaryText` is derived from `summary` on every write.
pub(super) fn save(connection: &Connection, meeting: &Meeting) -> Result<()> {
    connection.execute(
        &upsert_sql("meeting", COLUMNS),
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
            meeting
                .summary
                .as_ref()
                .map(SummaryDocument::plain_text)
                .unwrap_or_default(),
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

pub(super) fn fetch(connection: &Connection, id: Uuid) -> Result<Option<Meeting>> {
    Ok(connection
        .query_row(
            &format!("SELECT {COLUMNS} FROM meeting WHERE id = ?1"),
            [DbUuid(id)],
            from_row,
        )
        .optional()?)
}

/// The row as it is now, for a read-modify-write inside a transaction;
/// `MeetingNotFound` when there is none, since every caller needs one.
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
/// folder-versus-files rule lives with the recording layout).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeletedMeeting {
    pub assets: Vec<AudioAsset>,
    /// The speakers' sample clip URLs.
    pub clips: Vec<String>,
}

impl Store {
    /// Inserts or replaces the meeting row (GRDB's `save`); `summaryText`
    /// is derived from `summary`.
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

    /// The meeting with `id`.
    pub fn meeting(&self, id: Uuid) -> Result<Option<Meeting>> {
        self.read(|connection| fetch(connection, id))
    }

    /// Newest first by `startedAt`.
    pub fn meetings(&self, limit: i64, offset: i64) -> Result<Vec<Meeting>> {
        self.read(|connection| {
            query_all(
                connection,
                &format!(
                    "SELECT {COLUMNS} FROM meeting ORDER BY startedAt DESC, id LIMIT ?1 OFFSET ?2"
                ),
                params![limit, offset],
                from_row,
            )
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
            query_all(
                connection,
                &format!(
                    "SELECT {COLUMNS} FROM meeting WHERE state IN ({placeholders}) \
                     ORDER BY startedAt, id"
                ),
                params_from_iter(kinds.iter().map(|kind| kind.as_str())),
                from_row,
            )
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
            let mut meetings = query_all(
                transaction,
                &format!("SELECT {COLUMNS} FROM meeting WHERE state = ?1 ORDER BY startedAt, id"),
                [MeetingStateKind::Recording.as_str()],
                from_row,
            )?;
            for meeting in &mut meetings {
                meeting.state = MeetingState::Failed {
                    reason: reason.to_owned(),
                };
                meeting.updated_at = now;
                save(transaction, meeting)?;
            }
            Ok(meetings.into_iter().map(|meeting| meeting.id).collect())
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

    /// `update_meeting` for the one field the pipeline changes most.
    pub fn set_state(&self, id: Uuid, state: MeetingState, now: DateTime<Utc>) -> Result<()> {
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
            meeting.title_origin = TitleOrigin::User;
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
    ///
    /// The store commits with `synchronous = NORMAL`, so the delete need
    /// not be on disk when this returns: a power loss or OS crash can roll
    /// it back after the caller removed the files, as in the Swift app. A
    /// delete that must be on disk first needs a commit with `FULL`, which
    /// the store does not offer yet.
    pub fn delete_meeting(&self, id: Uuid) -> Result<DeletedMeeting> {
        self.write(|transaction| {
            let meeting = current(transaction, id)?;
            match meeting.state.kind() {
                kind @ (MeetingStateKind::Recording | MeetingStateKind::Processing) => {
                    return Err(StoreError::MeetingBusy(id, kind));
                }
                MeetingStateKind::Queued | MeetingStateKind::Ready | MeetingStateKind::Failed => {}
            }
            let assets = assets::assets_of_meeting(transaction, id)?;
            let clips = people::speakers_of_meeting(transaction, id)?
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
