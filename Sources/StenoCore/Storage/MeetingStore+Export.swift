import Foundation
import GRDB

extension MeetingStore {
  /// Everything an adapter receives, read in one transaction.
  public func export(meetingID: UUID) async throws -> MeetingExport {
    try await writer.read { db in
      guard let export = try Self.exportRows(meetingID: meetingID, db) else {
        throw MeetingStoreError.meetingNotFound(meetingID)
      }
      return export
    }
  }

  static func exportRows(meetingID: UUID, _ db: Database) throws -> MeetingExport? {
    guard let meeting = try meetingRow(meetingID, db)?.meeting else { return nil }
    let key = meetingID.uuidString
    let participants =
      try ParticipantRow
      .filter(ParticipantRow.Columns.meetingID == key)
      .order(ParticipantRow.Columns.displayName, ParticipantRow.Columns.id)
      .fetchAll(db)
      .map(\.participant)
    let speakers =
      try SpeakerRow
      .filter(SpeakerRow.Columns.meetingID == key)
      .order(SpeakerRow.Columns.clusterLabel, SpeakerRow.Columns.id)
      .fetchAll(db)
      .map(\.speaker)
    let segments =
      try TranscriptSegmentRow
      .filter(TranscriptSegmentRow.Columns.meetingID == key)
      .order(TranscriptSegmentRow.Columns.start, TranscriptSegmentRow.Columns.id)
      .fetchAll(db)
      .map(\.segment)
    let tasks =
      try MeetingTaskRow
      .filter(MeetingTaskRow.Columns.meetingID == key)
      .order(Column("id"))
      .fetchAll(db)
      .map(\.task)
    let decisions =
      try DecisionRow
      .filter(DecisionRow.Columns.meetingID == key)
      .order(Column("id"))
      .fetchAll(db)
      .map(\.decision)

    var personIDs = Set<UUID>()
    for speaker in speakers { if let id = speaker.personID { personIDs.insert(id) } }
    for participant in participants { if let id = participant.personID { personIDs.insert(id) } }
    for task in tasks { if let id = task.assigneePersonID { personIDs.insert(id) } }
    let persons =
      try PersonRow
      .filter(personIDs.map(\.uuidString).contains(PersonRow.Columns.id))
      .order(PersonRow.Columns.displayName, PersonRow.Columns.id)
      .fetchAll(db)
      .map(\.person)

    return MeetingExport(
      meeting: meeting,
      participants: participants,
      speakers: speakers,
      persons: persons,
      segments: segments,
      tasks: tasks,
      decisions: decisions,
      audio: try assetRow(meetingID: meetingID, db)?.asset
    )
  }
}
