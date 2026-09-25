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
public struct RecordingIntake: HandoverIntake, Sendable {
  public typealias Enqueue = @Sendable (Meeting, AudioAsset) async throws -> Void

  public let store: MeetingStore
  public let settings: SettingsStore
  public let enqueue: Enqueue
  public let now: @Sendable () -> Date

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

  /// The production wiring: `enqueue` is `ProcessingPipeline.enqueue`.
  public init(
    store: MeetingStore,
    settings: SettingsStore,
    pipeline: ProcessingPipeline,
    now: @escaping @Sendable () -> Date = Date.init
  ) {
    self.init(
      store: store, settings: settings,
      enqueue: { meeting, asset in try await pipeline.enqueue(meeting, asset: asset) },
      now: now)
  }

  public func admit(file: URL, metadata: RecordingMetadata, device: PairedDevice) async throws
    -> UUID
  {
    let existing = try await store.receipt(metadata.recordingID)
    if let meetingID = existing?.state.meetingID, try await store.meeting(id: meetingID) != nil {
      return meetingID
    }

    let settings = try await settings.load()
    let meetingID = UUID()
    let timestamp = now()
    let layout = RecordingLayout(audioFolder: settings.audioFolder, meetingID: meetingID)
    try layout.createDirectories()
    let destination = layout.master(metadata.format)
    if FileManager.default.fileExists(atPath: destination.path) {
      try FileManager.default.removeItem(at: destination)
    }
    try FileManager.default.copyItem(at: file, to: destination)

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
      try await store.save(receipt)
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

  /// "Phone recording 2026-09-24 11:00" in the Mac's time zone.
  static func title(for startedAt: Date, timeZone: TimeZone = .current) -> String {
    let formatter = DateFormatter()
    formatter.locale = Locale(identifier: "en_US_POSIX")
    formatter.timeZone = timeZone
    formatter.dateFormat = "yyyy-MM-dd HH:mm"
    return "Phone recording " + formatter.string(from: startedAt)
  }
}
