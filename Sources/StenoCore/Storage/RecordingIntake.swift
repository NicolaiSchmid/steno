import Foundation

/// Admits a fully received phone recording: copies the file into
/// `audioFolder/<meetingID>/` (`RecordingLayout`), commits the
/// `HandoverReceipt` as `.complete(meetingID)` together with a `.phone`
/// meeting `.queued` with a `.mixed` `AudioAsset` under the default
/// retention, deletes the upload, and only then hands the meeting to the
/// pipeline. Idempotent on `recordingID`: a recording whose receipt is
/// `.complete` and whose meeting still exists returns the same meeting id
/// and does nothing else.
///
/// The phone deletes its copy once `complete` answers 200, so everything
/// the admission wrote is on the disk first. The copy, its meeting folder
/// and every folder `admit` created are synced (`F_FULLFSYNC`, `fsync`
/// where that fails), and the receipt, the meeting and its asset commit in
/// one durable transaction (`MeetingStore.saveDurably(_:meeting:asset:)`),
/// so no crash, full disk or busy store leaves a `.complete` receipt
/// without its meeting. When that commit fails, the copy is removed and the
/// receipt becomes `.failed(reason)` as usual, so the handover service's
/// retry with the same path admits again: the phone keeps its copy then.
/// `enqueue` after the commit is `ProcessingPipeline.enqueueSaved` in the
/// production wiring (`init(currentPipeline:)`); its failure does not undo
/// the admission, the meeting waits `.queued` for the next launch's resume.
/// Rust: `RecordingIntake` in `crates/steno-pipeline/src/intake.rs`.
public struct RecordingIntake: HandoverIntake, Sendable {
  public typealias Enqueue = @Sendable (Meeting, AudioAsset) async throws -> Void

  public let store: MeetingStore
  public let settings: SettingsStore
  public let enqueue: Enqueue
  public let now: @Sendable () -> Date
  /// The syncs `admit` makes; the disk in the product, a recorder in the
  /// tests.
  var syncs = Syncs.disk

  /// `enqueue` hands the meeting `admit` saved to the pipeline
  /// (`ProcessingPipeline.enqueueSaved` in the production wiring); tests
  /// pass a counting closure.
  public init(
    store: MeetingStore,
    settings: SettingsStore,
    enqueue: @escaping Enqueue,
    now: @escaping @Sendable () -> Date = Date.init
  ) {
    self.store = store
    self.settings = settings
    self.enqueue = enqueue
    self.now = now
  }

  /// The production wiring: `enqueue` is `ProcessingPipeline.enqueueSaved`
  /// on the pipeline `currentPipeline` returns when a recording is
  /// admitted, so a pipeline reload never strands the intake
  /// (`AppEnvironment.makeIntake`).
  public init(
    store: MeetingStore,
    settings: SettingsStore,
    currentPipeline: @escaping @Sendable () async throws -> ProcessingPipeline,
    now: @escaping @Sendable () -> Date = Date.init
  ) {
    self.init(
      store: store, settings: settings,
      enqueue: { meeting, asset in
        try await currentPipeline().enqueueSaved(meeting, asset: asset)
      },
      now: now)
  }

  /// `init(currentPipeline:)` over one pipeline that never changes.
  public init(
    store: MeetingStore,
    settings: SettingsStore,
    pipeline: ProcessingPipeline,
    now: @escaping @Sendable () -> Date = Date.init
  ) {
    self.init(store: store, settings: settings, currentPipeline: { pipeline }, now: now)
  }

