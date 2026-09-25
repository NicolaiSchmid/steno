import Foundation
import GRDB

/// Errors a store call can raise beyond GRDB's own.
public enum MeetingStoreError: Error, Sendable, Equatable {
  case meetingNotFound(UUID)
  case speakerNotFound(UUID)
  case personNotFound(UUID)
  case speakersInDifferentMeetings(UUID, UUID)
}

/// The one store over the GRDB database. A `Sendable` final class, not an
/// actor: the pool already serialises writes and an actor would serialise
/// reads too. Every write is `save`; row changes reach the app through the
/// `observe*` streams.
public final class MeetingStore: Sendable {
  public let writer: any DatabaseWriter

  /// Runs the migrator on `writer`.
  public init(writer: any DatabaseWriter) throws {
    self.writer = writer
    try Migrations.migrator().migrate(writer)
  }

  /// A `DatabasePool` in WAL mode with a five-second busy timeout, creating
  /// the parent directory when needed. The app and the CLI use this.
  public static func onDisk(at url: URL) throws -> MeetingStore {
    try FileManager.default.createDirectory(
      at: url.deletingLastPathComponent(), withIntermediateDirectories: true)
    var configuration = Configuration()
    configuration.busyMode = .timeout(5)
    let pool = try DatabasePool(path: url.path, configuration: configuration)
    return try MeetingStore(writer: pool)
  }

  /// A private in-memory `DatabaseQueue`. Tests use this.
  public static func inMemory() throws -> MeetingStore {
    try MeetingStore(writer: DatabaseQueue())
  }

  // MARK: - Meetings

  public func save(_ meeting: Meeting) async throws {
    try await writer.write { db in try MeetingRow(meeting).save(db) }
  }

  /// The meeting and its asset in one transaction; `ProcessingPipeline.enqueue`.
  public func save(_ meeting: Meeting, asset: AudioAsset) async throws {
    try await writer.write { db in
      try MeetingRow(meeting).save(db)
      try AudioAssetRow(asset).save(db)
    }
  }

  public func meeting(id: UUID) async throws -> Meeting? {
    try await writer.read { db in try Self.meetingRow(id, db)?.meeting }
  }

  /// Newest first by `startedAt`.
  public func meetings(limit: Int = 100, offset: Int = 0) async throws -> [Meeting] {
    try await writer.read { db in
      try MeetingRow
        .order(MeetingRow.Columns.startedAt.desc, MeetingRow.Columns.id)
        .limit(limit, offset: offset)
        .fetchAll(db)
        .map(\.meeting)
    }
  }

  /// Read-modify-write in one transaction: `mutate` sees the row as it is
  /// now, not as a caller read it earlier, so two writers editing different
  /// columns never revert each other. `updatedAt` is set to `now`. Returns the
  /// meeting as written.
  @discardableResult
  public func update(
    meetingID: UUID, now: Date, _ mutate: @Sendable (inout Meeting) throws -> Void
  ) async throws -> Meeting {
    try await writer.write { db in
      var meeting = try Self.currentMeeting(meetingID, db)
      try mutate(&meeting)
      meeting.updatedAt = now
      try MeetingRow(meeting).update(db)
      return meeting
    }
  }

  public func setState(_ state: MeetingState, meetingID: UUID, now: Date) async throws {
    try await update(meetingID: meetingID, now: now) { $0.state = state }
  }

  /// One transaction: the meeting's processing columns (see
  /// `Meeting.applyProcessingResults`) plus every speaker and segment of the
  /// meeting, replaced. The pipeline's merge and cleanup stages call this.
  public func replaceTranscript(
    _ meeting: Meeting, segments: [TranscriptSegment], speakers: [Speaker]
  ) async throws {
    let meetingID = meeting.id
    try await writer.write { db in
      try Self.writeProcessingResults(of: meeting, db)
      try TranscriptSegmentRow
        .filter(TranscriptSegmentRow.Columns.meetingID == meetingID.uuidString)
        .deleteAll(db)
      try SpeakerRow.filter(SpeakerRow.Columns.meetingID == meetingID.uuidString).deleteAll(db)
      for speaker in speakers {
        var speaker = speaker
        speaker.meetingID = meetingID
        try SpeakerRow(speaker).insert(db)
      }
      for segment in segments {
        var segment = segment
        segment.meetingID = meetingID
        try TranscriptSegmentRow(segment).insert(db)
      }
    }
  }

