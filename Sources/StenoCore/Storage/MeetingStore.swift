import Foundation
import GRDB

/// Errors a store call can raise beyond GRDB's own.
public enum MeetingStoreError: Error, Sendable, Equatable, CustomStringConvertible {
  case meetingNotFound(UUID)
  case speakerNotFound(UUID)
  case personNotFound(UUID)
  case speakersInDifferentMeetings(UUID, UUID)
  /// `setDismissed(speakerID:_:)` on a meeting's "me" speaker: the mic lane
  /// is the user's own voice. Rust: `StoreError::MeSpeakerNotDismissable`.
  case meSpeakerNotDismissable(UUID)
  /// `rejectSuggestion` for the person the speaker is confirmed as; that is
  /// `clear(speakerID:)`. Rust: `StoreError::RejectedConfirmedPerson`.
  case rejectedConfirmedPerson(UUID, UUID)
  /// `delete(meetingID:)` while the capture writer or the pipeline still
  /// holds the meeting's files.
  case meetingBusy(UUID, MeetingState.Kind)
  /// `resolvePerson(named:)` with nothing but whitespace.
  case blankPersonName
  /// The phone intake's admission of a recording whose receipt is another
  /// upload's, thrown at `RecordingIntake.admit`'s own read and by
  /// `saveDurably(_:meeting:asset:)`: it belongs to another device (the
  /// admitting phone was revoked and another one announced the same
  /// recording id, or took the receipt over), or it holds other bytes (the
  /// phone announced another file under the id). Rust:
  /// `StoreError::ReceiptOfAnotherUpload`.
  case receiptOfAnotherUpload(UUID)

