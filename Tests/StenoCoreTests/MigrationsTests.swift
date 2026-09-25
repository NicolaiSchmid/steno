import Foundation
import GRDB
import Testing

@testable import StenoCore

@Suite struct MigrationsTests {
  @Test func migratorRunsOnAnEmptyDatabase() throws {
    let queue = try DatabaseQueue()
    try Migrations.migrator().migrate(queue)
    try queue.read { (db) throws in
      #expect(try Migrations.migrator().appliedIdentifiers(db) == Set(Migrations.identifiers))
      #expect(Migrations.identifiers == ["v1"])
      let tables = try String.fetchAll(
        db, sql: "SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name")
      for expected in [
        "meeting", "participant", "person", "speaker", "transcriptSegment", "meetingTask",
        "decision", "audioAsset", "delivery", "pairedDevice", "handoverReceipt", "setting",
        "transcriptSegment_ft", "meeting_ft",
      ] {
        #expect(tables.contains(expected), "table \(expected)")
      }
      let violations = try Row.fetchAll(db, sql: "PRAGMA foreign_key_check")
      #expect(violations.isEmpty)
      #expect(try Bool.fetchOne(db, sql: "PRAGMA foreign_keys") == true)
    }
  }

  @Test func migratingTwiceIsANoOp() throws {
    let queue = try DatabaseQueue()
    try Migrations.migrator().migrate(queue)
    try Migrations.migrator().migrate(queue)
    try queue.read { (db) throws in
      #expect(try Migrations.migrator().appliedIdentifiers(db).count == 1)
    }
  }

  @Test func insertedSegmentIsFoundThroughFTS5() throws {
    let queue = try DatabaseQueue()
    try Migrations.migrator().migrate(queue)
    try queue.write { db in
      try MeetingRow(SampleData.meeting()).insert(db)
      for segment in SampleData.segments() {
        var plain = segment
        plain.speakerID = nil
        try TranscriptSegmentRow(plain).insert(db)
      }
      try MeetingRow(SampleData.meeting()).update(db)
    }
    try queue.read { (db) throws in
      let pattern = try #require(FTS5Pattern(matchingAllTokensIn: "Kern neunzig"))
      let ids = try String.fetchAll(
        db,
        sql: """
          SELECT transcriptSegment.id FROM transcriptSegment
          JOIN transcriptSegment_ft ON transcriptSegment_ft.rowid = transcriptSegment.rowid
          WHERE transcriptSegment_ft MATCH ?
          """,
        arguments: [pattern])
      #expect(ids == [SampleData.uuid(40).uuidString])

      let meetings = try String.fetchAll(
        db,
        sql: """
          SELECT meeting.id FROM meeting
          JOIN meeting_ft ON meeting_ft.rowid = meeting.rowid
          WHERE meeting_ft MATCH ?
          """,
        arguments: [FTS5Pattern(matchingAllTokensIn: "Zeitplan")])
      #expect(meetings == [SampleData.meetingID.uuidString])
    }
  }

  @Test func rowsRoundTripThroughTheirRecords() throws {
    let queue = try DatabaseQueue()
    try Migrations.migrator().migrate(queue)
    try queue.write { db in
      try MeetingRow(SampleData.meeting(state: .failed(reason: "boom"))).insert(db)
      for person in SampleData.persons() { try PersonRow(person).insert(db) }
      for participant in SampleData.participants() { try ParticipantRow(participant).insert(db) }
      for speaker in SampleData.speakers() { try SpeakerRow(speaker).insert(db) }
      for segment in SampleData.segments() { try TranscriptSegmentRow(segment).insert(db) }
      for task in SampleData.tasks() { try MeetingTaskRow(task).insert(db) }
      for decision in SampleData.decisions() { try DecisionRow(decision).insert(db) }
      try AudioAssetRow(SampleData.audioAsset()).insert(db)
      try DeliveryRow(SampleData.delivery()).insert(db)
      try PairedDeviceRow(SampleData.pairedDevice(), tokenHash: Data(repeating: 7, count: 32))
        .insert(db)
      try HandoverReceiptRow(SampleData.handoverReceipt()).insert(db)
    }
    try queue.read { (db) throws in
      #expect(
        try MeetingRow.fetchOne(db, key: SampleData.meetingID)?.meeting
          == SampleData.meeting(state: .failed(reason: "boom")))
      #expect(
        try PersonRow.order(PersonRow.Columns.displayName).fetchAll(db).map(\.person)
          == SampleData.persons().sorted { $0.displayName < $1.displayName })
      #expect(
        try ParticipantRow.order(ParticipantRow.Columns.id).fetchAll(db).map(\.participant)
          == SampleData.participants())
      #expect(
        try SpeakerRow.order(SpeakerRow.Columns.id).fetchAll(db).map(\.speaker)
          == SampleData.speakers())
      #expect(
        try TranscriptSegmentRow.order(TranscriptSegmentRow.Columns.start).fetchAll(db)
          .map(\.segment) == SampleData.segments())
      #expect(try MeetingTaskRow.fetchAll(db).map(\.task) == SampleData.tasks())
      #expect(try DecisionRow.fetchAll(db).map(\.decision) == SampleData.decisions())
      #expect(try AudioAssetRow.fetchAll(db).map(\.asset) == [SampleData.audioAsset()])
      #expect(try DeliveryRow.fetchAll(db).map(\.delivery) == [SampleData.delivery()])
      #expect(try PairedDeviceRow.fetchAll(db).map(\.device) == [SampleData.pairedDevice()])
      #expect(
        try HandoverReceiptRow.fetchAll(db).map(\.receipt) == [SampleData.handoverReceipt()])

      let idText = try String.fetchOne(
        db, sql: "SELECT id FROM meeting WHERE rowid = 1")
      #expect(idText == SampleData.meetingID.uuidString)
      let language = try String.fetchOne(db, sql: "SELECT language FROM meeting")
      #expect(language == "de")
      let summaryText = try String.fetchOne(db, sql: "SELECT summaryText FROM meeting")
      #expect(summaryText?.hasPrefix("Fokus: Speaker 1 schlägt vor") == true)
      let blob = try Data.fetchOne(
        db, sql: "SELECT embedding FROM person WHERE displayName = 'Nicolai'")
      #expect(blob?.count == Embedding.dimension * 4)
    }
  }

  @Test func deletingAMeetingCascades() throws {
    let queue = try DatabaseQueue()
    try Migrations.migrator().migrate(queue)
    try queue.write { db in
      try MeetingRow(SampleData.meeting()).insert(db)
      for person in SampleData.persons() { try PersonRow(person).insert(db) }
      for speaker in SampleData.speakers() { try SpeakerRow(speaker).insert(db) }
      for segment in SampleData.segments() { try TranscriptSegmentRow(segment).insert(db) }
      try AudioAssetRow(SampleData.audioAsset()).insert(db)
      _ = try MeetingRow.deleteOne(db, key: SampleData.meetingID)
    }
    try queue.read { (db) throws in
      #expect(try SpeakerRow.fetchCount(db) == 0)
      #expect(try TranscriptSegmentRow.fetchCount(db) == 0)
      #expect(try AudioAssetRow.fetchCount(db) == 0)
      #expect(try PersonRow.fetchCount(db) == 2)
      #expect(try Int.fetchOne(db, sql: "SELECT count(*) FROM transcriptSegment_ft") == 0)
    }
  }
}
