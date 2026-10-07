import Foundation

/// Admits a fully received phone recording: copies the file into
/// `audioFolder/<meetingID>/` (`RecordingLayout`), writes the
/// `HandoverReceipt` as `.complete(meetingID)`, enqueues a `.phone` meeting
/// with a `.mixed` `AudioAsset` under the default retention, and only then
/// deletes the upload. Idempotent on `recordingID`: a recording whose
/// receipt is `.complete` and whose meeting still exists returns the same
/// meeting id and does nothing else.
///
/// Order of writes, so that a failure at any point leaves a retryable state:
/// copy (the source stays), receipt `.complete`, enqueue (meeting and asset
/// in one transaction, processing starts). If enqueue throws, the copy is
/// removed and the receipt becomes `.failed(reason)`, so the handover
/// service's retry with the same path admits again instead of finding the
/// file gone or a second meeting created.
///
/// The phone deletes its copy once `complete` answers 200, so everything
/// the admission wrote is on the disk before the receipt says complete.
/// The copy, its meeting folder and every folder `admit` created are
/// synced (`F_FULLFSYNC`, `fsync` where that fails), and the `.complete`
/// receipt and the meeting commit durably (`MeetingStore.writeDurably`):
/// the receipt here, the meeting in `enqueue`, which is
/// `ProcessingPipeline.enqueueDurably` in the production wiring
/// (`init(currentPipeline:)`) and must be in any other production
/// `enqueue`. The `.failed` receipt of a refused admission commits as
/// usual: the phone keeps its copy then.
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

  /// `enqueue` is `ProcessingPipeline.enqueue(_:asset:)` in the app and the
  /// CLI; tests pass a counting closure.
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

  /// The production wiring: `enqueue` is `ProcessingPipeline.enqueueDurably`
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
        try await currentPipeline().enqueueDurably(meeting, asset: asset)
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
      try await store.saveDurably(receipt)
      try await enqueue(meeting, asset)
    } catch {
      try? FileManager.default.removeItem(at: destination)
      receipt.state = .failed("admit: \(error)")
      try? await store.save(receipt)
      throw error
    }
    try? FileManager.default.removeItem(at: file)
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
