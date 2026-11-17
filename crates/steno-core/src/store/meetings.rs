//! `meeting` rows and the transactions that write a meeting together with
//! what hangs off it.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use rusqlite::{Connection, OptionalExtension, Row, params, params_from_iter};
use uuid::Uuid;

use super::convert::{DbDate, DbEnum, DbJson, DbUuid, RowExt as _};
use super::{
    Result, Store, StoreError, assets, people, query_all, settings, tasks, transcript, upsert_sql,
};
use crate::model::{
    AudioAsset, AudioRetention, Decision, LanguageTag, Meeting, MeetingState, MeetingStateKind,
    MeetingTask, Participant, RecordingEndReason, Speaker, SpeakerAssignment,
    SpeakerNameSuggestion, SummaryDocument, TitleOrigin, TranscriptSegment, VoiceStrength,
    derived_uuid, me_speaker_id, room_speaker_id,
};
use crate::paths::file_url_path;

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

/// Inserts or replaces the row; `summaryText` is derived from `summary`.
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

/// Saves `participants` for `meeting_id`, each re-pointed at it.
fn save_participants(
    connection: &Connection,
    meeting_id: Uuid,
    participants: &[Participant],
) -> Result<()> {
    for participant in participants {
        let mut participant = participant.clone();
        participant.meeting_id = meeting_id;
        people::save_participant(connection, &participant)?;
    }
    Ok(())
}

pub(super) fn has_row(connection: &Connection, id: Uuid) -> Result<bool> {
    Ok(connection
        .query_row("SELECT 1 FROM meeting WHERE id = ?1", [DbUuid(id)], |_| {
            Ok(())
        })
        .optional()?
        .is_some())
}

/// The row as it is now, for a read-modify-write; `MeetingNotFound` when
/// there is none.
pub(super) fn current(connection: &Connection, id: Uuid) -> Result<Meeting> {
    fetch(connection, id)?.ok_or(StoreError::MeetingNotFound(id))
}

fn save_failed(
    connection: &Connection,
    meeting: &mut Meeting,
    reason: &str,
    now: DateTime<Utc>,
) -> Result<()> {
    meeting.state = MeetingState::Failed {
        reason: reason.to_owned(),
    };
    meeting.updated_at = now;
    save(connection, meeting)
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
/// at, removed once the transaction has committed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeletedMeeting {
    pub assets: Vec<AudioAsset>,
    /// The speakers' sample clip URLs.
    pub clips: Vec<String>,
}

impl DeletedMeeting {
    /// The meeting folder when an asset lives in one, else every file the
    /// rows name, each once, in asset then clip order.
    #[must_use]
    pub fn files_to_remove(&self, meeting_id: Uuid) -> Vec<PathBuf> {
        let folder_name = crate::json::uuid_string(meeting_id);
        let folder = self
            .assets
            .iter()
            .filter_map(|asset| file_url_path(&asset.url))
            .filter_map(|path| path.parent().map(Path::to_path_buf))
            .find(|folder| {
                folder
                    .file_name()
                    .is_some_and(|name| name == folder_name.as_str())
            });
        if let Some(folder) = folder {
            return vec![folder];
        }
        let mut seen = BTreeSet::new();
        self.assets
            .iter()
            .flat_map(AudioAsset::expirable_files)
            .chain(self.clips.iter().cloned())
            .filter_map(|url| file_url_path(&url))
            .filter(|path| seen.insert(path.clone()))
            .collect()
    }
}

impl Store {
    /// Inserts or replaces the meeting row; `summaryText` is derived from
    /// `summary`.
    pub fn save_meeting(&self, meeting: &Meeting) -> Result<()> {
        self.write(|transaction| save(transaction, meeting))
    }

    /// The meeting and its asset in one transaction.
    pub fn save_meeting_with_asset(&self, meeting: &Meeting, asset: &AudioAsset) -> Result<()> {
        self.write(|transaction| {
            save(transaction, meeting)?;
            assets::save(transaction, asset)
        })
    }

    /// A meeting with its asset, in one transaction, only while no row has
    /// its id: the launch's adoption of a recording with no meeting. A row
    /// written meanwhile fails with [`StoreError::MeetingExists`] and
    /// nothing is written.
    pub fn insert_meeting_with_asset(&self, meeting: &Meeting, asset: &AudioAsset) -> Result<()> {
        self.write(|transaction| {
            if has_row(transaction, meeting.id)? {
                return Err(StoreError::MeetingExists(meeting.id));
            }
            save(transaction, meeting)?;
            assets::save(transaction, asset)
        })
    }