  public var description: String {
    switch self {
    case .meetingNotFound(let id): "meeting \(id) not found"
    case .meetingBusy(let id, let state): "meeting \(id) is \(state.rawValue) and cannot be deleted"
    case .speakerNotFound(let id): "speaker \(id) not found"
    case .personNotFound(let id): "person \(id) not found"
    case .speakersInDifferentMeetings(let a, let b):
      "speakers \(a) and \(b) belong to different meetings"
    case .meSpeakerNotDismissable(let id):
      "speaker \(id) is the user's own voice and cannot be dismissed"
    case .rejectedConfirmedPerson(let speaker, let person):
      "speaker \(speaker) is confirmed as person \(person), which a No cannot answer"
    case .blankPersonName: "a person needs a name"
    case .receiptOfAnotherUpload(let id): "the receipt of recording \(id) belongs to another upload"
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

  /// Runs the migrator on `writer`, then backfills the admission ledger
  /// (`backfillAdmissions`). GRDB's migrator ignores an applied migration it
  /// does not know (a newer build's), and this store does not ask
  /// `hasBeenSuperseded`: every migration only adds tables and columns, so
  /// this build keeps reading and writing what it knows, as the Rust store
  /// does.
  public init(writer: any DatabaseWriter, events: MeetingEventBus = MeetingEventBus()) throws {
    self.writer = writer
    self.events = events
    try Migrations.migrator().migrate(writer)
    try writer.write { db in try db.execute(sql: Self.backfillAdmissions) }
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

  /// One write transaction whose commit is on the disk when this returns,
  /// for the commits an answer to another device depends on: the phone
  /// intake's admission, before `complete` tells the phone to delete its
  /// copy; the `failed` receipt after a failed admission commit, which
  /// writes over that commit's frames before the intake removes its copy;
  /// a pairing, whose token the phone keeps; and a revoke. The pool's
  /// writer commits under `synchronous = NORMAL`, which syncs the WAL only
  /// at a checkpoint, so a power loss can roll a commit back.
  /// This transaction runs under `synchronous = FULL` with `fullfsync` on,
  /// so its commit syncs the WAL with `F_FULLFSYNC`, which also flushes the
  /// drive's cache; the sync covers every earlier commit in the WAL too.
  /// That costs one WAL fsync per commit, plus a flush of the drive's cache,
  /// all while the writer is held, so other writes wait for it.
  /// SQLite refuses to change `synchronous` inside a transaction, so the
  /// levels are set before `BEGIN` and set back once the transaction has
  /// committed or rolled back, in the same writer access: no other write
  /// runs under them. Rust: `Store::write_durably`.
  public func writeDurably<T: Sendable>(_ body: @escaping @Sendable (Database) throws -> T)
    async throws -> T
  {
    try await writer.writeWithoutTransaction { db in
      try Self.underFullSync(db) {
        var value: T?
        try db.inTransaction(.immediate) {
          value = try body(db)
          return .commit
        }
        return value!
      }
    }
  }

  /// Copies every commit in the WAL into the database file, syncs it, and
  /// starts the WAL over, so a power loss afterwards brings back what the store
  /// reads now. `HandoverService.checkpointStore(_:)` runs it at launch, before
  /// the handover listener starts, and says why. `checkpoint(.truncate)` under
  /// `synchronous = FULL` with `fullfsync` on copies the frames, syncs the
  /// database file, waits until no reader is left in the WAL and truncates the
  /// WAL file, keeping a new salt for its next header. The truncation is not
  /// synced: after a power loss the file can come back with its old header and
  /// frames, and recovery replays every frame under the header's salt, which
  /// would put older pages back over the checkpointed ones. So one durable
  /// write that changes a page follows, in the same writer access: a private
  /// table created and dropped, which leaves the schema and the applied
  /// migrations as they were. Its commit writes the WAL header with the new
  /// salt and syncs it with its frames, so recovery replays only those frames:
  /// every older one carries the old salt. A failed sync throws, and so does a
  /// checkpoint that another connection (a writer, or a reader still in the
  /// WAL) blocks when the busy timeout runs out (`SQLITE_BUSY`), since the
  /// commits it could not copy are not known to be on the disk. An in-memory
  /// store has no WAL and returns at once. Rust: `Store::checkpoint_durably`.
  public func checkpointDurably() async throws {
    try await writer.writeWithoutTransaction { db in
      try Self.underFullSync(db) {
        let (walFrameCount, _) = try db.checkpoint(.truncate)
        guard walFrameCount >= 0 else { return }
        // A commit that changes no page writes no frame, and so no WAL
        // header; creating a table always changes one.
        try db.inTransaction(.immediate) {
          try db.execute(sql: "CREATE TABLE stenoWalRestart(x); DROP TABLE stenoWalRestart")
          return .commit
        }
      }
    }
  }

  /// `body` with the connection at `synchronous = FULL` and `fullfsync` on,
  /// and the levels it found set back afterwards. Outside a transaction
  /// only: SQLite refuses to change `synchronous` inside one.
  private static func underFullSync<T>(_ db: Database, _ body: () throws -> T) throws -> T {
    let synchronous = try Int.fetchOne(db, sql: "PRAGMA synchronous") ?? 1
    let fullfsync = try Bool.fetchOne(db, sql: "PRAGMA fullfsync") ?? false
    // Neither pragma fails outside a transaction, and the transaction has
    // ended by now.
    defer {
      try? db.execute(sql: "PRAGMA synchronous = \(synchronous)")
      try? db.execute(sql: "PRAGMA fullfsync = \(fullfsync ? 1 : 0)")
    }
    try db.execute(sql: "PRAGMA synchronous = FULL")
    try db.execute(sql: "PRAGMA fullfsync = ON")
    return try body()
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

  /// The user's rename: the trimmed `title` with `titleOrigin = .user`, so
  /// no later default or summary title is mistaken for it. A blank title
  /// leaves the row alone.
  public func rename(meetingID: UUID, title: String, now: Date) async throws {
    let trimmed = title.trimmingCharacters(in: .whitespacesAndNewlines)
    guard !trimmed.isEmpty else { return }
    try await update(meetingID: meetingID, now: now) {
      $0.title = trimmed
      $0.titleOrigin = .user
    }
  }

  /// One transaction: the meeting's processing columns (see
  /// `Meeting.applyProcessingResults`) plus every speaker and segment of the
  /// meeting, replaced. The pipeline's merge stage calls this.
  ///
  /// The user's answers about the stored voices come back on the new rows
  /// that are the same voice (`sameVoices`), never by id alone: ids derive
  /// from "Speaker n", which counts by first speech, so another run can
  /// give a label to another voice. A kept confirmation keeps its person
  /// and, when the new row has no clip and no new row took its file, the
  /// stored clip; a dismissed voice stays dismissed; a No to a person
  /// stays, so a new suggestion of that person for that voice is dropped.
  /// An answer whose voice is not found again is dropped, and the voice
  /// shows for review. A name suggestion stays with its speaker id. Every
  /// person with a confirmed speaker before has their voice recomputed.
  /// Rust: `Store::replace_transcript`.
  public func replaceTranscript(
    _ meeting: Meeting, segments: [TranscriptSegment], speakers: [Speaker]
  ) async throws {
    let meetingID = meeting.id
    try await writer.write { db in
      try Self.writeProcessingResults(of: meeting, db)
      let meetingSpeakers = SpeakerRow.filter(SpeakerRow.Columns.meetingID == meetingID.uuidString)
      let replaced = try meetingSpeakers.fetchAll(db).map(\.speaker)
      // Deleting the speakers cascades to their suggestions and the user's
      // Noes.
      let suggestions =
        try SpeakerNameSuggestionRow
        .filter(SpeakerNameSuggestionRow.Columns.meetingID == meetingID.uuidString)
        .fetchAll(db).map(\.suggestion)
      let rejections = try Self.rejectionRows(meetingID: meetingID, db)
      let answered = replaced.filter { speaker in
        speaker.assignment.isConfirmed || speaker.dismissed
          || rejections.contains { $0.speakerID == speaker.id }
      }
      // New id -> the stored row whose answers it takes.
      var earlier: [UUID: Speaker] = [:]
      for (old, new) in Self.sameVoices(meetingID: meetingID, earlier: answered, speakers: speakers)
      {
        earlier[new] = old
      }
      // Kept or dropped, each stored confirmation's person is recomputed.
      let voices = Set(replaced.filter(\.assignment.isConfirmed).compactMap(\.personID))
      try TranscriptSegmentRow
        .filter(TranscriptSegmentRow.Columns.meetingID == meetingID.uuidString)
        .deleteAll(db)
      try meetingSpeakers.deleteAll(db)
      var keptRejections: [SpeakerSuggestionRejectionRow] = []
      for speaker in speakers {
        var speaker = speaker
        speaker.meetingID = meetingID
        if let kept = earlier[speaker.id] {
          let own = rejections.filter { $0.speakerID == kept.id }
          for var rejection in own {
            rejection.speakerID = speaker.id
            keptRejections.append(rejection)
          }
          if kept.assignment.isConfirmed {
            speaker.assignment = kept.assignment
            if speaker.sampleClipURL == nil, let clip = kept.sampleClipURL,
              !speakers.contains(where: { $0.sampleClipURL == clip })
            {
              speaker.sampleClipURL = clip
              speaker.sampleClipRange = kept.sampleClipRange
            }
          } else if case .suggested(let personID, _) = speaker.assignment,
            own.contains(where: { $0.personID == personID })
          {
            speaker.assignment = .unknown
          }
          if kept.dismissed {
            speaker.dismissed = true
            speaker.assignment = .unknown
          }
        }
        try SpeakerRow(speaker).insert(db)
      }
      try Self.replaceNameSuggestions(suggestions, meetingID: meetingID, db)
      for rejection in keptRejections { try Self.insertRejection(rejection, db) }
      for segment in segments {
        var segment = segment
        segment.meetingID = meetingID
        try TranscriptSegmentRow(segment).insert(db)
      }
      for personID in voices { try Self.refreshVoice(personID: personID, db) }
    }
  }

  /// Pairs each of `earlier` (stored rows the user answered) with the new
  /// row that is the same voice, one to one, best match first: the cosine of
  /// their embeddings, from `VoiceStrength.floor` up; the same id wins a
  /// tie. The "me" speaker and the Rust app's room row (a run without
  /// diarization) are lanes, not clusters, so each pairs only with itself,
  /// whatever its embedding. A row without an embedding pairs with nothing
  /// else. Rust: `same_voices`.
  static func sameVoices(meetingID: UUID, earlier: [Speaker], speakers: [Speaker])
    -> [(old: Speaker, new: UUID)]
  {
    let lanes: Set<UUID> = [
      LaneMerger.meSpeakerID(meetingID: meetingID),
      UUID(derivedFrom: meetingID, salt: "speaker-room"),
    ]
    var pairs: [(score: Float, old: Speaker, new: UUID)] = []
    for old in earlier {
      for new in speakers {
        let score: Float
        if lanes.contains(old.id) || lanes.contains(new.id) {
          guard old.id == new.id else { continue }
          score = .infinity
        } else if let lhs = old.embedding, let rhs = new.embedding {
          score = lhs.cosineSimilarity(to: rhs)
          guard score >= VoiceStrength.floor else { continue }
        } else {
          continue
        }
        pairs.append((score, old, new.id))
      }
    }
    pairs.sort { lhs, rhs in
      if lhs.score != rhs.score { return lhs.score > rhs.score }
      let lhsSame = lhs.old.id == lhs.new
      let rhsSame = rhs.old.id == rhs.new
      if lhsSame != rhsSame { return lhsSame }
      if lhs.old.id != rhs.old.id { return lhs.old.id.uuidString < rhs.old.id.uuidString }
      return lhs.new.uuidString < rhs.new.uuidString
    }
    var takenOld: Set<UUID> = []
    var takenNew: Set<UUID> = []
    var matched: [(old: Speaker, new: UUID)] = []
    for pair in pairs where !takenOld.contains(pair.old.id) && !takenNew.contains(pair.new) {
      takenOld.insert(pair.old.id)
      takenNew.insert(pair.new)
      matched.append((pair.old, pair.new))
    }
    return matched
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
  /// speaker picker offers it beside the cosine match and the calendar
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

  /// Removes one participant; a missing id is not an error.
  public func deleteParticipant(id: UUID) async throws {
    _ = try await writer.write { db in
      try ParticipantRow.filter(ParticipantRow.Columns.id == id.uuidString).deleteAll(db)
    }
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

  /// Every asset with its URLs, in id order. `RetentionSweep.keepAll()`
  /// pairs them with the files on disk.
  public func assets() async throws -> [AudioAsset] {
    try await writer.read { db in
      try AudioAssetRow.order(AudioAssetRow.Columns.id).fetchAll(db).map(\.asset)
    }
  }

  /// One write: `retention = .keepForever` and `expiresAt = nil` on every
  /// listed asset. Both together, because an asset left at `.keepDays` with
  /// no stamp is the deferred case that the next Re-export would stamp
  /// again. Unknown ids are ignored.
  public func keepForever(assetIDs: [UUID]) async throws {
    guard !assetIDs.isEmpty else { return }
    let keys = assetIDs.map(\.uuidString)
    try await writer.write { db in
      try AudioAssetRow.filter(keys.contains(AudioAssetRow.Columns.id))
        .updateAll(
          db,
          AudioAssetRow.Columns.retention.set(to: AudioRetention.Kind.keepForever.rawValue),
          AudioAssetRow.Columns.retentionDays.set(to: nil),
          AudioAssetRow.Columns.expiresAt.set(to: nil))
    }
  }

  /// Assets whose `expiresAt` is at or before `now` and whose meeting is
  /// `.ready` or `.failed`; the retention sweep's one query. A meeting that
  /// is still recording, queued or processing keeps its files whatever the
  /// stamp says.
  public func expiredAssets(now: Date) async throws -> [AudioAsset] {
    let settled = [MeetingState.Kind.ready, .failed].map(\.rawValue)
    return try await writer.read { db in
      try AudioAssetRow
        .filter(AudioAssetRow.Columns.expiresAt != nil && AudioAssetRow.Columns.expiresAt <= now)
        .filter(
          sql: "meetingID IN (SELECT id FROM meeting WHERE state IN (?, ?))",
          arguments: StatementArguments(settled)
        )
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

  /// Drops the listed rows; `DeliveryCoordinator` uses it for destinations
  /// that are no longer configured. Unknown ids are ignored.
  public func deleteDeliveries(ids: [UUID]) async throws {
    guard !ids.isEmpty else { return }
    let keys = ids.map(\.uuidString)
    try await writer.write { db in
      _ = try DeliveryRow.filter(keys.contains(DeliveryRow.Columns.id)).deleteAll(db)
    }
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
