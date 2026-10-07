//! `transcriptSegment` rows.
//! Swift: the segment methods of `Sources/StenoCore/Storage/MeetingStore.swift`.

use rusqlite::{Connection, Row, params};
use uuid::Uuid;

use super::convert::{DbEnum, DbUuid, RowExt as _};
use super::{Result, Store, execute_cached, insert_sql, query_all};
use crate::model::TranscriptSegment;

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

/// The meeting's transcript by start, ties by id.
pub(super) fn segments_of_meeting(
    connection: &Connection,
    meeting_id: Uuid,
) -> Result<Vec<TranscriptSegment>> {
    query_all(
        connection,
        &format!("SELECT {COLUMNS} FROM transcriptSegment WHERE meetingID = ?1 ORDER BY start, id"),
        [DbUuid(meeting_id)],
        from_row,
    )
}

impl Store {
    /// The meeting's transcript in time order.
    pub fn segments(&self, meeting_id: Uuid) -> Result<Vec<TranscriptSegment>> {
        self.read(|connection| segments_of_meeting(connection, meeting_id))
    }
}