  /// One transaction: the meeting's processing columns (summary JSON and
  /// `summaryText` among them) plus the meeting's tasks and decisions,
  /// replaced. Decision ids derive from the meeting id so re-runs are stable.
  public func replaceSummary(_ meeting: Meeting, tasks: [MeetingTask], decisions: [String])
    async throws
  {
    let meetingID = meeting.id
    try await writer.write { db in
      try Self.writeProcessingResults(of: meeting, db)
      try MeetingTaskRow.filter(MeetingTaskRow.Columns.meetingID == meetingID.uuidString)
        .deleteAll(db)
      for task in tasks {
        var task = task
        task.meetingID = meetingID
        try MeetingTaskRow(task).insert(db)
      }
      try DecisionRow.filter(DecisionRow.Columns.meetingID == meetingID.uuidString).deleteAll(db)
      for (index, text) in decisions.enumerated() {
        let decision = Decision(
          id: Self.derivedID(meetingID, salt: "decision-\(index)"), meetingID: meetingID, text: text
        )
        try DecisionRow(decision).insert(db)
      }
    }
  }

  /// Reads the row and overlays `results`' processing columns, so a stage
  /// that started minutes ago never writes back the scratchpad or tags it
  /// read then.
  static func writeProcessingResults(of results: Meeting, _ db: Database) throws {
    var meeting = try currentMeeting(results.id, db)
    meeting.applyProcessingResults(results)
    try MeetingRow(meeting).update(db)
  }

  static func currentMeeting(_ id: UUID, _ db: Database) throws -> Meeting {
    guard let row = try meetingRow(id, db) else { throw MeetingStoreError.meetingNotFound(id) }
    return row.meeting
  }

  // MARK: - Participants

  public func save(_ participant: Participant) async throws {
    try await writer.write { db in try ParticipantRow(participant).save(db) }
  }

  public func participants(meetingID: UUID) async throws -> [Participant] {
    try await writer.read { db in
      try ParticipantRow
        .filter(ParticipantRow.Columns.meetingID == meetingID.uuidString)
        .order(ParticipantRow.Columns.displayName, ParticipantRow.Columns.id)
        .fetchAll(db)
        .map(\.participant)
    }
  }

  // MARK: - Audio assets

  public func save(_ asset: AudioAsset) async throws {
    try await writer.write { db in try AudioAssetRow(asset).save(db) }
  }

  public func asset(id: UUID) async throws -> AudioAsset? {
    try await writer.read { db in
      try AudioAssetRow.filter(AudioAssetRow.Columns.id == id.uuidString).fetchOne(db)?.asset
    }
  }

  public func asset(meetingID: UUID) async throws -> AudioAsset? {
    try await writer.read { db in try Self.assetRow(meetingID: meetingID, db)?.asset }
  }

  /// Assets whose `expiresAt` is at or before `now`; the retention sweep's
  /// one query.
  public func expiredAssets(now: Date) async throws -> [AudioAsset] {
    try await writer.read { db in
      try AudioAssetRow
        .filter(AudioAssetRow.Columns.expiresAt != nil && AudioAssetRow.Columns.expiresAt <= now)
        .order(AudioAssetRow.Columns.expiresAt, AudioAssetRow.Columns.id)
        .fetchAll(db)
        .map(\.asset)
    }
  }

  // MARK: - Deliveries

  /// One row per (meeting, destination): a delivery with the same pair but
  /// another id replaces the old row.
  public func save(_ delivery: Delivery) async throws {
    try await writer.write { db in
      try DeliveryRow
        .filter(DeliveryRow.Columns.meetingID == delivery.meetingID.uuidString)
        .filter(DeliveryRow.Columns.destinationID == delivery.destinationID)
        .filter(Column("id") != delivery.id.uuidString)
        .deleteAll(db)
      try DeliveryRow(delivery).save(db)
    }
  }