    /// A stopped recording's commit, in one transaction on the writer, its
    /// reads included, so it never waits for a reader.
    ///
    /// Writes only while the row is still `recording`: it turns `queued`
    /// with `duration`, `end_reason` and `now`, keeping the rest as stored.
    /// `asset` is saved with `retention`, else the settings' default as
    /// stored now (keep-forever, with a warning naming the key, when that
    /// row cannot be read), and no `expires_at`. Returns the row as saved.
    /// A row that moved on fails with [`StoreError::NotRecording`], one
    /// that is gone with [`StoreError::MeetingNotFound`], and nothing is
    /// written.
    pub fn save_stopped_recording(
        &self,
        meeting_id: Uuid,
        duration: f64,
        end_reason: RecordingEndReason,
        asset: &AudioAsset,
        retention: Option<AudioRetention>,
        now: DateTime<Utc>,
    ) -> Result<Meeting> {
        self.write(|transaction| {
            let mut stored = current(transaction, meeting_id)?;
            if stored.state != MeetingState::Recording {
                return Err(StoreError::NotRecording(meeting_id, stored.state.kind()));
            }
            let retention = match retention {
                Some(retention) => retention,
                None => settings::default_retention(transaction)?,
            };
            stored.duration = duration;
            stored.end_reason = Some(end_reason);
            stored.state = MeetingState::Queued;
            stored.updated_at = now;
            save(transaction, &stored)?;
            assets::save(
                transaction,
                &AudioAsset {
                    meeting_id,
                    retention,
                    expires_at: None,
                    ..asset.clone()
                },
            )?;
            Ok(stored)
        })
    }

    /// The meeting and its participants in one transaction; every
    /// participant is re-pointed at the meeting.
    pub fn save_meeting_with_participants(
        &self,
        meeting: &Meeting,
        participants: &[Participant],
    ) -> Result<()> {
        self.write(|transaction| {
            save(transaction, meeting)?;
            save_participants(transaction, meeting.id, participants)
        })
    }

    /// Names a recording after its calendar event once the event is known,
    /// which may be after the recording started. Names the row `id` only:
    ///
    /// - while that row still records;
    /// - under its default title;
    /// - before any event named it.
    ///
    /// Then, in one transaction, the row takes `calendar_event_id`, `title`
    /// with origin `calendar` when there is one, and `participants`.
    /// Returns whether it wrote; a row that is gone is
    /// [`StoreError::MeetingNotFound`].
    pub fn name_recording_after_event(
        &self,
        id: Uuid,
        title: Option<&str>,
        calendar_event_id: &str,
        participants: &[Participant],
        now: DateTime<Utc>,
    ) -> Result<bool> {
        self.write(|transaction| {
            let mut stored = current(transaction, id)?;
            if stored.state != MeetingState::Recording
                || stored.title_origin != TitleOrigin::Default
                || stored.calendar_event_id.is_some()
            {
                return Ok(false);
            }
            stored.calendar_event_id = Some(calendar_event_id.to_owned());
            if let Some(title) = title {
                title.clone_into(&mut stored.title);
                stored.title_origin = TitleOrigin::Calendar;
            }
            stored.updated_at = now;
            save(transaction, &stored)?;
            save_participants(transaction, id, participants)?;
            Ok(true)
        })
    }

    pub fn meeting(&self, id: Uuid) -> Result<Option<Meeting>> {
        self.read(|connection| fetch(connection, id))
    }

    /// Every meeting's id, without reading the rows.
    pub fn meeting_ids(&self) -> Result<Vec<Uuid>> {
        self.read(|connection| {
            query_all(connection, "SELECT id FROM meeting", [], |row| {
                Ok(row.get::<_, DbUuid>(0)?.0)
            })
        })
    }

    /// Every meeting, newest first by `startedAt`: the list column's source.
    pub fn all_meetings(&self) -> Result<Vec<Meeting>> {
        self.read(|connection| {
            query_all(
                connection,
                &format!("SELECT {COLUMNS} FROM meeting ORDER BY startedAt DESC, id"),
                [],
                from_row,
            )
        })
    }

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

    /// Every meeting in one of `kinds`, oldest first by `startedAt`.
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

    /// The reason a launch gives a recording a process left `recording`.
    pub const INTERRUPTED_RECORDING_REASON: &'static str =
        "Recording was interrupted before it finished.";

