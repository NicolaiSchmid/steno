import Foundation

/// Everything the pipeline needs, and the only injection axis: the app and
/// the CLI pass real implementations, tests pass the fakes in `Testing/`.
/// `events` defaults to `store.events`, so the store's `deleted` and the
/// pipeline's `progress` reach one subscriber. `now` stamps rows; `clock`
/// measures stage durations for the learned rates, and tests pass a
/// `ManualClock`.
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
  public let clock: any Clock<Duration>

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
    now: @escaping @Sendable () -> Date = Date.init,
    clock: any Clock<Duration> = ContinuousClock()
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
    self.clock = clock
  }
}

/// The post-meeting pipeline: one actor, one typed function per
/// `PipelineStage` (in `Stages/`), `progress` posted as each stage starts
/// with a fraction and an estimate from the run's `ProcessingRun`, and one
/// place that turns any error into `.failed(reason)`. Lanes are decoded one
/// at a time inside the stage that needs them, so at most one
/// `AudioBuffer16k` is alive. One operation runs per meeting at a time: a
/// second `process`, `rerunSummary` or `redeliver` on a meeting that is in
/// flight throws instead of interleaving writes with the first. Stage
/// durations feed `MeetingStore.record` when the meeting was alone in flight
/// for the whole stage, so overlapping runs never pollute the rates. Both
/// engines are prepared through one in-flight task shared by `warmUp()` and
/// `process`, so a warm-up racing a run loads the models once.
public actor ProcessingPipeline {
  let dependencies: PipelineDependencies
  private var running: [UUID: Task<Void, Never>] = [:]
  private var inFlight: Set<UUID> = []
  /// Counts every admission to `inFlight`; a stage whose count moved was
  /// not alone for its whole span.
  private var admissions = 0
  private var runs: [UUID: ProcessingRun] = [:]
  /// The `prepare()` calls in flight, if any; see `prepared()`.
  private var preparing: Task<Void, any Error>?

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

  /// Loads the speech engine and the diarizer now, so a run that starts
  /// later finds them resident: the app calls this when a recording starts,
  /// and the cold CoreML compile lands during the meeting instead of in the
  /// wait after it. The engines' own `prepare()` is idempotent once loaded,
  /// so `process` still calls it; only the load moves. A failure is thrown
  /// to the caller and nothing is remembered: the next `prepare()` tries
  /// again.
  public func warmUp() async throws {
    try await prepared()
  }

  /// Runs both `prepare()` calls once through one task held on the actor
  /// while it is in flight. The pipeline is the serialization point because
  /// the engines cannot be: their `loaded()` methods check a cached manager
  /// and then `await` the load, and an actor is re-entrant across that
  /// `await`, so two callers arriving inside the gap would both load. Every
  /// caller awaits the same task; the task is dropped once it has settled,
  /// so a later run prepares again (a no-op on a loaded engine) and a failed
  /// load is retried rather than cached. Errors carry `.decode` for the
  /// engine and `.diarize` for the diarizer, as the stages did.
  private func prepared() async throws {
    if let task = preparing {
      try await task.value
      return
    }
    let dependencies = self.dependencies
    let task = Task<Void, any Error> {
      do {
        try await dependencies.speechEngine.prepare()
      } catch {
        throw PipelineFailure.wrapping(error, stage: .decode)
      }
      do {
        try await dependencies.diarizer.prepare()
      } catch {
        throw PipelineFailure.wrapping(error, stage: .diarize)
      }
    }
    preparing = task
    defer { if preparing == task { preparing = nil } }
    try await task.value
  }

  /// Runs every stage: `queued → processing → ready`, or `failed(reason)`
  /// with whatever was persisted so far (the transcript survives a cleanup
  /// or summarize failure). Once `persist` has marked the meeting `.ready`
  /// nothing downgrades it: a `retention` error is thrown to the caller and
  /// the meeting stays ready and delivered. Both engines are prepared before
  /// the first event, through the task `warmUp()` shares, so a cold model
  /// load lands before the run's clock starts and never enters a rate. The
  /// last transcribed lane's buffer is handed to `diarize` and released
  /// before `matchSpeakers`, so at most one buffer is alive and the diarized
  /// lane is not decoded twice. A meeting processed again starts a new run
  /// whose first event is `.decode` at fraction 0.
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
        try await prepared()
        let settings = try await attributing(.decode) { try await dependencies.settings.load() }
        let rates = try await attributing(.decode) { try await store.stageRates() }
        runs[meeting.id] = ProcessingRun(
          estimator: ProcessingEstimator(
            duration: meeting.duration, lanes: asset.lanes,
            speechEngineID: dependencies.speechEngine.id,
            llmModel: ProcessingEstimator.llmModelKey(settings), rates: rates),
          stages: PipelineStage.allCases, stopwatch: Stopwatch(dependencies.clock))
        var current = meeting
        current.state = .processing
        let (transcription, diarization) = try await transcribeAndDiarize(
          asset: asset, meeting: current)
        current.language = transcription.language
        var diarized = diarization
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
  /// deliveries as they were; only `process` marks `.failed`. Progress is
  /// posted over a run of `[.summarize, .deliver]`.
  public func rerunSummary(meetingID: UUID, templateID: String) async throws {
    guard let meeting = try await store.meeting(id: meetingID) else {
      throw PipelineFailure(stage: .summarize, reason: "meeting \(meetingID) not found")
    }
    try await exclusively(meetingID, stage: .summarize) {
      let export = try await attributing(.summarize) {
        try await store.export(meetingID: meetingID)
      }
      runs[meetingID] = try await attributing(.summarize) {
        try await makeRun(
          meeting: meeting, lanes: export.audio?.lanes ?? [],
          tokens: ProcessingEstimator.tokenCount(export.segments),
          stages: [.summarize, .deliver])
      }
      var current = meeting
      current.templateID = templateID
      current.state = .ready
      _ = try await summarize(
        meeting: current, segments: export.segments, speakers: export.speakers)
      await deliver(meetingID: meetingID)
    }
  }

  /// Deliver only: the one re-export entry point. Progress is posted over a
  /// run of `[.deliver]`.
  public func redeliver(meetingID: UUID) async throws {
    guard let meeting = try await store.meeting(id: meetingID) else {
      throw PipelineFailure(stage: .deliver, reason: "meeting \(meetingID) not found")
    }
    try await exclusively(meetingID, stage: .deliver) {
      runs[meetingID] = try await attributing(.deliver) {
        let asset = try await store.asset(meetingID: meetingID)
        return try await makeRun(
          meeting: meeting, lanes: asset?.lanes ?? [], tokens: nil, stages: [.deliver])
      }
      await deliver(meetingID: meetingID)
    }
  }

  /// The two stages that share a buffer: `decodeAndTranscribe` hands its
  /// last lane to `diarize`, and the buffer is local to this call, so it is
  /// gone before `matchSpeakers` and never outlives `diarize` through
  /// `merge`.
  private func transcribeAndDiarize(asset: AudioAsset, meeting: Meeting) async throws -> (
    Transcription, Diarization
  ) {
    let (transcription, lastLane) = try await decodeAndTranscribe(
      asset: asset, meetingID: meeting.id)
    let diarization = try await diarize(asset: asset, meeting: meeting, buffer: lastLane)
    return (transcription, diarization)
  }

  // MARK: - Stage plumbing

  /// A run over `stages` for a meeting outside `process`: the stored rates,
  /// the configured engine and model, the given lanes and token count.
  private func makeRun(
    meeting: Meeting, lanes: [AudioLane], tokens: Int?, stages: [PipelineStage]
  ) async throws -> ProcessingRun {
    let settings = try await dependencies.settings.load()
    let rates = try await store.stageRates()
    return ProcessingRun(
      estimator: ProcessingEstimator(
        duration: meeting.duration, lanes: lanes, tokens: tokens,
        speechEngineID: dependencies.speechEngine.id,
        llmModel: ProcessingEstimator.llmModelKey(settings), rates: rates),
      stages: stages, stopwatch: Stopwatch(dependencies.clock))
  }

  /// Marks `meetingID` in flight for the duration of `body`; a second
  /// operation on the same meeting throws a `PipelineFailure` for `stage`.
  /// The meeting's `ProcessingRun`, if `body` made one, goes with it.
  private func exclusively<T: Sendable>(
    _ meetingID: UUID, stage: PipelineStage, _ body: () async throws -> T
  ) async throws -> T {
    guard inFlight.insert(meetingID).inserted else {
      throw PipelineFailure(stage: stage, reason: "meeting \(meetingID) is already being processed")
    }
    admissions += 1
    defer {
      inFlight.remove(meetingID)
      runs[meetingID] = nil
    }
    return try await body()
  }

  /// Replaces the run's guessed token count with the transcript's, the one
  /// re-estimate inside a run; `cleanup` calls it before posting.
  func revise(tokens: Int, meetingID: UUID) {
    runs[meetingID]?.estimator.tokens = tokens
  }

  /// Posts `progress` for `stage` (lane `lane` inside transcribe) starting on
  /// `meetingID`, computed and clamped on the meeting's run. A stage called
  /// outside a run (tests reach stages directly) posts over a fresh run on
  /// the seeds that is not kept.
  func post(_ stage: PipelineStage, lane: Int = 0, meetingID: UUID) async {
    var run = runs[meetingID] ?? standaloneRun()
    let progress = run.progress(stage, lane: lane, elapsed: run.stopwatch.elapsed)
    if runs[meetingID] != nil { runs[meetingID] = run }
    await dependencies.events.post(.progress(meetingID: meetingID, progress: progress))
  }

  private func standaloneRun() -> ProcessingRun {
    ProcessingRun(
      estimator: ProcessingEstimator(
        duration: 0, lanes: [], speechEngineID: dependencies.speechEngine.id,
        llmModel: StageRates.fakeModel, rates: .seeds),
      stages: PipelineStage.allCases, stopwatch: Stopwatch(dependencies.clock))
  }

  /// Turns any error thrown by `body` into a `PipelineFailure` carrying
  /// `stage`, without posting progress (work before a stage starts, the
  /// second lane's decode).
  func attributing<T: Sendable>(_ stage: PipelineStage, _ body: () async throws -> T)
    async rethrows -> T
  {
    do {
      return try await body()
    } catch {
      throw PipelineFailure.wrapping(error, stage: stage)
    }
  }

  /// Posts `progress` for `stage`, runs `body` attributing its errors to the
  /// stage, and measures it on the dependencies' clock. The duration is
  /// recorded as a rate sample once the stage is complete (its last lane
  /// for transcribe) when the meeting was alone in flight for the whole
  /// stage; a body that threw records nothing.
  func run<T: Sendable>(
    _ stage: PipelineStage, lane: Int = 0, meetingID: UUID, _ body: () async throws -> T
  ) async rethrows -> T {
    await post(stage, lane: lane, meetingID: meetingID)
    let alone = inFlight.count == 1
    let admissionsBefore = admissions
    let watch = Stopwatch(dependencies.clock)
    let value = try await attributing(stage, body)
    let seconds = watch.elapsed.timeInterval
    await recordTiming(
      stage, lane: lane, seconds: seconds, alone: alone && admissions == admissionsBefore,
      meetingID: meetingID)
    return value
  }

  private func recordTiming(
    _ stage: PipelineStage, lane: Int, seconds: Double, alone: Bool, meetingID: UUID
  ) async {
    guard var run = runs[meetingID] else { return }
    run.measure(stage, seconds: seconds, alone: alone)
    runs[meetingID] = run
    let lastLane = max(run.estimator.lanes.count, 1) - 1
    guard stage != .transcribe || lane == lastLane,
      let sample = run.sample(stage, recordedAt: now)
    else { return }
    // The rates are a convenience; a bookkeeping failure never fails a run.
    try? await store.record(sample)
  }
}
