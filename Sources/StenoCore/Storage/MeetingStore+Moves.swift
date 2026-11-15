import Foundation
import GRDB

/// A speaker combined into another of its meeting (`MeetingStore.confirm`,
/// `mergeSpeakers`): its own row, kept as it was, the voice it joined
/// (`into`) and the voice it reads as (`root`, the end of the chain). Rust:
/// `MergedSpeaker`.
public struct MergedSpeaker: Sendable, Equatable, Hashable {
  public var speaker: Speaker
  public var into: UUID
  public var root: UUID

  public init(speaker: Speaker, into: UUID, root: UUID) {
    self.speaker = speaker
    self.into = into
    self.root = root
  }
}

/// One segment of a paragraph `moveTurn` moves, with the voice the caller
/// showed it under (nil for a paragraph without a voice).
public struct TurnSegmentMove: Sendable, Equatable, Hashable {
  public var segmentID: UUID
  public var shownSpeakerID: UUID?

  public init(segmentID: UUID, shownSpeakerID: UUID?) {
    self.segmentID = segmentID
    self.shownSpeakerID = shownSpeakerID
  }
}

extension Speaker {
  /// The person this speaker is confirmed as, nil unless `.confirmed`.
  var confirmedPersonID: UUID? {
    if case .confirmed(let personID) = assignment { return personID }
    return nil
  }
}

// Undoable combining and paragraph moves
// (`.plans/2026-11-14-speaker-identification.md`, step 4), the same rules as
// the Rust store's (`people.rs`): both write one database file until the
// Mac cutover, so a combine or a move written by one undoes in the other.
extension MeetingStore {
  /// Every speaker of the meeting combined into another, by cluster label,
  /// then id: each with the voice it joined and the voice it reads as.
  /// Combines from before v7 deleted their source and are not listed.
  public func mergedSpeakers(meetingID: UUID) async throws -> [MergedSpeaker] {
    try await writer.read { db in try Self.mergedSpeakers(meetingID: meetingID, db) }
  }

  static func mergedSpeakers(meetingID: UUID, _ db: Database) throws -> [MergedSpeaker] {
    let rows =
      try SpeakerRow
      .filter(SpeakerRow.Columns.meetingID == meetingID.uuidString)
      .filter(SpeakerRow.Columns.mergedInto != nil)
      .order(SpeakerRow.Columns.clusterLabel, SpeakerRow.Columns.id)
      .fetchAll(db)
    return try rows.compactMap { row in
      guard let into = try mergedInto(row.id, db) else { return nil }
      return MergedSpeaker(speaker: row.speaker, into: into, root: try rootOf(row.id, db))
    }
  }

  /// How many transcripts processing has written for the meeting:
  /// `replaceTranscript` raises it, and `moveTurn` is refused against an
  /// older one. Throws `meetingNotFound` for a missing meeting.
  public func transcriptGeneration(meetingID: UUID) async throws -> Int {
    try await writer.read { db in
      guard let generation = try Self.transcriptGeneration(meetingID: meetingID, db) else {
        throw MeetingStoreError.meetingNotFound(meetingID)
      }
      return generation
    }
  }

  static func transcriptGeneration(meetingID: UUID, _ db: Database) throws -> Int? {
    try Int.fetchOne(
      db, sql: "SELECT transcriptGeneration FROM meeting WHERE id = ?",
      arguments: [meetingID.uuidString])
  }

