use rusqlite::{Connection, Row, params};
use uuid::Uuid;

use super::convert::{DbEnum, DbUuid, Unwrap as _};
use super::{Result, Store};
use crate::model::TranscriptSegment;

const COLUMNS: &str = "id, meetingID, start, end, speakerID, lane, text, rawText";

fn from_row(row: &Row<'_>) -> rusqlite::Result<TranscriptSegment> {
    Ok(TranscriptSegment {
        id: row.get::<_, DbUuid>("id")?.0,
        meeting_id: row.get::<_, DbUuid>("meetingID")?.0,
        start: row.get("start")?,
        end: row.get("end")?,
        speaker_id: row.get::<_, Option<DbUuid>>("speakerID")?.unwrap_db(),
        lane: row.get::<_, DbEnum<_>>("lane")?.0,
        text: row.get("text")?,
        raw_text: row.get("rawText")?,
    })
}

pub(super) fn insert_segment(connection: &Connection, segment: &TranscriptSegment) -> Result<()> {
    connection.execute(
        &format!(
            "INSERT INTO transcriptSegment ({COLUMNS}) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)"
        ),
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
    )?;
    Ok(())
}

impl Store {
    /// The meeting's transcript in time order.
    pub fn segments(&self, meeting_id: Uuid) -> Result<Vec<TranscriptSegment>> {
        self.read(|connection| {
            let mut statement = connection.prepare(&format!(
                "SELECT {COLUMNS} FROM transcriptSegment WHERE meetingID = ?1 ORDER BY start, id"
            ))?;
            let rows = statement.query_map([DbUuid(meeting_id)], from_row)?;
            Ok(rows.collect::<rusqlite::Result<_>>()?)
        })
    }
}
