import Foundation
import GRDB

extension MeetingStore {
  /// Every known person, by display name.
  public func persons() async throws -> [Person] {
    try await writer.read { db in
      try PersonRow.order(PersonRow.Columns.displayName, PersonRow.Columns.id).fetchAll(db)
        .map(\.person)
    }
  }

  public func person(id: UUID) async throws -> Person? {
    try await writer.read { db in try Self.personRow(id, db)?.person }
  }

  public func save(_ person: Person) async throws {
    try await writer.write { db in try PersonRow(person).save(db) }
  }

  public func speakers(meetingID: UUID) async throws -> [Speaker] {
    try await writer.read { db in
      try SpeakerRow
        .filter(SpeakerRow.Columns.meetingID == meetingID.uuidString)
        .order(SpeakerRow.Columns.clusterLabel, SpeakerRow.Columns.id)
        .fetchAll(db)
        .map(\.speaker)
    }
  }

  /// The speakers of every meeting in `meetingIDs`, keyed by meeting, in one
  /// read; a meeting without speakers has no key. Within a meeting the order
  /// is `speakers(meetingID:)`'s. The meeting list reads its speaker chips
  /// through this once per list update instead of once per row.
  public func speakers(forMeetings meetingIDs: [UUID]) async throws -> [UUID: [Speaker]] {
    guard !meetingIDs.isEmpty else { return [:] }
    let keys = meetingIDs.map(\.uuidString)
    return try await writer.read { db in
      let speakers =
        try SpeakerRow
        .filter(keys.contains(SpeakerRow.Columns.meetingID))
        .order(
          SpeakerRow.Columns.meetingID, SpeakerRow.Columns.clusterLabel, SpeakerRow.Columns.id
        )
        .fetchAll(db)
        .map(\.speaker)
      return Dictionary(grouping: speakers, by: \.meetingID)
    }
  }

  public func save(_ speaker: Speaker) async throws {
    try await writer.write { db in try SpeakerRow(speaker).save(db) }
  }

  /// Nulls `sampleClipURL`, and nothing else, on exactly the named speakers
  /// of `meetingID`; `RetentionSweep` calls it once the clips of a meeting's
  /// confirmed speakers are gone with the audio. IDs of other meetings' rows
  /// are ignored.
  public func clearSampleClips(meetingID: UUID, speakerIDs: [UUID]) async throws {
    guard !speakerIDs.isEmpty else { return }
    try await writer.write { db in
      try SpeakerRow
        .filter(SpeakerRow.Columns.meetingID == meetingID.uuidString)
        .filter(speakerIDs.map(\.uuidString).contains(SpeakerRow.Columns.id))
        .updateAll(db, SpeakerRow.Columns.sampleClipURL.set(to: nil))
    }
  }

  /// People who own a confirmed speaker, most recently met first (the
  /// latest `meeting.startedAt` among their confirmed speakers), then by
  /// name; a dismissed voice owns nobody. What the speaker picker lists
  /// before anything is typed.
  public func recentPersons() async throws -> [Person] {
    try await writer.read { db in
      try PersonRow.fetchAll(
        db,
        sql: """
          SELECT person.* FROM person
          JOIN speaker ON speaker.personID = person.id AND speaker.assignment = ?
            AND speaker.dismissed = 0
          JOIN meeting ON meeting.id = speaker.meetingID
          GROUP BY person.id
          ORDER BY MAX(meeting.startedAt) DESC, person.displayName COLLATE NOCASE, person.id
          """,
        arguments: [SpeakerAssignment.Kind.confirmed.rawValue]
      ).map(\.person)
    }
  }