  /// Undo a combine, and "Separate again": the combined speaker is its own
  /// voice again, exactly as it was before (its row was kept), with the
  /// paragraphs that still carry it or a speaker combined into it; a
  /// paragraph moved away since stays where it was moved. The voices of the
  /// person it read as and of its own confirmed person are recomputed. For
  /// the "me" speaker the meeting's "me" participant follows the speaker's
  /// own assignment again: confirmed as a person names it after them,
  /// anything else unnames it. Throws `notCombined` when the speaker is
  /// missing or not combined, and `staleTranscript` when neither it nor any
  /// speaker combined into it owns a segment any more.
  public func separateSpeaker(_ speakerID: UUID) async throws {
    try await writer.write { db in
      guard let speaker = try Self.speakerRow(speakerID, db)?.speaker,
        try Self.mergedInto(speakerID, db) != nil
      else { throw MeetingStoreError.notCombined(speakerID) }
      guard try Self.ownsSegments(speakerID, db) else {
        throw MeetingStoreError.staleTranscript
      }
      let root = try Self.rootOf(speakerID, db)
      var personIDs: Set<UUID> = []
      if let personID = speaker.confirmedPersonID { personIDs.insert(personID) }
      if let personID = try Self.speakerRow(root, db)?.speaker.confirmedPersonID {
        personIDs.insert(personID)
      }
      try db.execute(
        sql: "UPDATE speaker SET mergedInto = NULL WHERE id = ?", arguments: [speakerID.uuidString])
      if speakerID == LaneMerger.meSpeakerID(meetingID: speaker.meetingID) {
        if let personID = speaker.confirmedPersonID,
          let person = try Self.personRow(personID, db)?.person
        {
          try Self.nameMeParticipant(meetingID: speaker.meetingID, person: person, db)
        } else {
          try Self.unnameMeParticipant(meetingID: speaker.meetingID, db)
        }
      }
      for personID in personIDs { try Self.refreshVoice(personID: personID, db) }
    }
  }

  /// Whether the speaker, or a speaker combined into it at any depth, still
  /// carries a segment.
  static func ownsSegments(_ speakerID: UUID, _ db: Database) throws -> Bool {
    try Bool.fetchOne(
      db,
      sql: """
        WITH RECURSIVE tree(id, depth) AS (
          SELECT ?, 0
          UNION ALL
          SELECT speaker.id, tree.depth + 1 FROM speaker
          JOIN tree ON speaker.mergedInto = tree.id WHERE tree.depth < \(mergeDepth))
        SELECT EXISTS (SELECT 1 FROM transcriptSegment WHERE speakerID IN (SELECT id FROM tree))
        """,
      arguments: [speakerID.uuidString]) ?? false
  }

  /// "This paragraph": every listed segment moves to `person`'s voice in the
  /// meeting, in one transaction, and the destination's id is returned.
  ///
  /// Refused with `staleTranscript`, changing nothing, unless the meeting
  /// exists, `generation` is its `transcriptGeneration`, `segments` is not
  /// empty, and every segment is the meeting's and still reads as the voice
  /// the caller showed (nil for nil). The person is saved when the store
  /// does not know them. The destination is the meeting's root speaker
  /// confirmed as the person (the first by cluster label); without one, a
  /// new speaker: the next free "Speaker N" label, confirmed as the person,
  /// with no embedding, no clip and confidence 0, so the move never trains
  /// anybody's voice. When every segment already reads as the destination
  /// nothing changes. Then the segments carry the destination, and every
  /// speaker the move emptied (a segment's previous speaker, then the voice
  /// a pruned one was combined into) is deleted once no segment carries it
  /// and no speaker is combined into it, with its suggestion, its Noes and,
  /// after the commit, its clip file. The voices that lost a deleted
  /// speaker are recomputed: its own confirmed person's and, for a combined
  /// one, the person of the voice it read as. A move changes no embedding.
  @discardableResult
  public func moveTurn(
    meetingID: UUID, generation: Int, segments: [TurnSegmentMove], to person: Person
  ) async throws -> UUID {
    let (destination, clips): (UUID, [URL]) = try await writer.write { db in
      guard let current = try Self.transcriptGeneration(meetingID: meetingID, db),
        current == generation, !segments.isEmpty
      else { throw MeetingStoreError.staleTranscript }
      let roots = try Self.speakerRoots(meetingID: meetingID, db)
      var previous: [UUID] = []
      var resolved: [UUID?] = []
      for move in segments {
        guard
          let row =
            try TranscriptSegmentRow
            .filter(TranscriptSegmentRow.Columns.id == move.segmentID.uuidString)
            .filter(TranscriptSegmentRow.Columns.meetingID == meetingID.uuidString)
            .fetchOne(db)
        else { throw MeetingStoreError.staleTranscript }
        let shown = row.speakerID.map { roots[$0] ?? $0 }
        guard shown == move.shownSpeakerID else { throw MeetingStoreError.staleTranscript }
        if let speakerID = row.speakerID, !previous.contains(speakerID) {
          previous.append(speakerID)
        }
        resolved.append(shown)
      }
      if try Self.personRow(person.id, db) == nil {
        try PersonRow(person).insert(db)
      }
      let destination: UUID
      if let owner = try Self.owner(meetingID: meetingID, personID: person.id, except: nil, db) {
        destination = owner.id
      } else {
        let speaker = Speaker(
          id: UUID(), meetingID: meetingID,
          clusterLabel: try Self.nextSpeakerLabel(meetingID: meetingID, db),
          assignment: .confirmed(personID: person.id), clusterConfidence: 0)
        try SpeakerRow(speaker).insert(db)
        destination = speaker.id
      }
      if resolved.allSatisfy({ $0 == destination }) { return (destination, []) }

      let ids = segments.map(\.segmentID.uuidString)
      try TranscriptSegmentRow
        .filter(TranscriptSegmentRow.Columns.meetingID == meetingID.uuidString)
        .filter(ids.contains(TranscriptSegmentRow.Columns.id))
        .updateAll(db, TranscriptSegmentRow.Columns.speakerID.set(to: destination.uuidString))
      let pruned = try Self.pruneEmptiedSpeakers(previous, keeping: destination, db)
      for personID in pruned.personIDs { try Self.refreshVoice(personID: personID, db) }
      return (destination, pruned.clips)
    }
    for clip in clips { try? FileManager.default.removeItem(at: clip) }
    return destination
  }

