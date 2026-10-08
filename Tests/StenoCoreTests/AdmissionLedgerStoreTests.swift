import Foundation
import GRDB
import Testing

@testable import StenoCore

/// The handover admission ledger (schema v5) in the store: the admission's
/// transaction writes its row, a revoke and a meeting delete leave it, every
/// open backfills it, and a migration this build does not know is ignored.
/// Rust: `crates/steno-core/tests/handover_store.rs` and the migrator's
/// tests.
@Suite struct AdmissionLedgerStoreTests {
  /// The ledger's rows as SQLite holds them: recording id, byte count,
  /// meeting id, admitted at.
  static func ledgerRows(_ store: MeetingStore) async throws -> [[String]] {
    try await store.writer.read { db in
      try Row.fetchAll(
        db,
        sql: """
          SELECT recordingID, CAST(byteCount AS TEXT) AS byteCount, meetingID, admittedAt
          FROM handoverAdmission ORDER BY admittedAt, meetingID
          """
      ).map { row -> [String] in
        [row["recordingID"], row["byteCount"], row["meetingID"], row["admittedAt"]]
      }
    }
  }

  /// The meeting `n`, its asset, and the sample receipt `.complete` with it
  /// over bytes hashing to `sha256`.
  static func admission(_ n: Int, sha256: UInt8) -> (HandoverReceipt, Meeting, AudioAsset) {
    var meeting = SampleData.meeting()
    meeting.id = SampleData.uuid(n)
    var asset = SampleData.audioAsset()
    asset.id = SampleData.uuid(n + 1000)
    asset.meetingID = meeting.id
    var receipt = SampleData.handoverReceipt()
    receipt.state = .complete(meetingID: meeting.id)
    receipt.sha256 = Data(repeating: sha256, count: 32)
    return (receipt, meeting, asset)
  }

