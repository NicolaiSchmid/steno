//! Full-text search over transcript segments and meeting titles and
//! summaries, through the two FTS5 tables the migrations keep in sync.
//! Swift: `Sources/StenoCore/Storage/MeetingStore+Search.swift`.

use rusqlite::{Connection, params};
use uuid::Uuid;

use super::convert::{DbUuid, RowExt as _};
use super::{Result, Store, query_all};

/// One full-text hit: a segment when `segment_id` is set, otherwise the
/// meeting's title or summary. `rank` is FTS5's bm25 (lower is better).
#[derive(Debug, Clone, PartialEq)]
pub struct SearchHit {
    pub meeting_id: Uuid,
    pub segment_id: Option<Uuid>,
    pub snippet: String,
    pub rank: f64,
}

/// GRDB's `FTS5Pattern(matchingAllTokensIn:)`: every token of `query` as a
/// quoted phrase, so FTS5 syntax in the text never reaches the parser;
/// `None` when the query holds no token.
#[must_use]
pub fn fts5_pattern(query: &str) -> Option<String> {
    let tokens: Vec<String> = query
        .split(|character: char| !character.is_alphanumeric())
        .filter(|token| !token.is_empty())
        .map(|token| format!("\"{token}\""))
        .collect();
    if tokens.is_empty() {
        None
    } else {
        Some(tokens.join(" "))
    }
}

fn search_rows(connection: &Connection, pattern: &str, limit: i64) -> Result<Vec<SearchHit>> {
    let mut hits = query_all(
        connection,
        "SELECT transcriptSegment.meetingID AS meetingID, transcriptSegment.id AS segmentID, \
         snippet(transcriptSegment_ft, 0, '[', ']', '…', 12) AS snippet, \
         transcriptSegment_ft.rank AS rank \
         FROM transcriptSegment_ft \
         JOIN transcriptSegment ON transcriptSegment.rowid = transcriptSegment_ft.rowid \
         WHERE transcriptSegment_ft MATCH ?1 \
         ORDER BY rank, transcriptSegment.start \
         LIMIT ?2",
        params![pattern, limit],
        |row| {
            Ok(SearchHit {
                meeting_id: row.col::<DbUuid>("meetingID")?,
                segment_id: Some(row.col::<DbUuid>("segmentID")?),
                snippet: row.get("snippet")?,
                rank: row.get("rank")?,
            })
        },
    )?;
    hits.extend(query_all(
        connection,
        "SELECT meeting.id AS meetingID, snippet(meeting_ft, -1, '[', ']', '…', 12) AS snippet, \
         meeting_ft.rank AS rank \
         FROM meeting_ft \
         JOIN meeting ON meeting.rowid = meeting_ft.rowid \
         WHERE meeting_ft MATCH ?1 \
         ORDER BY rank, meeting.startedAt DESC \
         LIMIT ?2",
        params![pattern, limit],
        |row| {
            Ok(SearchHit {
                meeting_id: row.col::<DbUuid>("meetingID")?,
                segment_id: None,
                snippet: row.get("snippet")?,
                rank: row.get("rank")?,
            })
        },
    )?);
    // Byte order of a UUID is the order of its upper-case string, so the
    // tie-break matches Swift's `uuidString` compare without the strings; a
    // meeting hit (no segment) sorts before its segments, as "" did.
    hits.sort_by(|left, right| {
        left.rank
            .total_cmp(&right.rank)
            .then_with(|| left.meeting_id.cmp(&right.meeting_id))
            .then_with(|| left.segment_id.cmp(&right.segment_id))
    });
    hits.truncate(usize::try_from(limit).unwrap_or(usize::MAX));
    Ok(hits)
}

impl Store {
    /// Matches every token of `query` (unicode61: case- and
    /// diacritic-insensitive, so "jerome" finds "Jérôme") over transcript
    /// segments and meeting titles and summaries, best rank first. An
    /// empty query matches nothing.
    pub fn search(&self, query: &str, limit: i64) -> Result<Vec<SearchHit>> {
        let Some(pattern) = fts5_pattern(query) else {
            return Ok(Vec::new());
        };
        self.read(|connection| search_rows(connection, &pattern, limit))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn patterns_quote_every_token() {
        assert_eq!(fts5_pattern("  "), None);
        assert_eq!(
            fts5_pattern("budget NOT* \"friday\"").as_deref(),
            Some("\"budget\" \"NOT\" \"friday\"")
        );
        assert_eq!(fts5_pattern("Jérôme").as_deref(), Some("\"Jérôme\""));
    }
}