  /// One person recorded twice, across meetings: re-points speakers,
  /// participants and task assignees from `remove` to `keep`, takes
  /// `remove`'s email when `keep` has none, deletes `remove` and recomputes
  /// `keep`'s voice from the speakers now confirmed to them.
  public func mergePersons(keep: UUID, remove: UUID) async throws {
    guard keep != remove else { return }
    try await writer.write { db in
      guard let keepRow = try Self.personRow(keep, db) else {
        throw MeetingStoreError.personNotFound(keep)
      }
      guard let removeRow = try Self.personRow(remove, db) else {
        throw MeetingStoreError.personNotFound(remove)
      }
      var kept = keepRow.person
      if kept.email == nil { kept.email = removeRow.person.email }
      try PersonRow(kept).update(db)

      try db.execute(
        sql: "UPDATE speaker SET personID = ? WHERE personID = ?",
        arguments: [keep.uuidString, remove.uuidString])
      try db.execute(
        sql: "UPDATE participant SET personID = ? WHERE personID = ?",
        arguments: [keep.uuidString, remove.uuidString])
      try db.execute(
        sql: "UPDATE meetingTask SET assigneePersonID = ? WHERE assigneePersonID = ?",
        arguments: [keep.uuidString, remove.uuidString])
      try PersonRow.filter(PersonRow.Columns.id == remove.uuidString).deleteAll(db)
      try Self.refreshVoice(personID: keep, db)
    }
  }

  /// Two clusters inside one meeting: moves `source`'s segments to `target`,
  /// averages the embeddings, keeps `target`'s assignment (or takes
  /// `source`'s when `target` is unknown), deletes the `source` speaker and
  /// recomputes the voices of the persons involved. `source`'s sample clip
  /// moves to `target` when `target` has none and is deleted otherwise, so a
  /// range never survives without its file.
  public func mergeSpeakers(_ source: UUID, into target: UUID, meetingID: UUID) async throws {
    guard source != target else { return }
    let merge = try await writer.write { db in
      let merge = try Self.mergeSpeakerRows(source, into: target, meetingID: meetingID, db)
      for personID in merge.personIDs { try Self.refreshVoice(personID: personID, db) }
      return merge
    }
    if let clip = merge.clipToRemove { try? FileManager.default.removeItem(at: clip) }
  }

  /// What `mergeSpeakerRows` leaves for the caller: the clip file to remove
  /// once the transaction commits (nil when it moved to the target) and the
  /// persons whose voices the merge touched.
  struct SpeakerMerge {
    var clipToRemove: URL?
    var personIDs: Set<UUID>
  }

  /// The row half of `mergeSpeakers`, inside a caller's transaction. Throws
  /// for a missing speaker or one from another meeting; a self-merge is a
  /// no-op. Voices are not refreshed here.
  static func mergeSpeakerRows(
    _ source: UUID, into target: UUID, meetingID: UUID, _ db: Database
  ) throws -> SpeakerMerge {
    guard source != target else { return SpeakerMerge(clipToRemove: nil, personIDs: []) }
    guard let sourceRow = try speakerRow(source, db) else {
      throw MeetingStoreError.speakerNotFound(source)
    }
    guard let targetRow = try speakerRow(target, db) else {
      throw MeetingStoreError.speakerNotFound(target)
    }
    guard sourceRow.meetingID == meetingID, targetRow.meetingID == meetingID else {
      throw MeetingStoreError.speakersInDifferentMeetings(source, target)
    }
    let merged = sourceRow.speaker
    var kept = targetRow.speaker
    var personIDs: Set<UUID> = []
    if case .confirmed(let personID) = merged.assignment { personIDs.insert(personID) }
    switch (kept.embedding, merged.embedding) {
    case (let lhs?, let rhs?):
      kept.embedding = Embedding.weightedMean(lhs, weight: 1, rhs, weight: 1)
    case (nil, let rhs?):
      kept.embedding = rhs
    default:
      break
    }
    if case .unknown = kept.assignment { kept.assignment = merged.assignment }
    if case .confirmed(let personID) = kept.assignment { personIDs.insert(personID) }
    var clipToRemove = merged.sampleClipURL
    if kept.sampleClipRange == nil {
      kept.sampleClipRange = merged.sampleClipRange
      kept.sampleClipURL = merged.sampleClipURL
      clipToRemove = nil
    }
    kept.clusterConfidence = max(kept.clusterConfidence, merged.clusterConfidence)
    try SpeakerRow(kept).update(db)

    try db.execute(
      sql: "UPDATE transcriptSegment SET speakerID = ? WHERE speakerID = ?",
      arguments: [target.uuidString, source.uuidString])
    try SpeakerRow.filter(SpeakerRow.Columns.id == source.uuidString).deleteAll(db)
    return SpeakerMerge(clipToRemove: clipToRemove, personIDs: personIDs)
  }