    /// Launch reconciliation's failing half: marks the meetings in `ids`
    /// `failed(reason)`, each only while still `recording`, in one
    /// transaction, and returns the ones it marked in `ids` order. A
    /// recording another process still writes, or one started since, is
    /// left alone.
    pub fn fail_recordings(
        &self,
        ids: &[Uuid],
        reason: &str,
        now: DateTime<Utc>,
    ) -> Result<Vec<Uuid>> {
        self.write(|transaction| {
            let mut failed = Vec::new();
            for &id in ids {
                let Some(mut meeting) = fetch(transaction, id)? else {
                    continue;
                };
                if meeting.state != MeetingState::Recording {
                    continue;
                }
                save_failed(transaction, &mut meeting, reason, now)?;
                failed.push(id);
            }
            Ok(failed)
        })
    }

    /// Read-modify-write in one transaction: `mutate` sees the row as it is
    /// now. `updatedAt` is set to `now`. Returns the meeting as written.
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
    /// and segment, replaced. The merge stage calls this.
    ///
    /// The user's answers about the stored voices come back on the new rows
    /// that are unambiguously the same voice ([`same_voices`]), never by id
    /// alone: ids derive from "Speaker n", which counts by first speech, so
    /// another run can give a label to another voice. A kept confirmation
    /// keeps its person, the score its Yes was given
    /// (`speaker.confirmedSimilarity`) and, when the new row has no clip,
    /// the stored `sampleClipURL` and `sampleClipRange`, so the clip the
    /// user confirmed stays playable and is not swept; a dismissed voice
    /// stays dismissed; a No to a person stays, so a new suggestion of that
    /// person for that voice is dropped. An answer whose voice is not found
    /// again, or whose match is contested, is dropped, and the voice shows
    /// for review. Every person with a confirmed speaker before has their
    /// voice recomputed.
    ///
    /// A voice combined into another competes with its own embedding,
    /// whatever its root's answer, and carries its own Noes and its root's
    /// confirmation or dismissal. A score is carried only when it was
    /// measured against the person the new row is confirmed as. A new row
    /// that took a combined voice's confirmation is combined again, into
    /// the new row of its root when the root was found again, else into
    /// another new row confirmed as that person, so a person keeps one voice
    /// per meeting; one that took its root's dismissal is combined again
    /// when the root was found again. Combined again, it takes back the
    /// combined row's own assignment, dismissal and Yes score, so Separate
    /// again restores what the user said about it.
    ///
    /// Commits durably ([`Store::write_durably`]): the pipeline removes the
    /// clips the earlier rows named once this returns, and a power loss
    /// must not bring those rows back. Returns the replaced rows for that
    /// removal.
    pub fn replace_transcript(
        &self,
        meeting: &Meeting,
        segments: &[TranscriptSegment],
        speakers: &[Speaker],
    ) -> Result<Vec<Speaker>> {
        self.write_durably(|transaction| {
            write_processing_results(transaction, meeting)?;
            transcript::bump_transcript_generation(transaction, meeting.id)?;
            // The voices answers can follow; a voice combined into another
            // comes back uncombined, as its own row.
            let replaced = people::speakers_of_meeting(transaction, meeting.id)?;
            // Every row, combined ones too, for the voices to recompute and
            // the sweep, which must see every clip they named.
            let every = people::all_speakers_of_meeting(transaction, meeting.id)?;
            // Deleting the speakers cascades to their suggestions and the
            // user's Noes.
            let suggestions = people::name_suggestions_of_meeting(transaction, meeting.id)?;
            let rejections = people::suggestion_rejections_of_meeting(transaction, meeting.id)?;
            // Every combined voice competes with its own embedding, read as
            // its root: confirmed or dismissed as the root is, so it carries
            // the root's answer, not its own.
            let merged = people::merged_speakers_of_meeting(transaction, meeting.id)?;
            let combined: Vec<Speaker> = merged
                .iter()
                .map(|merged| {
                    let root = replaced.iter().find(|root| root.id == merged.root);
                    Speaker {
                        assignment: root
                            .map_or(SpeakerAssignment::Unknown, |root| root.assignment.clone()),
                        dismissed: root.is_some_and(|root| root.dismissed),
                        ..merged.speaker.clone()
                    }
                })
                .collect();
            let stored: Vec<&Speaker> = replaced.iter().chain(&combined).collect();
            // New id -> the stored row whose answers it takes. Every stored
            // row competes; only an unambiguous pair carries anything. Two
            // rows of one person can pair with one new row; the root wins.
            let mut earlier: BTreeMap<Uuid, &Speaker> = BTreeMap::new();
            for (old, new) in same_voices(meeting.id, &stored, speakers) {
                earlier.entry(new).or_insert(old);
            }
            // Kept or dropped, each stored confirmation's person is
            // recomputed.
            let voices: BTreeSet<Uuid> = every
                .iter()
                .filter(|speaker| speaker.assignment.is_confirmed())
                .filter_map(Speaker::person_id)
                .collect();
            // The score each kept Yes was given, before its row goes: only
            // when the row was itself confirmed as the person it now reads
            // as, so a combined voice never lends its own person's score.
            let mut yes_scores = BTreeMap::new();
            for (&new, &kept) in &earlier {
                let own = every.iter().find(|speaker| speaker.id == kept.id);
                if kept.assignment.is_confirmed()
                    && own.is_some_and(|own| own.assignment == kept.assignment)
                {
                    yes_scores.insert(new, people::confirmed_similarity(transaction, kept.id)?);
                }
            }
            let own_rows = combined_rows(transaction, &merged)?;
            transaction.execute(
                "DELETE FROM transcriptSegment WHERE meetingID = ?1",
                [DbUuid(meeting.id)],
            )?;
            transaction.execute(
                "DELETE FROM speaker WHERE meetingID = ?1",
                [DbUuid(meeting.id)],
            )?;
            let mut kept_rejections = Vec::new();
            for speaker in speakers {
                let mut speaker = speaker.clone();
                speaker.meeting_id = meeting.id;
                if let Some(kept) = earlier.get(&speaker.id) {
                    let rejected: BTreeSet<Uuid> = rejections
                        .iter()
                        .filter(|rejection| rejection.speaker_id == kept.id)
                        .map(|rejection| {
                            kept_rejections.push(people::Rejection {
                                speaker_id: speaker.id,
                                ..*rejection
                            });
                            rejection.person_id
                        })
                        .collect();
                    if kept.assignment.is_confirmed() {
                        speaker.assignment = kept.assignment.clone();
                        if speaker.sample_clip_url.is_none() && kept.sample_clip_url.is_some() {
                            speaker.sample_clip_url.clone_from(&kept.sample_clip_url);
                            speaker.sample_clip_range = kept.sample_clip_range;
                        }
                    } else if let SpeakerAssignment::Suggested { person_id, .. } =
                        speaker.assignment
                        && rejected.contains(&person_id)
                    {
                        speaker.assignment = SpeakerAssignment::Unknown;
                    }
                    if kept.dismissed {
                        speaker.dismissed = true;
                        speaker.assignment = SpeakerAssignment::Unknown;
                    }
                }
                people::insert_speaker(transaction, &speaker)?;
                if let Some(&score) = yes_scores.get(&speaker.id) {
                    people::set_confirmed_similarity(transaction, speaker.id, score)?;
                }
            }
            combine_again(transaction, meeting.id, &earlier, &own_rows)?;
            people::replace_name_suggestions(transaction, meeting.id, &suggestions)?;
            people::restore_suggestion_rejections(transaction, meeting.id, &kept_rejections)?;
            for segment in segments {
                let mut segment = segment.clone();
                segment.meeting_id = meeting.id;
                transcript::insert_segment(transaction, &segment)?;
            }
            for person_id in voices {
                people::refresh_voice(transaction, person_id)?;
            }
            Ok(every)
        })
    }

