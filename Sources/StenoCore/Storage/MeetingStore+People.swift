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

  public func save(_ speaker: Speaker) async throws {
    try await writer.write { db in try SpeakerRow(speaker).save(db) }
  }

  /// One person recorded twice, across meetings: re-points speakers,
  /// participants and task assignees from `remove` to `keep`, stores the
  /// sample-count-weighted renormalised mean embedding on `keep`, and deletes
  /// `remove`.
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
      let removed = removeRow.person
      switch (kept.embedding, removed.embedding) {
      case (let lhs?, let rhs?):
        kept.embedding = Embedding.weightedMean(
          lhs, weight: Float(max(kept.sampleCount, 1)),
          rhs, weight: Float(max(removed.sampleCount, 1)))
      case (nil, let rhs?):
        kept.embedding = rhs
      default:
        break
      }
      kept.sampleCount += removed.sampleCount
      if kept.email == nil { kept.email = removed.email }
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
    }
  }

  /// Two clusters inside one meeting: moves `source`'s segments to `target`,
  /// averages the embeddings, keeps `target`'s assignment (or takes
  /// `source`'s when `target` is unknown), deletes the `source` speaker and
  /// its sample clip file.
  public func mergeSpeakers(_ source: UUID, into target: UUID, meetingID: UUID) async throws {
    guard source != target else { return }
    let clipToRemove: URL? = try await writer.write { db in
      guard let sourceRow = try Self.speakerRow(source, db) else {
        throw MeetingStoreError.speakerNotFound(source)
      }
      guard let targetRow = try Self.speakerRow(target, db) else {
        throw MeetingStoreError.speakerNotFound(target)
      }
      guard sourceRow.meetingID == meetingID, targetRow.meetingID == meetingID else {
        throw MeetingStoreError.speakersInDifferentMeetings(source, target)
      }
      let merged = sourceRow.speaker
      var kept = targetRow.speaker
      switch (kept.embedding, merged.embedding) {
      case (let lhs?, let rhs?):
        kept.embedding = Embedding.weightedMean(lhs, weight: 1, rhs, weight: 1)
      case (nil, let rhs?):
        kept.embedding = rhs
      default:
        break
      }
      if case .unknown = kept.assignment { kept.assignment = merged.assignment }
      if kept.sampleClipRange == nil { kept.sampleClipRange = merged.sampleClipRange }
      kept.clusterConfidence = max(kept.clusterConfidence, merged.clusterConfidence)
      try SpeakerRow(kept).update(db)

      try db.execute(
        sql: "UPDATE transcriptSegment SET speakerID = ? WHERE speakerID = ?",
        arguments: [target.uuidString, source.uuidString])
      try SpeakerRow.filter(SpeakerRow.Columns.id == source.uuidString).deleteAll(db)
      return merged.sampleClipURL
    }
    if let clipToRemove { try? FileManager.default.removeItem(at: clipToRemove) }
  }

  /// The one operation that sets `.confirmed`: saves the person (new or
  /// existing), confirms the speaker, enrols the speaker's embedding through
  /// `memory`, and deletes the sample clip.
  public func confirm(speakerID: UUID, person: Person, memory: any SpeakerMemory) async throws {
    let (embedding, clip): (Embedding?, URL?) = try await writer.write { db in
      guard var speaker = try Self.speakerRow(speakerID, db)?.speaker else {
        throw MeetingStoreError.speakerNotFound(speakerID)
      }
      if try Self.personRow(person.id, db) == nil {
        try PersonRow(person).insert(db)
      }
      speaker.assignment = .confirmed(personID: person.id)
      let clip = speaker.sampleClipURL
      speaker.sampleClipURL = nil
      try SpeakerRow(speaker).update(db)
      return (speaker.embedding, clip)
    }
    if let embedding {
      try await memory.enroll(embedding, as: person)
    }
    if let clip { try? FileManager.default.removeItem(at: clip) }
  }
}