  /// The person a typed or calendar name stands for: the stored person whose
  /// display name matches ignoring case and diacritics ("jerome" finds
  /// Jérôme), else a new, unsaved person with the trimmed name and `email`.
  /// Nothing is written; `confirm` saves a new person. A blank name throws
  /// `MeetingStoreError.blankPersonName`.
  public func resolvePerson(named name: String, email: String? = nil, now: Date) async throws
    -> Person
  {
    let trimmed = name.trimmingCharacters(in: .whitespacesAndNewlines)
    guard !trimmed.isEmpty else { throw MeetingStoreError.blankPersonName }
    if let existing = try await persons().first(where: {
      Person.namesMatch($0.displayName, trimmed)
    }) {
      return existing
    }
    return Person(id: UUID(), displayName: trimmed, email: email, sampleCount: 0, createdAt: now)
  }

  /// The one operation that sets `.confirmed`. Saves the person when the
  /// store does not know them, drops the speaker's name suggestion, which
  /// the confirmation answered, and any No to this person for this voice,
  /// and undoes a dismissal. Then: the same person again changes nothing
  /// else; a person who already owns another speaker of this meeting takes
  /// this speaker over (`mergeSpeakerRows`, Jamie's "add the same name
  /// twice"); otherwise the speaker becomes `.confirmed(person)`. The voices
  /// of the new person and of whoever the speaker was confirmed to before
  /// are recomputed (`refreshVoice`), so a wrong name is taken back exactly.
  /// The sample clip stays while the meeting's master recording exists, so a
  /// confirmed speaker can still be heard; once the audio is gone the clip
  /// goes with the confirmation.
  public func confirm(speakerID: UUID, person: Person) async throws {
    let clipsToRemove: [URL] = try await writer.write { db in
      guard var speaker = try Self.speakerRow(speakerID, db)?.speaker else {
        throw MeetingStoreError.speakerNotFound(speakerID)
      }
      try SpeakerNameSuggestionRow
        .filter(SpeakerNameSuggestionRow.Columns.speakerID == speakerID.uuidString)
        .deleteAll(db)
      if try Self.personRow(person.id, db) == nil {
        try PersonRow(person).insert(db)
      }
      // Naming a voice answers an earlier No to this person (on the "me"
      // speaker every one, all from Remove name) and makes a dismissed
      // voice a person again.
      var answered =
        SpeakerSuggestionRejectionRow
        .filter(SpeakerSuggestionRejectionRow.Columns.speakerID == speakerID.uuidString)
      if speakerID != LaneMerger.meSpeakerID(meetingID: speaker.meetingID) {
        answered = answered.filter(
          SpeakerSuggestionRejectionRow.Columns.personID == person.id.uuidString)
      }
      try answered.deleteAll(db)
      if speaker.dismissed {
        speaker.dismissed = false
        try SpeakerRow(speaker).update(db)
      }
      if case .confirmed(let current) = speaker.assignment, current == person.id { return [] }

      var personIDs: Set<UUID> = [person.id]
      if case .confirmed(let previous) = speaker.assignment { personIDs.insert(previous) }
      var clips: [URL] = []
      let owner =
        try SpeakerRow
        .filter(SpeakerRow.Columns.meetingID == speaker.meetingID.uuidString)
        .filter(SpeakerRow.Columns.personID == person.id.uuidString)
        .filter(SpeakerRow.Columns.assignment == SpeakerAssignment.Kind.confirmed.rawValue)
        .filter(SpeakerRow.Columns.id != speakerID.uuidString)
        .order(SpeakerRow.Columns.clusterLabel, SpeakerRow.Columns.id)
        .fetchOne(db)
      let kept: UUID
      if let owner {
        let merge = try Self.mergeSpeakerRows(
          speakerID, into: owner.id, meetingID: speaker.meetingID, db)
        if let clip = merge.clipToRemove { clips.append(clip) }
        personIDs.formUnion(merge.personIDs)
        kept = owner.id
      } else {
        speaker.assignment = .confirmed(personID: person.id)
        try SpeakerRow(speaker).update(db)
        kept = speakerID
      }
      if let clip = try Self.dropClipWhenAudioIsGone(speakerID: kept, db) { clips.append(clip) }
      for personID in personIDs { try Self.refreshVoice(personID: personID, db) }
      return clips
    }
    for clip in clipsToRemove { try? FileManager.default.removeItem(at: clip) }
  }