    /// One transaction: the processing columns plus the `text` of each of
    /// `segments` still stored. Speakers and other columns stay, so a
    /// speaker named while the cleanup ran keeps its name.
    pub fn update_segment_texts(
        &self,
        meeting: &Meeting,
        segments: &[TranscriptSegment],
    ) -> Result<()> {
        self.write(|transaction| {
            write_processing_results(transaction, meeting)?;
            for segment in segments {
                transcript::update_text(transaction, meeting.id, segment)?;
            }
            Ok(())
        })
    }

    /// One transaction: the columns [`Meeting::apply_processing_results`]
    /// copies, over the row as it is now. For a summarize stage with no
    /// summarizer, so an earlier summary stays.
    pub fn save_processing_results(&self, meeting: &Meeting) -> Result<()> {
        self.write(|transaction| write_processing_results(transaction, meeting))
    }

    /// One transaction: the processing columns plus the tasks, decisions
    /// and name suggestions, replaced. Decision ids derive from the meeting
    /// id. Of `speaker_names`, only named suggestions for one of the
    /// meeting's speakers are kept, the strongest per speaker. The stored
    /// template stays (see [`Meeting::apply_processing_results`]).
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
    /// cascades, and the handover receipt that admitted it. Persons stay.
    /// Fails with `MeetingBusy` while `recording` or `processing`. Returns
    /// the files the rows named, for the caller to remove.
    ///
    /// Not durable: a power loss can roll the delete back after the caller
    /// removed the files.
    pub fn delete_meeting(&self, id: Uuid) -> Result<DeletedMeeting> {
        self.delete(id, false)
    }

