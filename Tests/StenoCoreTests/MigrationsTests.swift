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
      #expect(Migrations.identifiers == ["v1", "v2", "v3", "v4"])
      let tables = try String.fetchAll(
        db, sql: "SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name")
      for expected in [
        "meeting", "participant", "person", "speaker", "transcriptSegment", "meetingTask",
        "decision", "audioAsset", "delivery", "pairedDevice", "handoverReceipt", "setting",
        "transcriptSegment_ft", "meeting_ft", "speakerNameSuggestion", "stageRate",
      ] {
        #expect(tables.contains(expected), "table \(expected)")
      }
      let violations = try Row.fetchAll(db, sql: "PRAGMA foreign_key_check")
      #expect(violations.isEmpty)
      #expect(try Bool.fetchOne(db, sql: "PRAGMA foreign_keys") == true)
    }
  }

  /// Inserts `meeting` into a database that predates `v3`: the row's columns
  /// minus the two `v3` added, so the upgrade tests need no second record
  /// type per schema version.
  static func insertPreV3(_ meeting: Meeting, _ db: Database) throws {
    var columns = try MeetingRow(meeting).databaseDictionary
    columns["endReason"] = nil
    columns["titleOrigin"] = nil
    let names = columns.keys.sorted()
    try db.execute(
      sql: """
        INSERT INTO meeting (\(names.joined(separator: ", ")))
        VALUES (\(names.map { _ in "?" }.joined(separator: ", ")))
        """,
      arguments: StatementArguments(names.map { columns[$0]! }))
  }

  /// A database created before `v2` (a release that shipped `v1` alone)
  /// upgrades in place: the later versions alone are applied, every row
  /// survives, the new cascade holds, the `v3` columns read as "no reason"
  /// and "default title", `v4` leaves an empty `stageRate` table, and the
  /// schema is byte-identical to a fresh
  /// database's.
  @Test func aV1DatabaseUpgradesToTheLatestVersionKeepingItsRows() throws {
    let queue = try DatabaseQueue()
    var v1Only = DatabaseMigrator()
    v1Only.registerMigration("v1", migrate: Migrations.v1)
    try v1Only.migrate(queue)
    try queue.write { db in
      try Self.insertPreV3(SampleData.meeting(), db)
      for person in SampleData.persons() { try PersonRow(person).insert(db) }
      for speaker in SampleData.speakers() { try SpeakerRow(speaker).insert(db) }
      try AudioAssetRow(SampleData.audioAsset()).insert(db)
    }
    try queue.read { (db) throws in
      #expect(try Migrations.migrator().appliedIdentifiers(db) == ["v1"])
      #expect(try !db.tableExists("speakerNameSuggestion"))
      #expect(try !db.columns(in: "meeting").contains { $0.name == "endReason" })
      #expect(try !db.tableExists("stageRate"))
    }

    try Migrations.migrator().migrate(queue)

    try queue.read { (db) throws in
      #expect(try Migrations.migrator().appliedIdentifiers(db) == Set(Migrations.identifiers))
      let meeting = try MeetingRow.fetchOne(db, key: SampleData.meetingID)?.meeting
      // A v1 row has no origin; the v3 column's default applies.
      var expected = SampleData.meeting()
      expected.titleOrigin = .default
      #expect(meeting == expected)
      #expect(meeting?.endReason == nil)
      #expect(meeting?.titleOrigin == .default)
      #expect(
        try Row.fetchOne(db, sql: "SELECT endReason, titleOrigin FROM meeting")
          == ["endReason": nil, "titleOrigin": "default"])
      #expect(
        try SpeakerRow.order(SpeakerRow.Columns.id).fetchAll(db).map(\.speaker)
          == SampleData.speakers())
      #expect(try AudioAssetRow.fetchAll(db).map(\.asset) == [SampleData.audioAsset()])
      #expect(try SpeakerNameSuggestionRow.fetchCount(db) == 0)
      #expect(try StageRateRow.fetchCount(db) == 0)
      #expect(try Row.fetchAll(db, sql: "PRAGMA foreign_key_check").isEmpty)
      let latest = try #require(Migrations.identifiers.last)
      try Snapshot.assert(SchemaSnapshotTests.dump(db), matches: "snapshots/schema/\(latest).sql")
    }
    try queue.write { db in
      let suggestion = SpeakerNameSuggestion(
        speakerID: SampleData.speakerTwoID, name: "Jérôme", confidence: 0.8, evidence: "quote")
      try SpeakerNameSuggestionRow(suggestion, meetingID: SampleData.meetingID)?.insert(db)
      #expect(try SpeakerNameSuggestionRow.fetchCount(db) == 1)
      _ = try SpeakerRow.deleteOne(db, key: SampleData.speakerTwoID)
      #expect(try SpeakerNameSuggestionRow.fetchCount(db) == 0, "the new cascade holds")
    }
  }

  @Test func migratingTwiceIsANoOp() throws {
    let queue = try DatabaseQueue()
    try Migrations.migrator().migrate(queue)
    try Migrations.migrator().migrate(queue)
    try queue.read { (db) throws in
      #expect(
        try Migrations.migrator().appliedIdentifiers(db).count == Migrations.identifiers.count)
    }
  }

  /// A `v2` database (the first release) gains the two columns with their
  /// defaults, and a row written afterwards stores every end reason as one
  /// readable JSON text and the title origin as its raw value.
  @Test func aV2DatabaseGainsEndReasonAndTitleOrigin() throws {
    let queue = try DatabaseQueue()
    try Migrations.migrator(upTo: 2).migrate(queue)
    try queue.write { db in try Self.insertPreV3(SampleData.meeting(), db) }
    try Migrations.migrator().migrate(queue)
    try queue.write { db in
      #expect(try Migrations.migrator().appliedIdentifiers(db) == Set(Migrations.identifiers))
      // The pre-v3 insert carries no origin; the new column's default applies.
      var expected = SampleData.meeting()
      expected.titleOrigin = .default
      #expect(try MeetingRow.fetchOne(db, key: SampleData.meetingID)?.meeting == expected)
      let reasons: [RecordingEndReason] = [
        .manual, .callEnded(appName: "Zen"), .callEnded(appName: nil), .deviceLost, .quit,
        .failed,
      ]
      for (index, reason) in reasons.enumerated() {
        var meeting = SampleData.meeting()
        meeting.id = SampleData.uuid(200 + index)
        meeting.endReason = reason
        meeting.titleOrigin = .summary
        try MeetingRow(meeting).insert(db)
        #expect(try MeetingRow.fetchOne(db, key: meeting.id)?.meeting == meeting)
      }
      #expect(
        try String.fetchOne(
          db, sql: "SELECT endReason FROM meeting WHERE id = ?",
          arguments: [SampleData.uuid(201).uuidString]) == #"{"callEnded":"Zen"}"#)
      #expect(
        try String.fetchOne(
          db, sql: "SELECT endReason FROM meeting WHERE id = ?",
          arguments: [SampleData.uuid(202).uuidString]) == #""callEnded""#)
      #expect(
        try String.fetchOne(
          db, sql: "SELECT titleOrigin FROM meeting WHERE id = ?",
          arguments: [SampleData.uuid(200).uuidString]) == "summary")
      // An unknown value fails the fetch instead of becoming a default case.
      try db.execute(
        sql: "UPDATE meeting SET endReason = 'paused' WHERE id = ?",
        arguments: [SampleData.uuid(200).uuidString])
      #expect(throws: (any Error).self) {
        try MeetingRow.fetchOne(db, key: SampleData.uuid(200))
      }
      try db.execute(
        sql: "UPDATE meeting SET endReason = NULL, titleOrigin = 'oracle' WHERE id = ?",
        arguments: [SampleData.uuid(200).uuidString])
      #expect(throws: (any Error).self) {
        try MeetingRow.fetchOne(db, key: SampleData.uuid(200))
      }
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
        try ParticipantRow.order(ParticipantRow.Columns.displayName).fetchAll(db).map(\.participant)
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