  /// Nulls a confirmed speaker's `sampleClipURL` and returns it for removal
  /// when the meeting's master recording no longer exists (or the meeting
  /// never had an asset); nil otherwise.
  static func dropClipWhenAudioIsGone(speakerID: UUID, _ db: Database) throws -> URL? {
    guard var speaker = try speakerRow(speakerID, db)?.speaker, let clip = speaker.sampleClipURL
    else { return nil }
    if let asset = try assetRow(meetingID: speaker.meetingID, db)?.asset,
      FileManager.default.fileExists(atPath: asset.url.path)
    {
      return nil
    }
    speaker.sampleClipURL = nil
    try SpeakerRow(speaker).update(db)
    return clip
  }

  /// How many confirmed speakers a person's voice is computed from.
  static let voiceWindow = 50

  /// Recomputes a person's voice from the speakers confirmed to them: the
  /// normalised mean of the newest `voiceWindow` embeddings by
  /// `meeting.startedAt`, `sampleCount` their number; nil and 0 without any.
  /// A dismissed voice never counts.
  /// Embeddings of another dimension than `Embedding.dimension` are skipped.
  /// A missing person is ignored.
  static func refreshVoice(personID: UUID, _ db: Database) throws {
    guard var person = try personRow(personID, db)?.person else { return }
    let rows = try SpeakerRow.fetchAll(
      db,
      sql: """
        SELECT speaker.* FROM speaker
        JOIN meeting ON meeting.id = speaker.meetingID
        WHERE speaker.personID = ? AND speaker.assignment = ? AND speaker.embedding IS NOT NULL
          AND speaker.dismissed = 0
        ORDER BY meeting.startedAt DESC, speaker.id
        """,
      arguments: [personID.uuidString, SpeakerAssignment.Kind.confirmed.rawValue])
    let samples = rows.compactMap(\.embedding)
      .filter { $0.values.count == Embedding.dimension }
      .prefix(voiceWindow)
    person.embedding = Embedding.mean(of: Array(samples))
    person.sampleCount = samples.count
    try PersonRow(person).update(db)
  }

  // MARK: - The user's other answers