    /// [`Self::delete_meeting`] that also removes a meeting left
    /// `recording`, for a caller that knows no capture of its own writes it
    /// (a failed save, a recovery whose folder is gone). Still refuses one
    /// `processing`. The caller removes its folder.
    pub fn delete_meeting_left_recording(&self, id: Uuid) -> Result<DeletedMeeting> {
        self.delete(id, true)
    }

    fn delete(&self, id: Uuid, left_recording: bool) -> Result<DeletedMeeting> {
        self.write(|transaction| {
            let meeting = current(transaction, id)?;
            match meeting.state.kind() {
                MeetingStateKind::Recording if left_recording => {}
                kind @ (MeetingStateKind::Recording | MeetingStateKind::Processing) => {
                    return Err(StoreError::MeetingBusy(id, kind));
                }
                MeetingStateKind::Queued | MeetingStateKind::Ready | MeetingStateKind::Failed => {}
            }
            let assets = assets::assets_of_meeting(transaction, id)?;
            let clips = people::all_speakers_of_meeting(transaction, id)?
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

/// Each combined row as it is, with its own Yes score, by id, for
/// [`combine_again`].
fn combined_rows<'a>(
    transaction: &Connection,
    merged: &'a [people::MergedSpeaker],
) -> Result<BTreeMap<Uuid, (&'a people::MergedSpeaker, Option<f32>)>> {
    let mut rows = BTreeMap::new();
    for merged in merged {
        let score = people::confirmed_similarity(transaction, merged.speaker.id)?;
        rows.insert(merged.speaker.id, (merged, score));
    }
    Ok(rows)
}

/// Process again keeps a combine: each new row that took a combined
/// voice's confirmation is combined again, into the new row of the root it
/// read as when that root was found again, else into another new row
/// confirmed as the person, so a person keeps one voice per meeting. One
/// that took its root's dismissal is combined again when the root was
/// found again, so a combine under Not a person stays one voice. Without
/// either it stays its own voice, with the root's answer. Combined again,
/// the new row takes back the combined row's own assignment, dismissal and
/// Yes score, so Separate again restores them. `merged` is keyed by the
/// combined row's id, with its own Yes score.
fn combine_again(
    transaction: &Connection,
    meeting_id: Uuid,
    earlier: &BTreeMap<Uuid, &Speaker>,
    merged: &BTreeMap<Uuid, (&people::MergedSpeaker, Option<f32>)>,
) -> Result<()> {
    for (&new, &kept) in earlier {
        let Some(&(merged, own_score)) = merged.get(&kept.id) else {
            continue;
        };
        let root_found_again = earlier
            .iter()
            .find(|(_, old)| old.id == merged.root)
            .map(|(&id, _)| id)
            .filter(|&id| id != new);
        let into = match (&kept.assignment, root_found_again) {
            (_, Some(id)) if kept.assignment.is_confirmed() || kept.dismissed => Some(id),
            (SpeakerAssignment::Confirmed { person_id }, None) => {
                people::owner_of(transaction, meeting_id, *person_id, new)?
            }
            _ => None,
        };
        let Some(into) = into else {
            continue;
        };
        if let Some(mut row) = people::fetch_speaker(transaction, new)? {
            row.assignment = merged.speaker.assignment.clone();
            row.dismissed = merged.speaker.dismissed;
            people::save_speaker(transaction, &row)?;
            let score = own_score.filter(|_| row.assignment.is_confirmed());
            people::set_confirmed_similarity(transaction, new, score)?;
        }
        people::merge_speaker_rows(transaction, new, into, meeting_id)?;
    }
    Ok(())
}

/// How far a pair's cosine must lead the runner-up, for the stored row and
/// for the new row alike, before an answer moves across Process again.
/// Re-diarizing the same audio scores a voice against itself near 1 and
/// against other voices under 0.6 on the calibration corpus (the highest
/// wrong-person score is 0.54), so a voice found again clears 0.10 with
/// room; a voice the new run split in two, or joined with another, scores
/// two candidates close together and is left for the user.
const SAME_VOICE_MARGIN: f32 = 0.10;

/// Pairs the stored rows with the new rows that are the same voice, one to
/// one, and only where that is unambiguous. Every stored row competes,
/// answered or not, so an answered row cannot take a voice another stored
/// row matches better. A pair holds when the cosine of their embeddings is
/// at least [`VoiceStrength::FLOOR`] and leads every other candidate of
/// both rows by [`SAME_VOICE_MARGIN`]; anything closer is contested and
/// pairs nothing. Two stored rows confirmed as the same person are no
/// rivals: the user said they are one voice, so both may pair with the same
/// new row. The "me" speaker and the room row of a run without
/// diarization are lanes, not clusters, so each pairs only with itself,
/// whatever its embedding. A row without an embedding pairs with nothing
/// else. Returns `(earlier row, new id)`.
pub(super) fn same_voices<'a>(
    meeting_id: Uuid,
    earlier: &[&'a Speaker],
    speakers: &[Speaker],
) -> Vec<(&'a Speaker, Uuid)> {
    let lanes = [me_speaker_id(meeting_id), room_speaker_id(meeting_id)];
    let mut pairs: Vec<(&'a Speaker, Uuid)> = earlier
        .iter()
        .filter(|old| lanes.contains(&old.id) && speakers.iter().any(|new| new.id == old.id))
        .map(|&old| (old, old.id))
        .collect();
    let clusters = |id: &Uuid| !lanes.contains(id);
    let old_clusters: Vec<&'a Speaker> = earlier
        .iter()
        .copied()
        .filter(|speaker| clusters(&speaker.id) && speaker.embedding.is_some())
        .collect();
    let new_clusters: Vec<&Speaker> = speakers
        .iter()
        .filter(|speaker| clusters(&speaker.id) && speaker.embedding.is_some())
        .collect();
    let same_person = |lhs: &Speaker, rhs: &Speaker| {
        lhs.assignment.is_confirmed() && lhs.assignment == rhs.assignment
    };
    let score = |old: &Speaker, new: &Speaker| match (&old.embedding, &new.embedding) {
        (Some(lhs), Some(rhs)) => lhs.cosine_similarity(rhs),
        _ => f32::NEG_INFINITY,
    };
    for &old in &old_clusters {
        let Some((best, similarity)) = new_clusters
            .iter()
            .map(|&new| (new, score(old, new)))
            .max_by(|lhs, rhs| lhs.1.total_cmp(&rhs.1))
        else {
            continue;
        };
        if similarity < VoiceStrength::FLOOR {
            continue;
        }
        let runner_up = new_clusters
            .iter()
            .filter(|new| new.id != best.id)
            .map(|&new| score(old, new))
            .chain(
                old_clusters
                    .iter()
                    .filter(|other| other.id != old.id && !same_person(old, other))
                    .map(|&other| score(other, best)),
            )
            .fold(f32::NEG_INFINITY, f32::max);
        if similarity - runner_up >= SAME_VOICE_MARGIN {
            pairs.push((old, best.id));
        }
    }
    pairs
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::model::{AudioFormat, AudioLane, AudioRetention, Settings};

    fn asset(meeting_id: Uuid, url: &str) -> AudioAsset {
        AudioAsset {
            id: Uuid::nil(),
            meeting_id,
            url: url.to_owned(),
            format: AudioFormat::Caf48kFloat32,
            lanes: vec![AudioLane::Mic],
            sidecars_16k: BTreeMap::from([(
                AudioLane::Mic,
                "file:///audio/elsewhere/mic.wav".to_owned(),
            )]),
            mixdown_url: None,
            retention: AudioRetention::KeepForever,
            expires_at: None,
        }
    }

    #[test]
    fn a_meeting_folder_is_removed_whole_and_loose_files_once_each() {
        let id = Uuid::parse_str("00000000-0000-0000-0000-000000000001").unwrap();
        let in_folder = DeletedMeeting {
            assets: vec![asset(
                id,
                "file:///audio/00000000-0000-0000-0000-000000000001/master.caf",
            )],
            clips: vec![
                "file:///audio/00000000-0000-0000-0000-000000000001/speakers/a.wav".to_owned(),
            ],
        };
        assert_eq!(
            in_folder.files_to_remove(id),
            vec![PathBuf::from("/audio/00000000-0000-0000-0000-000000000001")]
        );
        let loose = DeletedMeeting {
            assets: vec![asset(id, "file:///audio/master.caf")],
            clips: vec![
                "file:///audio/clip.wav".to_owned(),
                "file:///audio/clip.wav".to_owned(),
            ],
        };
        assert_eq!(
            loose.files_to_remove(id),
            vec![
                PathBuf::from("/audio/master.caf"),
                PathBuf::from("/audio/elsewhere/mic.wav"),
                PathBuf::from("/audio/clip.wav"),
            ]
        );
    }

    /// The merge's write switches a kept confirmation and its new clip in
    /// one commit under `synchronous = FULL`, and returns the row it
    /// replaced.
    #[test]
    fn replacing_the_transcript_switches_the_clips_with_the_confirmations_durably() {
        use std::sync::{Arc, Mutex};

        use crate::testing::sample_data;
        use crate::{Speaker, SpeakerAssignment};

        /// One commit's `synchronous` level, assignment and clip.
        type Commit = (i64, String, Option<String>);

        let directory = tempfile::tempdir().unwrap();
        let store = Store::open(directory.path().join("steno.sqlite")).unwrap();
        let anna = sample_data::person(0, "Anna");
        store.save_person(&anna).unwrap();
        let meeting = sample_data::meeting();
        store.save_meeting(&meeting).unwrap();
        let mut speaker = Speaker {
            id: Uuid::new_v4(),
            meeting_id: meeting.id,
            cluster_label: "Speaker 1".to_owned(),
            assignment: SpeakerAssignment::Confirmed { person_id: anna.id },
            // The voice the re-run finds again.
            embedding: Some(crate::model::Embedding(
                vec![0.5; crate::model::Embedding::DIMENSION],
            )),
            sample_clip_range: None,
            sample_clip_url: Some("file:///audio/speakers/earlier.wav".to_owned()),
            cluster_confidence: 1.0,
            dismissed: false,
        };
        store.save_speaker(&speaker).unwrap();
        let seen: Arc<Mutex<Vec<Commit>>> = Arc::default();
        let commits = seen.clone();
        store.probe_commits(move |connection| {
            let level = connection
                .query_row("PRAGMA synchronous", [], |row| row.get(0))
                .unwrap();
            let (assignment, clip) = connection
                .query_row("SELECT assignment, sampleClipURL FROM speaker", [], |row| {
                    Ok((row.get(0)?, row.get(1)?))
                })
                .unwrap();
            commits.lock().unwrap().push((level, assignment, clip));
        });

        let earlier = speaker.clone();
        speaker.assignment = SpeakerAssignment::Unknown;
        speaker.sample_clip_url = Some("file:///audio/speakers/new.wav".to_owned());
        let replaced = store.replace_transcript(&meeting, &[], &[speaker]).unwrap();

        assert_eq!(replaced, [earlier], "the rows it replaced, for the sweep");

        assert_eq!(
            *seen.lock().unwrap(),
            [(
                2,
                "confirmed".to_owned(),
                Some("file:///audio/speakers/new.wav".to_owned())
            )]
        );
    }

    /// A meeting `recording`, as the recorder's start writes it.
    fn a_recording_row(store: &Store) -> Meeting {
        let mut meeting = crate::testing::sample_data::meeting();
        meeting.id = Uuid::new_v4();
        meeting.state = MeetingState::Recording;
        meeting.end_reason = None;
        meeting.duration = 0.0;
        store.save_meeting(&meeting).unwrap();
        store.meeting(meeting.id).unwrap().unwrap()
    }

    /// Stores `value` as the setting row of `key`, past `save_settings`.
    fn store_setting_row(store: &Store, key: &str, value: &str) {
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

    /// The stop's save of a fresh recording row with no named retention.
    fn save_a_stop(store: &Store) -> (Meeting, AudioAsset) {
        let meeting = a_recording_row(store);
        let saved = store
            .save_stopped_recording(
                meeting.id,
                1.0,
                RecordingEndReason::Manual,
                &asset(meeting.id, "file:///audio/x/master.caf"),
                None,
                Utc::now(),
            )
            .unwrap();
        (saved, store.asset(meeting.id).unwrap().unwrap())
    }

    /// What `body` returned, and the warnings it logged on this thread.
    fn warnings<T>(body: impl FnOnce() -> T) -> (T, String) {
        use std::sync::{Arc, Mutex};

        #[derive(Clone, Default)]
        struct Lines(Arc<Mutex<Vec<u8>>>);
        impl std::io::Write for Lines {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(bytes);
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let lines = Lines::default();
        let writer = lines.clone();
        let subscriber = tracing_subscriber::fmt()
            .with_ansi(false)
            .with_max_level(tracing::Level::WARN)
            .with_writer(move || writer.clone())
            .finish();
        let value = tracing::subscriber::with_default(subscriber, body);
        let text = String::from_utf8(lines.0.lock().unwrap().clone()).unwrap();
        (value, text)
    }

    /// The stop's save writes each column it owns, takes the default
    /// retention at the commit unless named, never keeps `expires_at`, and
    /// refuses a second save.
    #[test]
    fn the_stopped_recordings_save_writes_its_columns() {
        let directory = tempfile::tempdir().unwrap();
        let store = Store::open(directory.path().join("steno.sqlite")).unwrap();
        let mut settings = store.settings().unwrap();
        settings.default_retention = AudioRetention::KeepDays(3);
        store.save_settings(&settings).unwrap();
        let now = DateTime::parse_from_rfc3339("2026-09-24T10:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        for (named, kept) in [
            (None, AudioRetention::KeepDays(3)),
            (
                Some(AudioRetention::DeleteAfterProcessing),
                AudioRetention::DeleteAfterProcessing,
            ),
        ] {
            let meeting = a_recording_row(&store);
            // Pointed at another meeting: the save points it at its own.
            let mut captured = asset(Uuid::new_v4(), "file:///audio/x/master.caf");
            captured.id = Uuid::new_v4();
            captured.expires_at = Some(now);
            let saved = store
                .save_stopped_recording(
                    meeting.id,
                    12.5,
                    RecordingEndReason::Manual,
                    &captured,
                    named,
                    now,
                )
                .unwrap();
            let stored = store.meeting(meeting.id).unwrap().unwrap();
            assert_eq!(saved, stored);
            assert_eq!(stored.state, MeetingState::Queued);
            assert_eq!(stored.duration, 12.5);
            assert_eq!(stored.end_reason, Some(RecordingEndReason::Manual));
            assert_eq!(stored.updated_at, now);
            let written = store.asset(meeting.id).unwrap().unwrap();
            assert_eq!(written.id, captured.id);
            assert_eq!(written.meeting_id, meeting.id);
            assert_eq!(written.retention, kept);
            assert_eq!(written.expires_at, None);
            let error = store
                .save_stopped_recording(
                    meeting.id,
                    99.0,
                    RecordingEndReason::Failed,
                    &captured,
                    None,
                    now,
                )
                .unwrap_err();
            assert!(matches!(error, StoreError::NotRecording(_, _)), "{error}");
            assert_eq!(store.meeting(meeting.id).unwrap().unwrap(), stored);
        }
    }

    /// An unreadable default retention row does not fail the stop's save:
    /// the audio is kept forever and the warning names the key only.
    #[test]
    fn an_unreadable_default_retention_saves_the_stop_kept_forever() {
        for value in ["sk-secret-rule", "\"sk-secret-rule\""] {
            let directory = tempfile::tempdir().unwrap();
            let store = Store::open(directory.path().join("steno.sqlite")).unwrap();
            store_setting_row(&store, "defaultRetention", value);
            let ((saved, written), logged) = warnings(|| save_a_stop(&store));
            assert_eq!(saved.state, MeetingState::Queued);
            assert_eq!(written.retention, AudioRetention::KeepForever);
            assert!(logged.contains("WARN"), "{logged}");
            assert!(logged.contains("defaultRetention"), "{logged}");
            assert!(!logged.contains("sk-secret"), "{logged}");
        }
    }

    /// Another unreadable setting leaves the stop's save alone.
    #[test]
    fn an_unrelated_unreadable_setting_does_not_touch_the_stops_save() {
        for (key, value) in [("llmContextTokens", "not json"), ("audioFolder", "123")] {
            let directory = tempfile::tempdir().unwrap();
            let store = Store::open(directory.path().join("steno.sqlite")).unwrap();
            let mut settings = store.settings().unwrap();
            settings.default_retention = AudioRetention::KeepDays(3);
            store.save_settings(&settings).unwrap();
            store_setting_row(&store, key, value);
            assert!(store.settings().is_err(), "{key} is unreadable");
            let (saved, written) = save_a_stop(&store);
            assert_eq!(saved.state, MeetingState::Queued);
            assert_eq!(written.retention, AudioRetention::KeepDays(3));
        }
    }

    /// With no default retention row the asset gets the settings' default.
    #[test]
    fn a_missing_default_retention_saves_the_stop_with_the_default() {
        let directory = tempfile::tempdir().unwrap();
        let store = Store::open(directory.path().join("steno.sqlite")).unwrap();
        let mut settings = store.settings().unwrap();
        settings.default_retention = AudioRetention::KeepDays(3);
        store.save_settings(&settings).unwrap();
        store
            .write(|transaction| {
                transaction.execute("DELETE FROM setting WHERE key = 'defaultRetention'", [])?;
                Ok(())
            })
            .unwrap();
        let (saved, written) = save_a_stop(&store);
        assert_eq!(saved.state, MeetingState::Queued);
        assert_eq!(written.retention, Settings::default().default_retention);
    }
}
