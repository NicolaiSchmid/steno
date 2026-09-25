import Foundation

/// Everything the pipeline needs, and the only injection axis: the app and
/// the CLI pass real implementations, tests pass the fakes in `Testing/`.
public struct PipelineDependencies: Sendable {
  public var decoder: any AudioDecoder
  public var speechEngine: any SpeechEngine
  public var diarizer: any Diarizer
  public var speakerMemory: any SpeakerMemory
  public var cleaner: any TranscriptCleaner
  public var summarizer: any MeetingSummarizer
  public var delivery: any DeliveryDispatcher
  public var store: MeetingStore
  public var settings: SettingsStore
  public var events: MeetingEventBus
  public var now: @Sendable () -> Date

  public init(
    decoder: any AudioDecoder,
    speechEngine: any SpeechEngine,
    diarizer: any Diarizer,
    speakerMemory: any SpeakerMemory,
    cleaner: any TranscriptCleaner,
    summarizer: any MeetingSummarizer,
    delivery: any DeliveryDispatcher,
    store: MeetingStore,
    settings: SettingsStore,
    events: MeetingEventBus,
    now: @escaping @Sendable () -> Date = Date.init
  ) {
    self.decoder = decoder
    self.speechEngine = speechEngine
    self.diarizer = diarizer
    self.speakerMemory = speakerMemory
    self.cleaner = cleaner
    self.summarizer = summarizer
    self.delivery = delivery
    self.store = store
    self.settings = settings
    self.events = events
    self.now = now
  }
}

