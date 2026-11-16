//! `transcriptSegment` rows.

use std::collections::{BTreeMap, BTreeSet};

use rusqlite::{Connection, OptionalExtension, Row, params};
use uuid::Uuid;

use super::convert::{DbEnum, DbUuid, RowExt as _};
use super::{Result, Store, StoreError, execute_cached, insert_sql, people, query_all};
use crate::model::{Person, Speaker, SpeakerAssignment, TranscriptSegment};

const COLUMNS: &str = "id, meetingID, start, end, speakerID, lane, text, rawText";

fn from_row(row: &Row<'_>) -> rusqlite::Result<TranscriptSegment> {
    Ok(TranscriptSegment {
        id: row.col::<DbUuid>("id")?,
        meeting_id: row.col::<DbUuid>("meetingID")?,
        start: row.get("start")?,
        end: row.get("end")?,
        speaker_id: row.col::<Option<DbUuid>>("speakerID")?,
        lane: row.col::<DbEnum<_>>("lane")?,
        text: row.get("text")?,
        raw_text: row.get("rawText")?,
    })
}

pub(super) fn insert_segment(connection: &Connection, segment: &TranscriptSegment) -> Result<()> {
    execute_cached(
        connection,
        &insert_sql("transcriptSegment", COLUMNS),
        params![
            DbUuid(segment.id),
            DbUuid(segment.meeting_id),
            segment.start,
            segment.end,
            segment.speaker_id.map(DbUuid),
            DbEnum(segment.lane),
            segment.text,
            segment.raw_text,
        ],
    )
}

/// Sets the stored `text` of `segment` (matched by id within `meeting_id`);
/// a segment that is no longer stored is skipped.
pub(super) fn update_text(
    connection: &Connection,
    meeting_id: Uuid,
    segment: &TranscriptSegment,
) -> Result<()> {
    execute_cached(
        connection,
        "UPDATE transcriptSegment SET text = ?1 WHERE id = ?2 AND meetingID = ?3",
        params![segment.text, DbUuid(segment.id), DbUuid(meeting_id)],
    )
}

/// The meeting's transcript in time order, each segment under the voice it
/// reads as: a speaker combined into another (`speaker.mergedInto`) gives
/// its segments that voice, always a root.
pub(super) fn segments_of_meeting(
    connection: &Connection,
    meeting_id: Uuid,
) -> Result<Vec<TranscriptSegment>> {
    query_all(
        connection,
        "SELECT segment.id AS id, segment.meetingID AS meetingID, segment.start AS start, \
         segment.end AS end, COALESCE(speaker.mergedInto, segment.speakerID) AS speakerID, \
         segment.lane AS lane, segment.text AS text, segment.rawText AS rawText \
         FROM transcriptSegment AS segment LEFT JOIN speaker ON speaker.id = segment.speakerID \
         WHERE segment.meetingID = ?1 ORDER BY segment.start, segment.id",
        [DbUuid(meeting_id)],
        from_row,
    )
}

/// The speaker id each segment of the meeting carries itself, combined or
/// not.
fn own_speakers_of_segments(
    connection: &Connection,
    meeting_id: Uuid,
) -> Result<BTreeMap<Uuid, Option<Uuid>>> {
    Ok(query_all(
        connection,
        "SELECT id, speakerID FROM transcriptSegment WHERE meetingID = ?1",
        [DbUuid(meeting_id)],
        |row| {
            Ok((
                row.col::<DbUuid>("id")?,
                row.col::<Option<DbUuid>>("speakerID")?,
            ))
        },
    )?
    .into_iter()
    .collect())
}

pub(super) fn transcript_generation(connection: &Connection, meeting_id: Uuid) -> Result<i64> {
    connection
        .query_row(
            "SELECT transcriptGeneration FROM meeting WHERE id = ?1",
            [DbUuid(meeting_id)],
            |row| row.get(0),
        )
        .optional()?
        .ok_or(StoreError::MeetingNotFound(meeting_id))
}

/// Raises the meeting's transcript generation: processing wrote a new
/// transcript, and moves made against the old one are stale.
pub(super) fn bump_transcript_generation(connection: &Connection, meeting_id: Uuid) -> Result<()> {
    connection.execute(
        "UPDATE meeting SET transcriptGeneration = transcriptGeneration + 1 WHERE id = ?1",
        [DbUuid(meeting_id)],
    )?;
    Ok(())
}

