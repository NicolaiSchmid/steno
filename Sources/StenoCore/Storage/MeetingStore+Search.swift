import Foundation
import GRDB

/// One full-text hit: a segment when `segmentID` is set, otherwise the
/// meeting's title or summary. `rank` is FTS5's bm25 (lower is better).
public struct SearchHit: Sendable, Equatable, Hashable {
  public var meetingID: UUID
  public var segmentID: UUID?
  public var snippet: String
  public var rank: Double

  public init(meetingID: UUID, segmentID: UUID?, snippet: String, rank: Double) {
    self.meetingID = meetingID
    self.segmentID = segmentID
    self.snippet = snippet
    self.rank = rank
  }
}

extension MeetingStore {
  /// Matches every token of `query` (unicode61: case- and
  /// diacritic-insensitive, so "jerome" finds "Jérôme") over transcript
  /// segments and meeting titles and summaries, best rank first.
  public func search(_ query: String, limit: Int = 50) async throws -> [SearchHit] {
    guard let pattern = FTS5Pattern(matchingAllTokensIn: query) else { return [] }
    return try await writer.read { db in
      let segmentRows = try Row.fetchAll(
        db,
        sql: """
          SELECT transcriptSegment.meetingID AS meetingID, transcriptSegment.id AS segmentID,
                 snippet(transcriptSegment_ft, 0, '[', ']', '…', 12) AS snippet,
                 transcriptSegment_ft.rank AS rank
          FROM transcriptSegment_ft
          JOIN transcriptSegment ON transcriptSegment.rowid = transcriptSegment_ft.rowid
          WHERE transcriptSegment_ft MATCH ?
          ORDER BY rank, transcriptSegment.start
          LIMIT ?
          """,
        arguments: [pattern, limit])
      let meetingRows = try Row.fetchAll(
        db,
        sql: """
          SELECT meeting.id AS meetingID, snippet(meeting_ft, -1, '[', ']', '…', 12) AS snippet,
                 meeting_ft.rank AS rank
          FROM meeting_ft
          JOIN meeting ON meeting.rowid = meeting_ft.rowid
          WHERE meeting_ft MATCH ?
          ORDER BY rank, meeting.startedAt DESC
          LIMIT ?
          """,
        arguments: [pattern, limit])

      var hits: [SearchHit] = []
      for row in segmentRows {
        guard let meetingID = UUID(uuidString: row["meetingID"]),
          let segmentID = UUID(uuidString: row["segmentID"])
        else { continue }
        hits.append(
          SearchHit(
            meetingID: meetingID, segmentID: segmentID, snippet: row["snippet"], rank: row["rank"]))
      }
      for row in meetingRows {
        guard let meetingID = UUID(uuidString: row["meetingID"]) else { continue }
        hits.append(
          SearchHit(
            meetingID: meetingID, segmentID: nil, snippet: row["snippet"], rank: row["rank"]))
      }
      hits.sort { lhs, rhs in
        if lhs.rank != rhs.rank { return lhs.rank < rhs.rank }
        if lhs.meetingID != rhs.meetingID {
          return lhs.meetingID.uuidString < rhs.meetingID.uuidString
        }
        return (lhs.segmentID?.uuidString ?? "") < (rhs.segmentID?.uuidString ?? "")
      }
      return Array(hits.prefix(limit))
    }
  }
}