/// The post-meeting pipeline: one actor, one typed function per
/// `PipelineStage` (in `Stages/`), `progress` posted as each stage starts,
/// and one place that turns any error into `.failed(reason)`. Lanes are
/// decoded one at a time inside the stage that needs them, so at most one
/// `AudioBuffer16k` is alive. One operation runs per meeting at a time:
/// a second `process`, `rerunSummary` or `redeliver` on a meeting that is
/// in flight throws instead of interleaving writes with the first.
public actor ProcessingPipeline {
  public let dependencies: PipelineDependencies
  private var running: [UUID: Task<Void, Never>] = [:]
  private var inFlight: Set<UUID> = []

  public init(dependencies: PipelineDependencies) {
    self.dependencies = dependencies
  }

  var store: MeetingStore { dependencies.store }
  var now: Date { dependencies.now() }

  /// Writes `Meeting(.queued)` plus the asset in one transaction and starts
  /// `process` in the background. The app (Mac recordings) and
  /// `RecordingIntake` (phone) both call this. Throws when the asset or the
  /// meeting is already in flight.
  public func enqueue(_ meeting: Meeting, asset: AudioAsset) async throws {
    guard running[asset.id] == nil, !inFlight.contains(meeting.id) else {
      throw PipelineFailure(
        stage: .decode, reason: "meeting \(meeting.id) is already being processed")
    }
    var queued = meeting
    queued.state = .queued
    queued.updatedAt = now
    var asset = asset
    asset.meetingID = meeting.id
    try await store.save(queued, asset: asset)
    let assetID = asset.id
    running[assetID] = Task { [weak self] in
      try? await self?.process(assetID: assetID)
      await self?.finished(assetID)
    }
  }

  /// Waits for every processing task started by `enqueue`; the CLI and the
  /// tests call it before reading results.
  public func waitUntilIdle() async {
    while let task = running.values.first {
      await task.value
    }
  }

  private func finished(_ assetID: UUID) {
    running[assetID] = nil
  }

  /// Runs every stage: `queued → processing → ready`, or `failed(reason)`
  /// with whatever was persisted so far (the transcript survives a cleanup
  /// or summarize failure).
  public func process(assetID: UUID) async throws {
    guard let asset = try await store.asset(id: assetID) else {
      throw PipelineFailure(stage: .decode, reason: "audio asset \(assetID) not found")
    }
    guard let meeting = try await store.meeting(id: asset.meetingID) else {
      throw PipelineFailure(stage: .decode, reason: "meeting \(asset.meetingID) not found")
    }
    try await exclusively(meeting.id, stage: .decode) {
      try await store.setState(.processing, meetingID: meeting.id, now: now)
      do {
        let settings = try await dependencies.settings.load()
        let transcription = try await decodeAndTranscribe(asset: asset, meetingID: meeting.id)
        var current = meeting
        current.language = transcription.language
        current.state = .processing
        let diarized = try await diarize(asset: asset, meeting: current)
        let matched = try await matchSpeakers(
          diarized.speakers, meetingID: meeting.id, settings: settings)
        let merged = try await merge(
          meeting: current, lanes: transcription.lanes, clusters: diarized.clusters,
          speakers: matched)
        let cleaned = try await cleanup(
          meeting: current, segments: merged.segments, speakers: merged.speakers)
        current.llmUsage = (current.llmUsage ?? .zero) + cleaned.usage
        current = try await summarize(
          meeting: current, segments: cleaned.segments, speakers: merged.speakers)
        let persisted = try await persist(meeting: current, asset: asset)
        await deliver(meetingID: meeting.id)
        try await retention(asset: persisted)
      } catch {
        let failure = PipelineFailure(stage: .decode, error: error)
        try? await store.setState(
          .failed(reason: failure.description), meetingID: meeting.id, now: now)
        throw failure
      }
    }
  }

  /// Summarize again with another template, then deliver. A failure is
  /// thrown to the caller and leaves the meeting's state, summary and
  /// deliveries as they were; only `process` marks `.failed`.
  public func rerunSummary(meetingID: UUID, templateID: String) async throws {
    guard let meeting = try await store.meeting(id: meetingID) else {
      throw PipelineFailure(stage: .summarize, reason: "meeting \(meetingID) not found")
    }
    try await exclusively(meetingID, stage: .summarize) {
      let export = try await store.export(meetingID: meetingID)
      var current = meeting
      current.templateID = templateID
      current.state = .ready
      _ = try await summarize(
        meeting: current, segments: export.segments, speakers: export.speakers)
      await deliver(meetingID: meetingID)
    }
  }

  /// Deliver only: the one re-export entry point.
  public func redeliver(meetingID: UUID) async throws {
    guard try await store.meeting(id: meetingID) != nil else {
      throw PipelineFailure(stage: .deliver, reason: "meeting \(meetingID) not found")
    }
    try await exclusively(meetingID, stage: .deliver) {
      await deliver(meetingID: meetingID)
    }
  }

  // MARK: - Stage plumbing

  /// Marks `meetingID` in flight for the duration of `body`; a second
  /// operation on the same meeting throws a `PipelineFailure` for `stage`.
  private func exclusively<T: Sendable>(
    _ meetingID: UUID, stage: PipelineStage, _ body: () async throws -> T
  ) async throws -> T {
    guard inFlight.insert(meetingID).inserted else {
      throw PipelineFailure(stage: stage, reason: "meeting \(meetingID) is already being processed")
    }
    defer { inFlight.remove(meetingID) }
    return try await body()
  }

  /// Posts `progress` for `stage` (unless `post` is false, for the second
  /// lane of a per-lane stage) and turns any error thrown by `body` into a
  /// `PipelineFailure` carrying that stage.
  func run<T: Sendable>(
    _ stage: PipelineStage, meetingID: UUID, post: Bool = true,
    _ body: () async throws -> T
  ) async throws -> T {
    if post {
      let index = PipelineStage.allCases.firstIndex(of: stage) ?? 0
      let fraction = Double(index) / Double(PipelineStage.allCases.count)
      await dependencies.events.post(
        .progress(meetingID: meetingID, stage: stage, fraction: fraction))
    }
    do {
      return try await body()
    } catch {
      throw PipelineFailure(stage: stage, error: error)
    }
  }
}