  /// "Remove name", beside `confirm`: the speaker goes back to `.unknown`
  /// in this meeting. The person it was confirmed to keeps their name and
  /// has their voice recomputed without it; for the "me" speaker
  /// (`LaneMerger.meSpeakerID`) the meeting's "me" participant is unnamed
  /// again. A suggestion is dropped the same way, without a No. An unknown
  /// speaker is left alone; a missing one throws `speakerNotFound`.
  /// Clearing the "me" speaker is recorded as a No to the person it named
  /// and to the user's own person (the Rust app's `selfPersonID`), so
  /// processing again in either app leaves the mic lane unnamed; naming it
  /// again answers those Noes.
  public func clear(speakerID: UUID) async throws {
    try await writer.write { db in
      guard let previous = try Self.speakerRow(speakerID, db)?.speaker.assignment else {
        throw MeetingStoreError.speakerNotFound(speakerID)
      }
      let speaker = try Self.unassignSpeaker(speakerID, db)
      guard case .confirmed(let personID) = previous,
        speakerID == LaneMerger.meSpeakerID(meetingID: speaker.meetingID)
      else { return }
      for person in [personID] + [try Self.selfPersonID(db)].compactMap({ $0 }) {
        try Self.insertRejection(
          SpeakerSuggestionRejectionRow(
            speakerID: speakerID, personID: person, meetingID: speaker.meetingID,
            similarity: nil), db)
      }
    }
  }

  /// Who the user is in the Rust app (its `selfPersonID` setting, which
  /// this build does not read otherwise); nil when unset or unreadable.
  static func selfPersonID(_ db: Database) throws -> UUID? {
    guard
      let stored = try String.fetchOne(
        db, sql: "SELECT value FROM setting WHERE key = ?", arguments: ["selfPersonID"]),
      let data = stored.data(using: .utf8),
      let text = try? JSONDecoder().decode(String.self, from: data)
    else { return nil }
    return UUID(uuidString: text)
  }

  /// No to a suggestion: `personID` is recorded as not this voice and is not
  /// suggested for it again, here or when the meeting is processed again.
  /// The voice match's score is kept with the No: the suggested similarity,
  /// else the cosine of the voice and the person's stored voice. A speaker
  /// still suggested as that person goes back to `.unknown`. Voice memory
  /// is not touched: nobody's voice learns from a No. Throws for a missing
  /// speaker or person, and `rejectedConfirmedPerson` for the person the
  /// speaker is confirmed as (that is `clear(speakerID:)`).
  public func rejectSuggestion(speakerID: UUID, personID: UUID) async throws {
    try await writer.write { db in
      guard var speaker = try Self.speakerRow(speakerID, db)?.speaker else {
        throw MeetingStoreError.speakerNotFound(speakerID)
      }
      guard let person = try Self.personRow(personID, db)?.person else {
        throw MeetingStoreError.personNotFound(personID)
      }
      var suggested: Float?
      switch speaker.assignment {
      case .confirmed(let confirmed) where confirmed == personID:
        throw MeetingStoreError.rejectedConfirmedPerson(speakerID, personID)
      case .suggested(let suggestedID, let similarity) where suggestedID == personID:
        suggested = similarity
      default:
        break
      }
      var similarity = suggested
      if similarity == nil, let voice = speaker.embedding, let known = person.embedding {
        similarity = voice.cosineSimilarity(to: known)
      }
      try Self.insertRejection(
        SpeakerSuggestionRejectionRow(
          speakerID: speakerID, personID: personID, meetingID: speaker.meetingID,
          similarity: similarity), db)
      if suggested != nil {
        speaker.assignment = .unknown
        try SpeakerRow(speaker).update(db)
      }
    }
  }

  /// The persons the user answered No to, per speaker of the meeting.
  public func suggestionRejections(meetingID: UUID) async throws -> [UUID: Set<UUID>] {
    try await writer.read { db in try Self.suggestionRejections(meetingID: meetingID, db) }
  }