/// One label past the largest "Speaker N" among the meeting's rows,
/// combined ones included.
fn next_speaker_label(connection: &Connection, meeting_id: Uuid) -> Result<String> {
    let labels: Vec<String> = query_all(
        connection,
        "SELECT clusterLabel FROM speaker WHERE meetingID = ?1",
        [DbUuid(meeting_id)],
        |row| row.get(0),
    )?;
    let largest = labels
        .iter()
        .filter_map(|label| label.strip_prefix("Speaker "))
        .filter(|digits| !digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_digit()))
        .filter_map(|digits| digits.parse::<u64>().ok())
        .max()
        .unwrap_or(0);
    Ok(format!("Speaker {}", largest.saturating_add(1)))
}

impl Store {
    /// The meeting's transcript in time order, combined voices read as the
    /// voice they joined.
    pub fn segments(&self, meeting_id: Uuid) -> Result<Vec<TranscriptSegment>> {
        self.read(|connection| segments_of_meeting(connection, meeting_id))
    }

    /// How many times processing wrote the meeting's transcript (0 before
    /// schema v7, and for every transcript an older build wrote).
    pub fn transcript_generation(&self, meeting_id: Uuid) -> Result<i64> {
        self.read(|connection| transcript_generation(connection, meeting_id))
    }

    /// "This paragraph": every one of `segments` (each with the voice the
    /// caller saw it under) to `person`'s voice in this meeting, or to a new
    /// voice confirmed as them, labelled the next free "Speaker N", with no
    /// embedding and no clip, so no voice memory learns from the move. A
    /// voice the move leaves without paragraphs is deleted, and the voice
    /// of its person recomputed without it. Refused with
    /// [`StoreError::StaleTranscript`] when processing wrote the transcript
    /// again since `generation`, or a segment is gone or under another
    /// voice. Saves an unknown person. Returns the clip URLs to remove once
    /// the transaction committed.
    pub fn move_turn(
        &self,
        meeting_id: Uuid,
        generation: i64,
        segments: &[(Uuid, Option<Uuid>)],
        person: &Person,
    ) -> Result<Vec<String>> {
        self.write(|transaction| {
            // A meeting deleted since is as stale as one processed again.
            let current = match transcript_generation(transaction, meeting_id) {
                Ok(current) => Some(current),
                Err(StoreError::MeetingNotFound(_)) => None,
                Err(error) => return Err(error),
            };
            if segments.is_empty() || current != Some(generation) {
                return Err(StoreError::StaleTranscript);
            }
            let shown: BTreeMap<Uuid, Option<Uuid>> = segments_of_meeting(transaction, meeting_id)?
                .into_iter()
                .map(|segment| (segment.id, segment.speaker_id))
                .collect();
            if segments
                .iter()
                .any(|(id, speaker)| shown.get(id) != Some(speaker))
            {
                return Err(StoreError::StaleTranscript);
            }
            if people::fetch_person(transaction, person.id)?.is_none() {
                people::save_person(transaction, person)?;
            }
            let destination = if let Some(owner) =
                people::owner_of(transaction, meeting_id, person.id, Uuid::nil())?
            {
                owner
            } else {
                let speaker = Speaker {
                    id: Uuid::new_v4(),
                    meeting_id,
                    cluster_label: next_speaker_label(transaction, meeting_id)?,
                    assignment: SpeakerAssignment::Confirmed {
                        person_id: person.id,
                    },
                    embedding: None,
                    sample_clip_range: None,
                    sample_clip_url: None,
                    cluster_confidence: 0.0,
                    dismissed: false,
                };
                people::insert_speaker(transaction, &speaker)?;
                speaker.id
            };
            if segments
                .iter()
                .all(|(_, speaker)| *speaker == Some(destination))
            {
                return Ok(Vec::new());
            }
            let own = own_speakers_of_segments(transaction, meeting_id)?;
            let left: BTreeSet<Uuid> = segments
                .iter()
                .filter_map(|(id, _)| own.get(id).copied().flatten())
                .collect();
            for (id, _) in segments {
                execute_cached(
                    transaction,
                    "UPDATE transcriptSegment SET speakerID = ?1 WHERE id = ?2 AND meetingID = ?3",
                    params![DbUuid(destination), DbUuid(*id), DbUuid(meeting_id)],
                )?;
            }
            people::prune_emptied_speakers(transaction, left, destination)
        })
    }
}