  public func deliveries(meetingID: UUID) async throws -> [Delivery] {
    try await writer.read { db in try Self.deliveryRows(meetingID: meetingID, db).map(\.delivery) }
  }

  // MARK: - Observation

  /// Newest first; emits the current list, then again after every change.
  public func observeMeetings() -> AsyncThrowingStream<[Meeting], any Error> {
    writer.stream(
      ValueObservation.tracking { db in
        try MeetingRow
          .order(MeetingRow.Columns.startedAt.desc, MeetingRow.Columns.id)
          .fetchAll(db)
          .map(\.meeting)
      })
  }

  /// The full export of one meeting, nil when the meeting does not exist.
  public func observeMeeting(id: UUID) -> AsyncThrowingStream<MeetingExport?, any Error> {
    writer.stream(ValueObservation.tracking { db in try Self.exportRows(meetingID: id, db) })
  }

  public func observeDeliveries(meetingID: UUID) -> AsyncThrowingStream<[Delivery], any Error> {
    writer.stream(
      ValueObservation.tracking { db in
        try Self.deliveryRows(meetingID: meetingID, db).map(\.delivery)
      })
  }

  // MARK: - Maintenance

  /// Rebuilds both FTS5 indexes from their content tables.
  public func rebuildSearchIndex() async throws {
    try await writer.write { db in
      try db.execute(
        sql: "INSERT INTO transcriptSegment_ft(transcriptSegment_ft) VALUES('rebuild')")
      try db.execute(sql: "INSERT INTO meeting_ft(meeting_ft) VALUES('rebuild')")
    }
  }

  // MARK: - Shared row lookups

  static func meetingRow(_ id: UUID, _ db: Database) throws -> MeetingRow? {
    try MeetingRow.filter(MeetingRow.Columns.id == id.uuidString).fetchOne(db)
  }

  static func speakerRow(_ id: UUID, _ db: Database) throws -> SpeakerRow? {
    try SpeakerRow.filter(SpeakerRow.Columns.id == id.uuidString).fetchOne(db)
  }

  static func personRow(_ id: UUID, _ db: Database) throws -> PersonRow? {
    try PersonRow.filter(PersonRow.Columns.id == id.uuidString).fetchOne(db)
  }

  static func assetRow(meetingID: UUID, _ db: Database) throws -> AudioAssetRow? {
    try AudioAssetRow
      .filter(AudioAssetRow.Columns.meetingID == meetingID.uuidString)
      .order(AudioAssetRow.Columns.id)
      .fetchOne(db)
  }

  static func deliveryRows(meetingID: UUID, _ db: Database) throws -> [DeliveryRow] {
    try DeliveryRow
      .filter(DeliveryRow.Columns.meetingID == meetingID.uuidString)
      .order(DeliveryRow.Columns.destinationID)
      .fetchAll(db)
  }

  /// A UUID derived from another and a salt, so rows the store creates on
  /// the meeting's behalf (decisions) keep stable ids across re-runs.
  static func derivedID(_ base: UUID, salt: String) -> UUID {
    var hash: UInt64 = 0xcbf2_9ce4_8422_2325
    for byte in Array(base.uuidString.utf8) + Array(salt.utf8) {
      hash ^= UInt64(byte)
      hash = hash &* 0x0000_0100_0000_01b3
    }
    var second = hash ^ 0x9e37_79b9_7f4a_7c15
    second = second &* 0xbf58_476d_1ce4_e5b9
    second ^= second >> 31
    let bytes =
      withUnsafeBytes(of: hash.bigEndian, Array.init)
      + withUnsafeBytes(of: second.bigEndian, Array.init)
    var uuid = uuid_t(
      bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7], bytes[8],
      bytes[9], bytes[10], bytes[11], bytes[12], bytes[13], bytes[14], bytes[15])
    uuid.6 = (uuid.6 & 0x0F) | 0x40
    uuid.8 = (uuid.8 & 0x3F) | 0x80
    return UUID(uuid: uuid)
  }
}