  /// "Not a person", or a person again. Dismissing also takes back a
  /// suggestion or confirmation as `clear(speakerID:)` does, so the voice
  /// leaves voice memory; its segments keep their text and their speaker.
  /// Restoring only clears the flag and leaves the voice unnamed. Throws
  /// `speakerNotFound` for a missing speaker, and `meSpeakerNotDismissable`
  /// for the "me" speaker, the user's own mic.
  public func setDismissed(speakerID: UUID, _ dismissed: Bool) async throws {
    try await writer.write { db in
      if dismissed, let stored = try Self.speakerRow(speakerID, db)?.speaker,
        speakerID == LaneMerger.meSpeakerID(meetingID: stored.meetingID)
      {
        throw MeetingStoreError.meSpeakerNotDismissable(speakerID)
      }
      var speaker: Speaker
      if dismissed {
        speaker = try Self.unassignSpeaker(speakerID, db)
      } else {
        guard let stored = try Self.speakerRow(speakerID, db)?.speaker else {
          throw MeetingStoreError.speakerNotFound(speakerID)
        }
        speaker = stored
      }
      guard speaker.dismissed != dismissed else { return }
      speaker.dismissed = dismissed
      try SpeakerRow(speaker).update(db)
    }
  }

  /// The speaker back to `.unknown`, inside a caller's transaction: the
  /// voice of the person it was confirmed to is recomputed without it, and
  /// a "me" speaker's participant is unnamed again. A speaker that is
  /// already unknown is left alone. Returns the speaker as written.
  static func unassignSpeaker(_ speakerID: UUID, _ db: Database) throws -> Speaker {
    guard var speaker = try speakerRow(speakerID, db)?.speaker else {
      throw MeetingStoreError.speakerNotFound(speakerID)
    }
    let previous = speaker.assignment
    guard previous != .unknown else { return speaker }
    speaker.assignment = .unknown
    try SpeakerRow(speaker).update(db)
    if case .confirmed(let personID) = previous {
      try refreshVoice(personID: personID, db)
      if speakerID == LaneMerger.meSpeakerID(meetingID: speaker.meetingID) {
        try unnameMeParticipant(meetingID: speaker.meetingID, db)
      }
    }
    return speaker
  }

  /// The meeting's "me" participant back to unnamed: no person, named
  /// `LaneMerger.meSpeakerLabel`. This build never names it (`confirm`
  /// leaves participants alone); the Rust host does when the user names
  /// the "me" voice, so a shared database reads the same in both.
  static func unnameMeParticipant(meetingID: UUID, _ db: Database) throws {
    try db.execute(
      sql:
        "UPDATE participant SET personID = NULL, displayName = ? WHERE meetingID = ? AND role = ?",
      arguments: [LaneMerger.meSpeakerLabel, meetingID.uuidString, ParticipantRole.me.rawValue])
  }

  /// The meeting's Noes, by speaker id then person id.
  static func rejectionRows(meetingID: UUID, _ db: Database) throws
    -> [SpeakerSuggestionRejectionRow]
  {
    try SpeakerSuggestionRejectionRow
      .filter(SpeakerSuggestionRejectionRow.Columns.meetingID == meetingID.uuidString)
      .order(
        SpeakerSuggestionRejectionRow.Columns.speakerID,
        SpeakerSuggestionRejectionRow.Columns.personID
      )
      .fetchAll(db)
  }

  static func suggestionRejections(meetingID: UUID, _ db: Database) throws -> [UUID: Set<UUID>] {
    var rejections: [UUID: Set<UUID>] = [:]
    for row in try rejectionRows(meetingID: meetingID, db) {
      rejections[row.speakerID, default: []].insert(row.personID)
    }
    return rejections
  }

  /// One No, unless already recorded or its speaker or person is gone.
  static func insertRejection(_ row: SpeakerSuggestionRejectionRow, _ db: Database) throws {
    try db.execute(
      sql: """
        INSERT OR IGNORE INTO speakerSuggestionRejection
          (speakerID, personID, meetingID, similarity)
        SELECT ?, ?, ?, ? WHERE EXISTS (SELECT 1 FROM speaker WHERE id = ?)
        AND EXISTS (SELECT 1 FROM person WHERE id = ?)
        """,
      arguments: [
        row.speakerID.uuidString, row.personID.uuidString, row.meetingID.uuidString,
        row.similarity.map { Double($0) }, row.speakerID.uuidString, row.personID.uuidString,
      ])
  }
}
