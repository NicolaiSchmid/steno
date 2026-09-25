import Foundation
import GRDB

/// Errors a store call can raise beyond GRDB's own.
public enum MeetingStoreError: Error, Sendable, Equatable, CustomStringConvertible {
  case meetingNotFound(UUID)
  case speakerNotFound(UUID)
  case personNotFound(UUID)
  case speakersInDifferentMeetings(UUID, UUID)
  /// `delete(meetingID:)` while the capture writer or the pipeline still
  /// holds the meeting's files.
  case meetingBusy(UUID, MeetingState.Kind)

  public var description: String {
    switch self {
    case .meetingNotFound(let id): "meeting \(id) not found"
    case .meetingBusy(let id, let state): "meeting \(id) is \(state.rawValue) and cannot be deleted"
    case .speakerNotFound(let id): "speaker \(id) not found"
    case .personNotFound(let id): "person \(id) not found"
    case .speakersInDifferentMeetings(let a, let b):
      "speakers \(a) and \(b) belong to different meetings"
    }
  }
}

/// The one store over the GRDB database. A `Sendable` final class, not an
/// actor: the pool already serialises writes and an actor would serialise
/// reads too. Every write is `save`; row changes reach the app through the
/// `observe*` streams, and the few things a row change cannot say (a
/// deletion, see `delete(meetingID:)`) are posted on `events`. The pipeline
/// posts on the same bus when built with `PipelineDependencies` from this
/// store, so the app subscribes once.
public final class MeetingStore: Sendable {
  public let writer: any DatabaseWriter
  public let events: MeetingEventBus

  /// Runs the migrator on `writer`.
  public init(writer: any DatabaseWriter, events: MeetingEventBus = MeetingEventBus()) throws {
    self.writer = writer
    self.events = events
    try Migrations.migrator().migrate(writer)
  }

  /// A `DatabasePool` in WAL mode with a five-second busy timeout, creating
  /// the parent directory when needed. The app and the CLI use this.
  public static func onDisk(at url: URL, events: MeetingEventBus = MeetingEventBus()) throws
    -> MeetingStore
  {
    try FileManager.default.createDirectory(
      at: url.deletingLastPathComponent(), withIntermediateDirectories: true)
    var configuration = Configuration()
    configuration.busyMode = .timeout(5)
    let pool = try DatabasePool(path: url.path, configuration: configuration)
    return try MeetingStore(writer: pool, events: events)
  }

