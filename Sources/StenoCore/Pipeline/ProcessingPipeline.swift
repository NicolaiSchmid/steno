import Foundation

/// Everything the pipeline needs, and the only injection axis: the app and
/// the CLI pass real implementations, tests pass the fakes in `Testing/`.
/// `events` defaults to `store.events`, so the store's `deleted` and the
/// pipeline's `progress` reach one subscriber.
public struct PipelineDependencies: Sendable {
  public let decoder: any AudioDecoder
  public let speechEngine: any SpeechEngine
  public let diarizer: any Diarizer
  public let speakerMemory: any SpeakerMemory
  public let cleaner: any TranscriptCleaner
  public let summarizer: any MeetingSummarizer
  public let dispatcher: any DeliveryDispatcher
  public let store: MeetingStore
  public let settings: SettingsStore
  public let events: MeetingEventBus
  public let now: @Sendable () -> Date

  public init(
    decoder: any AudioDecoder,
    speechEngine: any SpeechEngine,
    diarizer: any Diarizer,
    speakerMemory: any SpeakerMemory,
    cleaner: any TranscriptCleaner,
    summarizer: any MeetingSummarizer,
    dispatcher: any DeliveryDispatcher,
    store: MeetingStore,
    settings: SettingsStore,
    events: MeetingEventBus? = nil,
    now: @escaping @Sendable () -> Date = Date.init
  ) {
    self.decoder = decoder
    self.speechEngine = speechEngine
    self.diarizer = diarizer
    self.speakerMemory = speakerMemory
    self.cleaner = cleaner
    self.summarizer = summarizer
    self.dispatcher = dispatcher
    self.store = store
    self.settings = settings
    self.events = events ?? store.events
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
  let dependencies: PipelineDependencies
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
    start(assetID: asset.id)
  }

  /// Launch recovery for the queue: every meeting a previous process left
  /// `.queued` or `.processing` is processed again from `decode` (each stage
  /// replaces what an earlier run wrote), oldest first, in the background
  /// like `enqueue`. A meeting whose asset row is missing cannot be processed
  /// and is marked `.failed`. Meetings already in flight here are skipped.
  /// Returns the ids of the meetings whose processing was started. The app
  /// calls this once after `MeetingStore.failInterruptedRecordings(now:)`.
  @discardableResult
  public func resumeUnfinished() async throws -> [UUID] {
    var resumed: [UUID] = []
    for meeting in try await store.meetings(inStates: [.queued, .processing])
    where !inFlight.contains(meeting.id) {
      guard let asset = try await store.asset(meetingID: meeting.id) else {
        try await store.setState(
          .failed(reason: "Processing was interrupted and the recording's asset is missing"),
          meetingID: meeting.id, now: now)
        continue
      }
      guard running[asset.id] == nil else { continue }
      start(assetID: asset.id)
      resumed.append(meeting.id)
    }
    return resumed
  }

  /// Runs `process(assetID:)` in the background and tracks it for
  /// `waitUntilIdle`.
  private func start(assetID: UUID) {
    running[assetID] = Task { [weak self] in
      try? await self?.process(assetID: assetID)
      await self?.finished(assetID)
    }
  }

  /// Waits for every processing task started by `enqueue` or
  /// `resumeUnfinished`; the CLI and the tests call it before reading
  /// results.
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
  /// or summarize failure). Once `persist` has marked the meeting `.ready`
  /// nothing downgrades it: a `retention` error is thrown to the caller and
  /// the meeting stays ready and delivered.
  public func process(assetID: UUID) async throws {
    guard let asset = try await store.asset(id: assetID) else {
      throw PipelineFailure(stage: .decode, reason: "audio asset \(assetID) not found")
    }
    guard let meeting = try await store.meeting(id: asset.meetingID) else {
      throw PipelineFailure(stage: .decode, reason: "meeting \(asset.meetingID) not found")
    }
    try await exclusively(meeting.id, stage: .decode) {
      try await store.setState(.processing, meetingID: meeting.id, now: now)
      let persisted: AudioAsset
      do {
        let settings = try await attributing(.decode) { try await dependencies.settings.load() }
        let transcription = try await decodeAndTranscribe(asset: asset, meetingID: meeting.id)
        var current = meeting
        current.language = transcription.language
        current.state = .processing
        var diarized = try await diarize(asset: asset, meeting: current)
        diarized.speakers = try await matchSpeakers(
          diarized.speakers, meetingID: meeting.id, settings: settings)
        let merged = try await merge(
          meeting: current, lanes: transcription.lanes, diarization: diarized)
        let cleaned = try await cleanup(
          meeting: current, segments: merged.segments, speakers: merged.speakers)
        current.llmUsage = (current.llmUsage ?? .zero) + cleaned.usage
        current = try await summarize(
          meeting: current, segments: cleaned.segments, speakers: merged.speakers)
        persisted = try await persist(meeting: current, asset: asset)
      } catch {
        // Every stage attributes its own errors; `.decode` is the fallback
        // for anything thrown outside one.
        let failure = PipelineFailure.wrapping(error, stage: .decode)
        try? await store.setState(
          .failed(reason: failure.description), meetingID: meeting.id, now: now)
        throw failure
      }
      await deliver(meetingID: meeting.id)
      try await retention(asset: persisted)
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
      let export = try await attributing(.summarize) {
        try await store.export(meetingID: meetingID)
      }
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

  /// Posts `progress` for `stage` starting on `meetingID`.
  func post(_ stage: PipelineStage, meetingID: UUID) async {
    await dependencies.events.post(.progress(meetingID: meetingID, stage: stage))
  }

  /// Turns any error thrown by `body` into a `PipelineFailure` carrying
  /// `stage`, without posting progress (the second lane of a per-lane stage,
  /// work before a stage starts).
  func attributing<T: Sendable>(_ stage: PipelineStage, _ body: () async throws -> T)
    async throws -> T
  {
    do {
      return try await body()
    } catch {
      throw PipelineFailure.wrapping(error, stage: stage)
    }
  }

  /// Posts `progress` for `stage`, then runs `body` attributing its errors
  /// to the stage.
  func run<T: Sendable>(_ stage: PipelineStage, meetingID: UUID, _ body: () async throws -> T)
    async throws -> T
  {
    await post(stage, meetingID: meetingID)
    return try await attributing(stage, body)
  }
}