  /// What `pruneEmptiedSpeakers` deleted: the clip files to remove after
  /// the commit and the persons whose voices lost a speaker.
  struct PrunedSpeakers {
    var clips: [URL] = []
    var personIDs: Set<UUID> = []
  }

  /// Deletes each candidate, and then the voice each deleted one was
  /// combined into, while no segment carries it and no speaker is combined
  /// into it; `keeping` is never deleted.
  static func pruneEmptiedSpeakers(_ candidates: [UUID], keeping: UUID, _ db: Database) throws
    -> PrunedSpeakers
  {
    var pruned = PrunedSpeakers()
    var queue = candidates
    while !queue.isEmpty {
      let candidate = queue.removeFirst()
      guard candidate != keeping, let row = try speakerRow(candidate, db) else { continue }
      let inUse =
        try Bool.fetchOne(
          db,
          sql: """
            SELECT EXISTS (SELECT 1 FROM transcriptSegment WHERE speakerID = ?)
              OR EXISTS (SELECT 1 FROM speaker WHERE mergedInto = ?)
            """,
          arguments: [candidate.uuidString, candidate.uuidString]) ?? true
      if inUse { continue }
      let former = try mergedInto(candidate, db)
      if let personID = row.speaker.confirmedPersonID { pruned.personIDs.insert(personID) }
      if former != nil,
        let personID = try speakerRow(try rootOf(candidate, db), db)?.speaker.confirmedPersonID
      {
        pruned.personIDs.insert(personID)
      }
      if let clip = row.sampleClipURL { pruned.clips.append(clip) }
      try SpeakerRow.filter(SpeakerRow.Columns.id == candidate.uuidString).deleteAll(db)
      if let former { queue.append(former) }
    }
    return pruned
  }

  /// "Speaker N" with N one more than the largest among the meeting's
  /// `Speaker <digits>` labels, combined speakers included; "Speaker 1"
  /// without any.
  static func nextSpeakerLabel(meetingID: UUID, _ db: Database) throws -> String {
    let labels = try String.fetchAll(
      db, sql: "SELECT clusterLabel FROM speaker WHERE meetingID = ?",
      arguments: [meetingID.uuidString])
    let prefix = "Speaker "
    let numbers = labels.compactMap { label -> Int? in
      guard label.hasPrefix(prefix) else { return nil }
      let digits = label.dropFirst(prefix.count)
      guard !digits.isEmpty, digits.allSatisfy({ $0.isASCII && $0.isNumber }) else { return nil }
      return Int(digits)
    }
    return "\(prefix)\(min(numbers.max() ?? 0, Int.max - 1) + 1)"
  }
}