  public func admit(file: URL, metadata: RecordingMetadata, device: PairedDevice) async throws
    -> UUID
  {
    let existing = try await store.handoverReceipt(recordingID: metadata.recordingID)
    // Another phone's receipt under this id (this one was revoked, and that
    // one announced the id) is never completed or answered from: that phone
    // would take this meeting for its own and delete its copy.
    if let existing, existing.deviceID != device.id {
      throw MeetingStoreError.receiptOfAnotherDevice(metadata.recordingID)
    }
    if let meetingID = existing?.state.meetingID, try await store.meeting(id: meetingID) != nil {
      return meetingID
    }

    let settings = try await settings.load()
    let meetingID = UUID()
    let timestamp = now()
    let layout = RecordingLayout(audioFolder: settings.audioFolder, meetingID: meetingID)
    // The copy and its folders are on the disk before the receipt says
    // complete. `copyItem` syncs nothing (on APFS it clones the file's
    // metadata only), so the parent of every folder created here, the copy
    // and its meeting folder are synced, as Rust's `create_dir_all_durably`
    // and `copy_durably` do.
    let created = Self.missingFolders(layout.directory)
    try layout.createDirectories()
    for folder in created.reversed() {
      syncs.directory(folder.deletingLastPathComponent())
    }
    let destination = layout.master(metadata.format)
    if FileManager.default.fileExists(atPath: destination.path) {
      try FileManager.default.removeItem(at: destination)
    }
    try FileManager.default.copyItem(at: file, to: destination)
    do {
      try syncs.file(destination)
    } catch {
      try? FileManager.default.removeItem(at: destination)
      throw error
    }
    syncs.directory(layout.directory)

    let meeting = Meeting(
      id: meetingID,
      title: Self.title(for: metadata.startedAt),
      startedAt: metadata.startedAt,
      duration: metadata.durationSeconds,
      source: .phone,
      state: .queued,
      templateID: settings.defaultTemplateID,
      createdAt: timestamp,
      updatedAt: timestamp
    )
    let asset = AudioAsset(
      id: UUID(),
      meetingID: meetingID,
      url: destination,
      format: metadata.format,
      lanes: [.mixed],
      retention: settings.defaultRetention
    )

    var receipt =
      existing
      ?? HandoverReceipt(
        recordingID: metadata.recordingID,
        deviceID: device.id,
        state: .receiving,
        byteCount: metadata.byteCount,
        sha256: metadata.sha256,
        chunkSize: metadata.chunkSize,
        createdAt: timestamp,
        updatedAt: timestamp
      )
    receipt.state = .complete(meetingID: meetingID)
    receipt.updatedAt = timestamp

    do {
      try await store.saveDurably(receipt, meeting: meeting, asset: asset)
    } catch {
      try? FileManager.default.removeItem(at: destination)
      // Another phone's receipt is left as it is.
      if error as? MeetingStoreError != .receiptOfAnotherDevice(metadata.recordingID) {
        receipt.state = .failed("admit: \(error)")
        try? await store.save(receipt)
      }
      throw error
    }
    try? FileManager.default.removeItem(at: file)
    // Admitted: a pipeline that cannot take the meeting now leaves it
    // `.queued`, and the next launch resumes it.
    try? await enqueue(meeting, asset)
    return meetingID
  }

  /// `folder` and the folders above it that do not exist yet, deepest
  /// first.
  static func missingFolders(_ folder: URL) -> [URL] {
    var missing: [URL] = []
    var current = folder.standardizedFileURL
    while !FileManager.default.fileExists(atPath: current.path) {
      missing.append(current)
      let parent = current.deletingLastPathComponent()
      if parent.path == current.path { break }
      current = parent
    }
    return missing
  }

  /// "Phone recording 2026-09-24 11:00" in the Mac's time zone.
  static func title(for startedAt: Date, timeZone: TimeZone = .current) -> String {
    let formatter = DateFormatter()
    formatter.locale = Locale(identifier: "en_US_POSIX")
    formatter.timeZone = timeZone
    formatter.dateFormat = "yyyy-MM-dd HH:mm"
    return "Phone recording " + formatter.string(from: startedAt)
  }
}

extension RecordingIntake {
  /// The syncs a durable admission makes. A file's sync throws; a folder's
  /// is best effort, as in Rust's `Syncs` (`crates/steno-pipeline/src/files.rs`).
  struct Syncs: Sendable {
    /// Flushes the file at the URL to the disk.
    var file: @Sendable (URL) throws -> Void
    /// Makes the entries of the folder at the URL (a new file, a new
    /// folder) durable.
    var directory: @Sendable (URL) -> Void

    /// The product's syncs: `F_FULLFSYNC`, which also flushes the drive's
    /// cache, and `fsync` where that fails or the platform lacks it.
    static let disk = Syncs(
      file: { url in try fullSync(url) },
      directory: { url in try? fullSync(url) })

    static func fullSync(_ url: URL) throws {
      let descriptor = open(url.path, O_RDONLY)
      guard descriptor >= 0 else { throw posixError() }
      defer { close(descriptor) }
      #if canImport(Darwin)
        if fcntl(descriptor, F_FULLFSYNC) == 0 { return }
      #endif
      guard fsync(descriptor) == 0 else { throw posixError() }
    }

    private static func posixError() -> POSIXError {
      POSIXError(POSIXErrorCode(rawValue: errno) ?? .EIO)
    }
  }
}