  /// A private in-memory `DatabaseQueue`. Tests use this.
  public static func inMemory(events: MeetingEventBus = MeetingEventBus()) throws -> MeetingStore {
    try MeetingStore(writer: DatabaseQueue(), events: events)
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

  /// The meeting and its participants in one transaction;
  /// `LocalRecordingIntake.begin`.
  public func save(_ meeting: Meeting, participants: [Participant]) async throws {
    try await writer.write { db in
      try MeetingRow(meeting).save(db)
      for participant in participants {
        var participant = participant
        participant.meetingID = meeting.id
        try ParticipantRow(participant).save(db)
      }
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

  /// Every meeting in one of `kinds`, oldest first by `startedAt`: the order
  /// a queue is worked off in. `ProcessingPipeline.resumeUnfinished` reads
  /// `.queued` and `.processing` through this.
  public func meetings(inStates kinds: Set<MeetingState.Kind>) async throws -> [Meeting] {
    try await writer.read { db in
      try MeetingRow
        .filter(kinds.map(\.rawValue).contains(MeetingRow.Columns.state))
        .order(MeetingRow.Columns.startedAt, MeetingRow.Columns.id)
        .fetchAll(db)
        .map(\.meeting)
    }
  }

  /// Launch reconciliation: a meeting still `.recording` belongs to a
  /// process that died mid-meeting, since the capture session that owned it
  /// is gone. One transaction marks every such row `.failed(reason)` with
  /// `updatedAt = now` and returns their ids, oldest first. The master file,
  /// if the writer got that far, stays in the meeting folder.
  @discardableResult
  public func failInterruptedRecordings(
    reason: String = "Recording was interrupted before it finished.", now: Date
  ) async throws -> [UUID] {
    try await writer.write { db in
      let rows =
        try MeetingRow
        .filter(MeetingRow.Columns.state == MeetingState.Kind.recording.rawValue)
        .order(MeetingRow.Columns.startedAt, MeetingRow.Columns.id)
        .fetchAll(db)
      for row in rows {
        var meeting = row.meeting
        meeting.state = .failed(reason: reason)
        meeting.updatedAt = now
        try MeetingRow(meeting).update(db)
      }
      return rows.map(\.id)
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
  /// `summaryText` among them) plus the meeting's tasks, decisions and
  /// speaker name suggestions, replaced. Decision ids derive from the meeting
  /// id so re-runs are stable. Of `speakerNames`, only suggestions that carry
  /// a name and point at one of the meeting's speakers are kept, the
  /// strongest per speaker.
  public func replaceSummary(
    _ meeting: Meeting, tasks: [MeetingTask], decisions: [String],
    speakerNames: [SpeakerNameSuggestion] = []
  ) async throws {
    let meetingID = meeting.id
    try await writer.write { db in
      try Self.writeProcessingResults(of: meeting, db)
      try Self.replaceNameSuggestions(speakerNames, meetingID: meetingID, db)
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
          id: UUID(derivedFrom: meetingID, salt: "decision-\(index)"), meetingID: meetingID,
          text: text)
        try DecisionRow(decision).insert(db)
      }
    }
  }

  static func replaceNameSuggestions(
    _ suggestions: [SpeakerNameSuggestion], meetingID: UUID, _ db: Database
  ) throws {
    let key = meetingID.uuidString
    try SpeakerNameSuggestionRow.filter(SpeakerNameSuggestionRow.Columns.meetingID == key)
      .deleteAll(db)
    let speakerIDs = try Set(
      UUID.fetchAll(
        db, SpeakerRow.filter(SpeakerRow.Columns.meetingID == key).select(SpeakerRow.Columns.id)))
    // Ascending, so a duplicate speaker ends with its strongest suggestion.
    for suggestion in suggestions.sorted(by: { $0.confidence < $1.confidence })
    where speakerIDs.contains(suggestion.speakerID) {
      try SpeakerNameSuggestionRow(suggestion, meetingID: meetingID)?.save(db)
    }
  }

  /// The model's guess who each speaker is, at most one per speaker, in
  /// speaker id order: written with every summary, removed by `confirm` and
  /// with the speaker or the meeting. Never applied automatically; the
  /// review sheet offers it beside the cosine match and the calendar
  /// attendees (#78).
  public func nameSuggestions(meetingID: UUID) async throws -> [SpeakerNameSuggestion] {
    try await writer.read { db in
      try SpeakerNameSuggestionRow
        .filter(SpeakerNameSuggestionRow.Columns.meetingID == meetingID.uuidString)
        .order(SpeakerNameSuggestionRow.Columns.speakerID)
        .fetchAll(db)
        .map(\.suggestion)
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

  // MARK: - Deletion

  /// Removes the meeting and everything that hangs off it: the cascaded rows
  /// (participants, speakers, segments, tasks, decisions, assets,
  /// deliveries), their FTS rows through the triggers, the handover receipt
  /// that admitted it, and its files. Persons stay; they belong to every
  /// meeting. Throws `MeetingStoreError.meetingBusy` while the meeting is
  /// `.recording` or `.processing`, because the capture writer or the
  /// pipeline still holds the files.
  ///
  /// Files: when the asset sits in its own meeting folder
  /// (`RecordingLayout(audioFolder:meetingID:)`, the folder named after the
  /// meeting id) the whole folder goes, sample clips included. Otherwise only
  /// the files the rows point at are removed (master, sidecars, mixdown,
  /// clips), so an asset placed in a shared folder never takes its
  /// neighbours with it. The rows are gone and `MeetingEvent.deleted` is
  /// posted before any file is touched; a file that resists is thrown after
  /// the rest were removed.
  public func delete(meetingID: UUID) async throws {
    let files: [URL] = try await writer.write { db in
      let meeting = try Self.currentMeeting(meetingID, db)
      switch meeting.state {
      case .recording, .processing:
        throw MeetingStoreError.meetingBusy(meetingID, meeting.state.kind)
      case .queued, .ready, .failed:
        break
      }
      let key = meetingID.uuidString
      let assets = try AudioAssetRow.filter(AudioAssetRow.Columns.meetingID == key)
        .order(AudioAssetRow.Columns.id).fetchAll(db).map(\.asset)
      let clips = try SpeakerRow.filter(SpeakerRow.Columns.meetingID == key).fetchAll(db)
        .compactMap(\.sampleClipURL)
      try HandoverReceiptRow.filter(HandoverReceiptRow.Columns.meetingID == key).deleteAll(db)
      try MeetingRow.filter(MeetingRow.Columns.id == key).deleteAll(db)
      return Self.filesToRemove(meetingID: meetingID, assets: assets, clips: clips)
    }
    await events.post(.deleted(meetingID: meetingID))
    var firstError: (any Error)?
    for url in files where FileManager.default.fileExists(atPath: url.path) {
      do {
        try FileManager.default.removeItem(at: url)
      } catch {
        if firstError == nil { firstError = error }
      }
    }
    if let firstError { throw firstError }
  }

  /// The meeting folder when an asset lives in one, else every file the rows
  /// name, each once, in asset then clip order.
  static func filesToRemove(meetingID: UUID, assets: [AudioAsset], clips: [URL]) -> [URL] {
    let folders = assets.map { RecordingLayout(asset: $0).directory }
      .filter { $0.lastPathComponent == meetingID.uuidString }
    if let folder = folders.first { return [folder] }
    var seen: Set<String> = []
    return (assets.flatMap(\.expirableFiles) + clips).filter { seen.insert($0.path).inserted }
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

  /// One row per (meeting, destination); `Delivery.id` derives from the pair.
  public func save(_ delivery: Delivery) async throws {
    try await writer.write { db in try DeliveryRow(delivery).save(db) }
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
}
