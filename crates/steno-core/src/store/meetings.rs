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
    Participant, Speaker, SpeakerNameSuggestion, SummaryDocument, TitleOrigin, TranscriptSegment,
    derived_uuid,
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

/// Writes `meeting` back `failed(reason)` with `updatedAt = now`.
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

    /// A stopped recording's commit (`LocalRecordingIntake::complete`,
    /// through `ProcessingPipeline::enqueue_stopped_recording`), in one
    /// transaction and only while the stored row is still `recording`: the
    /// row takes `meeting`'s duration, end reason, state and `updatedAt`
    /// and keeps the rest as stored (a title or notes saved since the
    /// caller read it), and `asset` is saved. A row that moved on fails with
    /// [`StoreError::NotRecording`] and one that is gone with
    /// [`StoreError::MeetingNotFound`], and nothing is written, so a
    /// recording deleted or failed meanwhile is not brought back. Rust
    /// only: Swift's `complete` saved the row it had read.
    pub fn save_stopped_recording(&self, meeting: &Meeting, asset: &AudioAsset) -> Result<()> {
        self.write(|transaction| {
            let mut stored = current(transaction, meeting.id)?;
            if stored.state != MeetingState::Recording {
                return Err(StoreError::NotRecording(meeting.id, stored.state.kind()));
            }
            stored.duration = meeting.duration;
            stored.end_reason.clone_from(&meeting.end_reason);
            stored.state = meeting.state.clone();
            stored.updated_at = meeting.updated_at;
            save(transaction, &stored)?;
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

    /// Launch reconciliation's failing half: marks the meetings in `ids`
    /// `failed(reason)` with `updatedAt = now`, each only while it is still
    /// `recording`, in one transaction, and returns the ones it marked, in
    /// `ids` order. The launch recovers what it can first and passes the
    /// rows it could not salvage, with
    /// [`Self::INTERRUPTED_RECORDING_REASON`], so a row it left alone (a
    /// recording another process is still writing) and a recording started
    /// since it listed them stay `recording`. Swift:
    /// `MeetingStore.failInterruptedRecordings` in
    /// `Sources/StenoCore/Storage/MeetingStore.swift`, which failed every
    /// `recording` row.
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
    ///
    /// The `sampleClipURL`s of `speakers` are written in the same
    /// transaction, so a kept confirmation and the clip of the run that
    /// produced the row switch together. A kept confirmation whose new row
    /// has no clip keeps the earlier `sampleClipURL` and its
    /// `sampleClipRange` when its stored row names one (an extension of
    /// decision 5), so the clip the user confirmed stays playable and no
    /// sweep or retention pass takes it for a leftover; that clip can come
    /// from an earlier run than the row's embedding (the "me" row has none).
    /// The kept clip follows the confirmation, not the cluster: a re-run
    /// that maps another cluster onto the confirmed id keeps the clip the
    /// user confirmed. Every call commits so that the commit is on the disk
    /// when it returns ([`Store::write_durably`], one WAL sync per
    /// processing run): the pipeline removes the clips the earlier rows
    /// named once it has returned, and a power loss must not bring those
    /// rows back. Returns the rows it replaced, as that transaction read
    /// them, for that removal. Rust only: Swift commits with `synchronous =
    /// NORMAL`, writes its clips in place and writes each new row's clip as
    /// the run gave it, none included.
    pub fn replace_transcript(
        &self,
        meeting: &Meeting,
        segments: &[TranscriptSegment],
        speakers: &[Speaker],
    ) -> Result<Vec<Speaker>> {
        self.write_durably(|transaction| {
            write_processing_results(transaction, meeting)?;
            let replaced = people::speakers_of_meeting(transaction, meeting.id)?;
            // The stored confirmed rows, by id. A row this run writes under
            // one of their ids takes the stored confirmation, and the stored
            // clip and range only when this run gives it none and the stored
            // row names one.
            let confirmed: BTreeMap<Uuid, &Speaker> = replaced
                .iter()
                .filter(|speaker| speaker.assignment.is_confirmed())
                .map(|speaker| (speaker.id, speaker))
                .collect();
            let voices: BTreeSet<Uuid> = confirmed
                .values()
                .filter_map(|speaker| speaker.person_id())
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
                if let Some(kept) = confirmed.get(&speaker.id) {
                    speaker.assignment = kept.assignment.clone();
                    if speaker.sample_clip_url.is_none() && kept.sample_clip_url.is_some() {
                        speaker.sample_clip_url.clone_from(&kept.sample_clip_url);
                        speaker.sample_clip_range = kept.sample_clip_range;
                    }
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
            Ok(replaced)
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
    /// not be on the disk when this returns: a power loss or OS crash can
    /// roll it back after the caller removed the files, as in the Swift
    /// app. A delete that must be on the disk first would commit through
    /// [`Store::write_durably`].
    pub fn delete_meeting(&self, id: Uuid) -> Result<DeletedMeeting> {
        self.delete(id, false)
    }

    /// [`Self::delete_meeting`] that also removes a meeting left
    /// `recording`, for a caller that knows no capture of its own writes it
    /// (the host, while its recorder is idle): a recording whose save
    /// failed, or one the launch's recovery keeps for a folder that is gone
    /// for good, so the user can remove the row. Still refuses a meeting
    /// that is `processing`. The rows name no master for such a meeting;
    /// the caller removes its folder. Rust only: Swift's list refused a
    /// recording row.
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

    /// The merge's write switches a kept confirmation and its new clip in
    /// one commit, under `synchronous = FULL` (2), and returns the row it
    /// replaced: the pipeline removes the earlier clip once it returns, so a
    /// power loss must not bring back the row that named it. That the commit
    /// then survives a power loss is SQLite's and cannot be tested.
    #[test]
    fn replacing_the_transcript_switches_the_clips_with_the_confirmations_durably() {
        use std::sync::{Arc, Mutex};

        use crate::testing::sample_data;
        use crate::{Speaker, SpeakerAssignment};

        /// What one commit wrote: its `synchronous` level, the speaker's
        /// assignment and its clip.
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
            embedding: None,
            sample_clip_range: None,
            sample_clip_url: Some("file:///audio/speakers/earlier.wav".to_owned()),
            cluster_confidence: 1.0,
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
}
