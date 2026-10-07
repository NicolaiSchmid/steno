//! `meeting` rows and the transactions that write a meeting together with
//! what hangs off it.
//! Swift: `Sources/StenoCore/Storage/MeetingStore.swift`.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use rusqlite::{Connection, OptionalExtension, Row, params, params_from_iter};
use uuid::Uuid;

use super::convert::{DbDate, DbEnum, DbJson, DbUuid, RowExt as _};
use super::{Result, Store, StoreError, assets, people, query_all, tasks, transcript, upsert_sql};
use crate::model::{
    AudioAsset, Decision, LanguageTag, Meeting, MeetingState, MeetingStateKind, MeetingTask,
    Participant, Speaker, SpeakerAssignment, SpeakerNameSuggestion, SummaryDocument, TitleOrigin,
    TranscriptSegment, derived_uuid,
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
/// at, which the caller removes once the transaction has committed, through
/// [`DeletedMeeting::files_to_remove`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeletedMeeting {
    pub assets: Vec<AudioAsset>,
    /// The speakers' sample clip URLs.
    pub clips: Vec<String>,
}

impl DeletedMeeting {
    /// The meeting folder when an asset lives in one (the folder named
    /// after the meeting id, as the recording layout lays it out), else
    /// every file the rows name, each once, in asset then clip order.
    /// Swift: `MeetingStore.filesToRemove`.
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

    /// The reason a launch gives a recording that a process left
    /// `recording`. Swift: the default `reason` of
    /// `MeetingStore.failInterruptedRecordings` in
    /// `Sources/StenoCore/Storage/MeetingStore.swift`.
    pub const INTERRUPTED_RECORDING_REASON: &'static str =
        "Recording was interrupted before it finished.";

    /// Launch reconciliation: every meeting still `recording` belongs to a
    /// process that died mid-meeting. One transaction marks them
    /// `failed(reason)` with `updatedAt = now` and returns their ids,
    /// oldest first. The app passes [`Self::INTERRUPTED_RECORDING_REASON`].
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
    /// and segment of the meeting, replaced. The merge stage calls this.
    ///
    /// A speaker the user confirmed keeps its confirmation when its id
    /// comes back (speaker ids derive from the meeting id and the cluster
    /// label, so a re-run produces the same ids), and so does the model's
    /// name suggestion for it; every person who had a confirmed speaker
    /// here has their voice recomputed from the speakers now stored. A
    /// first write recomputes nothing, as in Swift. Rust only: Swift's
    /// `replaceTranscript` replaces the assignments and drops the suggestions
    /// (`.plans/2026-09-29-speaker-calibration.md`, decision 5).
    pub fn replace_transcript(
        &self,
        meeting: &Meeting,
        segments: &[TranscriptSegment],
        speakers: &[Speaker],
    ) -> Result<()> {
        self.write(|transaction| {
            write_processing_results(transaction, meeting)?;
            let confirmed: BTreeMap<Uuid, SpeakerAssignment> =
                people::speakers_of_meeting(transaction, meeting.id)?
                    .into_iter()
                    .filter(|speaker| speaker.assignment.is_confirmed())
                    .map(|speaker| (speaker.id, speaker.assignment))
                    .collect();
            let voices: BTreeSet<Uuid> = confirmed
                .values()
                .filter_map(SpeakerAssignment::person_id)
                .collect();
            // Deleting the speakers cascades to their suggestions.
            let suggestions = people::name_suggestions_of_meeting(transaction, meeting.id)?;
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
                if let Some(assignment) = confirmed.get(&speaker.id) {
                    speaker.assignment = assignment.clone();
                }
                people::insert_speaker(transaction, &speaker)?;
            }
            people::replace_name_suggestions(transaction, meeting.id, &suggestions)?;
            for segment in segments {
                let mut segment = segment.clone();
                segment.meeting_id = meeting.id;
                transcript::insert_segment(transaction, &segment)?;
            }
            for person_id in voices {
                people::refresh_voice(transaction, person_id)?;
            }
            Ok(())
        })
    }

    /// One transaction: the meeting's processing columns plus the `text` of
    /// each of `segments` that is still stored, by id. Speakers and the
    /// segments' other columns stay as they are, so a speaker the user
    /// named while the cleanup pass ran keeps its name. The cleanup stage
    /// calls this. Rust only: Swift's cleanup replaces the whole transcript.
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
    /// copies, over the row as it is now, and nothing else. The summarize
    /// stage calls this when no summarizer is set up, so the summary, tasks,
    /// decisions and name suggestions an earlier run wrote stay. Rust only:
    /// Swift's summarize stage without an LLM clears the summary.
    pub fn save_processing_results(&self, meeting: &Meeting) -> Result<()> {
        self.write(|transaction| write_processing_results(transaction, meeting))
    }

    /// One transaction: the meeting's processing columns plus its tasks,
    /// decisions and speaker name suggestions, replaced. Decision ids derive
    /// from the meeting id so re-runs are stable. Of `speaker_names`, only
    /// suggestions that carry a name and point at one of the meeting's
    /// speakers are kept, the strongest per speaker. The stored template
    /// stays: the user picks it, and the summary's `template_id` records
    /// which template made it (see [`Meeting::apply_processing_results`]).
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

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::model::{AudioFormat, AudioLane, AudioRetention};

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

    /// Swift: `MeetingStoreTests.testFilesToRemove`.
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
}