  /// The admission's transaction writes the ledger row of the recording id,
  /// size and SHA-256 with its meeting. A revoke's cascade takes the receipt
  /// and a meeting delete takes the meeting and the receipt, and both leave
  /// the row. Other bytes under the same recording id are a second admission
  /// with a row of their own, once their receipt replaced the first one; over
  /// the first receipt they are refused. The same bytes admitted again are
  /// the meeting the ledger holds while it exists, and once it is deleted a
  /// new meeting whose admission keeps the first row. Rust:
  /// `an_admission_writes_its_ledger_row_which_a_revoke_and_a_meeting_delete_leave`.
  @Test func anAdmissionWritesItsLedgerRow() async throws {
    let store = try MeetingStore.inMemory()
    let device = SampleData.pairedDevice()
    try await store.save(device, tokenHash: Data(repeating: 1, count: 32))
    let (first, meeting, asset) = Self.admission(501, sha256: 7)
    try await store.saveDurably(first, meeting: meeting, asset: asset)
    let id = first.recordingID

    #expect(
      try await store.admittedMeeting(
        recordingID: id, byteCount: first.byteCount, sha256: first.sha256) == meeting.id)
    #expect(
      try await store.admittedMeeting(
        recordingID: id, byteCount: first.byteCount + 1, sha256: first.sha256) == nil,
      "another size")
    #expect(
      try await store.admittedMeeting(
        recordingID: id, byteCount: first.byteCount, sha256: Data(repeating: 8, count: 32))
        == nil, "another SHA-256")
    let rows = try await Self.ledgerRows(store)
    #expect(
      rows == [
        [id.uuidString, "\(first.byteCount)", meeting.id.uuidString, "2026-09-24 10:05:00.000"]
      ], "the receipt's ids, size and time, as the Rust store writes them")

    // Over the stored receipt of other bytes the admission is refused and
    // writes nothing: built from that receipt, it would say those bytes were
    // admitted.
    let (otherBytes, second, secondAsset) = Self.admission(502, sha256: 8)
    await #expect(throws: MeetingStoreError.receiptOfAnotherUpload(id)) {
      try await store.saveDurably(otherBytes, meeting: second, asset: secondAsset)
    }
    #expect(try await store.meeting(id: second.id) == nil)
    #expect(try await Self.ledgerRows(store).count == 1)
    // The announce of the other bytes saved their receipt first.
    func unfinished(_ admitted: HandoverReceipt) -> HandoverReceipt {
      var receipt = admitted
      receipt.state = .receiving
      return receipt
    }
    try await store.save(unfinished(otherBytes))
    try await store.saveDurably(otherBytes, meeting: second, asset: secondAsset)
    let (sameBytes, third, thirdAsset) = Self.admission(503, sha256: 7)
    try await store.save(unfinished(sameBytes))
    #expect(
      try await store.saveDurably(sameBytes, meeting: third, asset: thirdAsset) == meeting.id,
      "the same bytes are the meeting the ledger holds")
    #expect(try await store.meeting(id: third.id) == nil, "no second meeting")
    #expect(
      try await store.handoverReceipt(recordingID: id)?.state == .complete(meetingID: meeting.id))
    #expect(
      try await store.admittedMeeting(
        recordingID: id, byteCount: first.byteCount, sha256: Data(repeating: 8, count: 32))
        == second.id, "other bytes are an admission of their own")

    // Once the user deleted that meeting, the same bytes are admitted as a
    // new one, and the first admission's row stands.
    try await store.delete(meetingID: meeting.id)
    #expect(try await store.handoverReceipt(recordingID: id) == nil)
    try await store.save(unfinished(sameBytes))
    #expect(try await store.saveDurably(sameBytes, meeting: third, asset: thirdAsset) == third.id)
    try await store.delete(deviceID: device.id)
    #expect(try await store.handoverReceipt(recordingID: id) == nil, "the revoke cascades")
    #expect(try await Self.ledgerRows(store).count == 2, "the ledger keeps both rows")
    #expect(
      try await store.admittedMeeting(
        recordingID: id, byteCount: first.byteCount, sha256: first.sha256) == meeting.id)
  }

  /// Every open backfills the ledger from the `.complete` receipts whose
  /// meeting row exists: a database an older build left at v4 (with the rows
  /// the `v0.10.0-rc.2` intake writes: a receipt and a meeting, no ledger),
  /// and an admission an older app committed after v5 (it ignores the
  /// table). A `.complete` receipt whose meeting is missing and an
  /// unfinished one get no row. Rust:
  /// `every_open_backfills_the_ledger_from_admitted_receipts`.
  @Test func everyOpenBackfillsTheLedger() async throws {
    let directory = try Fixtures.temporaryDirectory()
    defer { try? FileManager.default.removeItem(at: directory) }
    let url = directory.appendingPathComponent("steno.sqlite")
    var store: MeetingStore? = try MeetingStore.onDisk(at: url)
    try await store!.save(SampleData.pairedDevice(), tokenHash: Data(repeating: 1, count: 32))
    let (admitted, meeting, asset) = Self.admission(501, sha256: 7)
    // The older intake's two commits, neither of which writes the ledger.
    try await store!.save(meeting, asset: asset)
    try await store!.save(admitted)
    var missing = SampleData.handoverReceipt()
    missing.recordingID = SampleData.uuid(92)
    missing.state = .complete(meetingID: SampleData.uuid(599))
    try await store!.save(missing)
    var unfinished = SampleData.handoverReceipt()
    unfinished.recordingID = SampleData.uuid(93)
    unfinished.state = .receiving
    try await store!.save(unfinished)
    // Back to v4: no table, no identifier, the rows the older build wrote.
    try await store!.writer.write { db in
      try db.execute(
        sql: """
          DROP TABLE handoverAdmission;
          DELETE FROM grdb_migrations WHERE identifier = 'v5';
          """)
    }
    store = nil

    store = try MeetingStore.onDisk(at: url)
    #expect(
      try await Self.ledgerRows(store!) == [
        [
          admitted.recordingID.uuidString, "\(admitted.byteCount)", meeting.id.uuidString,
          "2026-09-24 10:05:00.000",
        ]
      ], "the admitted receipt only, admitted at its last update")

    // An older app on the v5 database: a receipt and a meeting, no row.
    let (laterReceipt, laterMeeting, laterAsset) = Self.admission(504, sha256: 9)
    var later = laterReceipt
    later.recordingID = SampleData.uuid(94)
    try await store!.save(laterMeeting, asset: laterAsset)
    try await store!.save(later)
    #expect(
      try await store!.admittedMeeting(
        recordingID: later.recordingID, byteCount: later.byteCount, sha256: later.sha256) == nil)
    store = nil
    store = try MeetingStore.onDisk(at: url)
    #expect(
      try await store!.admittedMeeting(
        recordingID: later.recordingID, byteCount: later.byteCount, sha256: later.sha256)
        == laterMeeting.id, "the next open backfills it")
    #expect(try await Self.ledgerRows(store!).count == 2)
  }

  /// A database a newer build migrated (here to a v6 that adds a table)
  /// opens: GRDB's migrator ignores the identifier it does not know, the
  /// store does not ask `hasBeenSuperseded`, and it still reads and writes.
  /// Rust: `a_migration_this_build_does_not_know_is_ignored_with_a_warning`.
  @Test func aMigrationThisBuildDoesNotKnowIsIgnored() async throws {
    let directory = try Fixtures.temporaryDirectory()
    defer { try? FileManager.default.removeItem(at: directory) }
    let url = directory.appendingPathComponent("steno.sqlite")
    var store: MeetingStore? = try MeetingStore.onDisk(at: url)
    try await store!.writer.write { db in
      try db.execute(
        sql: """
          CREATE TABLE later (id INTEGER PRIMARY KEY);
          INSERT INTO grdb_migrations (identifier) VALUES ('v6');
          """)
    }
    store = nil

    store = try MeetingStore.onDisk(at: url)
    let applied = try await store!.writer.read { db in
      try String.fetchAll(db, sql: "SELECT identifier FROM grdb_migrations ORDER BY rowid")
    }
    #expect(applied == Migrations.identifiers + ["v6"])
    try await store!.save(SampleData.meeting())
    #expect(try await store!.meeting(id: SampleData.meetingID) != nil)
  }
}
