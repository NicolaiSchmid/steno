import EventKit
import Foundation
import StenoAdapters
import StenoAudio
import StenoCore
import StenoHandover
import StenoSpeech

/// The composition root. Module types are injected as they are; the four
/// app protocols stand in front of system frameworks. `live()` wires the
/// product, `preview()` an in-memory store with the modules' fakes (also the
/// `-steno-ui-testing` launch mode). Nothing here contains logic the CLI
/// would also need.
@MainActor
@Observable
final class AppEnvironment {
  typealias MakeCaptureSession = @Sendable (CaptureConfiguration) throws -> CaptureSession
  typealias MakeDependencies = @Sendable (Settings, _ apiKey: String?) throws ->
    PipelineDependencies

  let store: MeetingStore
  let settings: SettingsStore
  let secrets: any SecretStore
  let events: MeetingEventBus
  let makeCaptureSession: MakeCaptureSession
  let detector: MeetingDetector
  let models: ModelStore
  let speakerMemory: any SpeakerMemory
  let sweep: RetentionSweep
  let calendar: any CalendarProviding
  let loginItem: any LoginItemControlling
  let permissions: any PermissionsChecking
  let updater: any UpdaterControlling
  let clock: any Clock<Duration>
  let now: @Sendable () -> Date
  let isPreview: Bool

  /// Rebuilt by `reloadPipeline()` when the speech engine or the LLM
  /// endpoint changes in Settings, so neither needs a relaunch.
  private(set) var pipeline: ProcessingPipeline
  /// Pipelines `reloadPipeline()` retired, kept alive until their runs end.
  private var draining: [ObjectIdentifier: ProcessingPipeline] = [:]
  private let makeDependencies: MakeDependencies
  /// nil when the handover identity could not be created (locked keychain);
  /// the Phones settings say so.
  private(set) var handover: HandoverService?
  private(set) var startupWarnings: [String] = []

  init(
    store: MeetingStore,
    settings: SettingsStore,
    secrets: any SecretStore,
    events: MeetingEventBus,
    makeCaptureSession: @escaping MakeCaptureSession,
    detector: MeetingDetector,
    pipeline: ProcessingPipeline,
    makeDependencies: @escaping MakeDependencies,
    models: ModelStore,
    speakerMemory: any SpeakerMemory,
    handover: HandoverService?,
    sweep: RetentionSweep,
    calendar: any CalendarProviding,
    loginItem: any LoginItemControlling,
    permissions: any PermissionsChecking,
    updater: any UpdaterControlling,
    clock: any Clock<Duration>,
    now: @escaping @Sendable () -> Date = Date.init,
    isPreview: Bool = false
  ) {
    self.store = store
    self.settings = settings
    self.secrets = secrets
    self.events = events
    self.makeCaptureSession = makeCaptureSession
    self.detector = detector
    self.pipeline = pipeline
    self.makeDependencies = makeDependencies
    self.models = models
    self.speakerMemory = speakerMemory
    self.handover = handover
    self.sweep = sweep
    self.calendar = calendar
    self.loginItem = loginItem
    self.permissions = permissions
    self.updater = updater
    self.clock = clock
    self.now = now
    self.isPreview = isPreview
  }

  // MARK: - Runtime

  /// Load, mutate, save: every settings edit in the app goes through here.
  @discardableResult
  func updateSettings(_ mutate: (inout Settings) -> Void) async throws -> Settings {
    var current = try await settings.load()
    mutate(&current)
    try await settings.save(current)
    return current
  }

  /// The one place launch-at-login is written: the login item and the
  /// setting together, for the menu bar toggle and the General tab alike.
  func setLaunchAtLogin(_ enabled: Bool) async throws {
    try loginItem.setEnabled(enabled)
    try await updateSettings { $0.launchAtLogin = enabled }
  }

  /// Replaces the pipeline with one built from the stored settings and the
  /// keychain's API key. The swap comes first, so a Save never waits for a
  /// run in progress and every later `enqueue` lands on the replacement; the
  /// retired pipeline is retained until it is idle, so meetings in flight
  /// finish on the dependencies they started with.
  func reloadPipeline() async throws {
    let settings = try await settings.load()
    let apiKey = try await secrets.secret(for: .llmAPIKey)
    let replacement = ProcessingPipeline(dependencies: try makeDependencies(settings, apiKey))
    let retired = pipeline
    pipeline = replacement
    draining[ObjectIdentifier(retired)] = retired
    Task { [weak self] in
      await retired.waitUntilIdle()
      self?.draining[ObjectIdentifier(retired)] = nil
    }
  }

  /// StenoCore's sweep; the app owns no deletion code. Runs at launch and
  /// after every processed meeting. A partial sweep is reported, not fatal.
  @discardableResult
  func runRetentionSweep() async -> [URL] {
    do {
      return try await sweep.run(now: now())
    } catch {
      startupWarnings.append("Retention sweep incomplete: \(error)")
      return []
    }
  }

  /// A recording the app did not finish (a crash mid-meeting) stays
  /// `.recording` in the store; at launch it becomes `.failed` so the list
  /// never shows a phantom red dot. The master file is still on disk.
  func reconcileInterruptedRecordings() async {
    do {
      _ = try await store.failInterruptedRecordings(now: now())
    } catch {
      startupWarnings.append("Interrupted recordings could not be marked: \(error)")
    }
  }

  /// A meeting left `.queued` or `.processing` by the last process (Quit or
  /// a crash during the LLM pass) is processed again from the start;
  /// without this it would sit in the queue forever with no button to
  /// reach it. Returns the meetings resumed.
  @discardableResult
  func resumeUnfinishedProcessing() async -> [UUID] {
    do {
      return try await pipeline.resumeUnfinished()
    } catch {
      startupWarnings.append("Unfinished meetings could not be resumed: \(error)")
      return []
    }
  }

  /// Loads the speech engine and the diarizer on the current pipeline while
  /// a recording runs, so the cold model load is over before the meeting
  /// ends and never sits in the wait the owner watches. Only when both
  /// models are on disk, because a `prepare()` may download and nothing
  /// downloads during a call; the guard reads the engine from the setting,
  /// not from the pipeline's engine, so the preview's `FakeSpeechEngine` is
  /// guarded by the `parakeet-v3` marker files like the real one. A failure
  /// is swallowed: the run's own `prepare()` reports it, and a warning here
  /// would sit in the menu bar during the recording for nothing the owner
  /// can act on. A `reloadPipeline()` during the recording yields a cold
  /// replacement, which is accepted.
  func warmUpPipelineIfModelsInstalled() async {
    guard let settings = try? await settings.load(),
      let engine = try? SpeechEngineID(settingsValue: settings.speechEngineID),
      models.isInstalled(engine.asset), models.isInstalled(.offlineDiarizer)
    else { return }
    try? await pipeline.warmUp()
  }

  /// Core's Mac recording transaction over the current pipeline, so a
  /// reload between start and stop never strands the recording.
  func makeLocalIntake() -> LocalRecordingIntake {
    LocalRecordingIntake(
      store: store, settings: settings,
      enqueue: { [weak self] meeting, asset in
        guard let pipeline = await MainActor.run(body: { self?.pipeline }) else {
          throw PipelineFailure(stage: .decode, reason: "the app is shutting down")
        }
        try await pipeline.enqueue(meeting, asset: asset)
      },
      now: now)
  }

  // MARK: - Roots

  /// The product: on-disk store under Application Support, the live capture
  /// backend, the real engines and diarizer over `ModelStore`, cosine speaker
  /// memory, the Obsidian coordinator, the handover listener with its
  /// login-keychain identity, EventKit, ServiceManagement, TCC and Sparkle.
  /// One `EKEventStore` for the calendar service and the permission check.
  private static let eventStore = EKEventStore()

  static func live(updater: any UpdaterControlling) async throws -> AppEnvironment {
    let paths = try StenoPaths.default()
    // One bus: the store posts `deleted` on it, the pipeline `progress`,
    // `speakersNeedReview` and `retentionApplied`; the app subscribes once.
    let events = MeetingEventBus()
    let store = try MeetingStore.onDisk(at: paths.databaseURL, events: events)
    let settingsStore = SettingsStore(writer: store.writer)
    let settings = try await settingsStore.load()
    let secrets = KeychainSecretStore()
    var warnings: [String] = []
    var apiKey: String?
    do {
      apiKey = try await secrets.secret(for: .llmAPIKey)
    } catch {
      warnings.append("Could not read the LLM API key from the keychain: \(error)")
    }
    let models = ModelStore(directory: settings.modelsDirectory)
    let memory = CosineSpeakerMemory(store: store)
    let makeDependencies: MakeDependencies = { settings, apiKey in
      let engineID = (try? SpeechEngineID(settingsValue: settings.speechEngineID)) ?? .parakeetV3
      let llm = LLMWiring.passes(settings: settings, apiKey: apiKey)
      return PipelineDependencies(
        decoder: AVFoundationAudioCodec(),
        speechEngine: try makeSpeechEngine(engineID, models: models),
        diarizer: try makeDiarizer(models: models),
        speakerMemory: memory,
        cleaner: llm?.cleaner,
        summarizer: llm?.summarizer,
        dispatcher: DeliveryCoordinator(store: store, settings: settingsStore),
        store: store,
        settings: settingsStore,
        events: events)
    }
    let pipeline = ProcessingPipeline(dependencies: try makeDependencies(settings, apiKey))
    let environment = AppEnvironment(
      store: store,
      settings: settingsStore,
      secrets: secrets,
      events: events,
      makeCaptureSession: { configuration in try CaptureSession(configuration: configuration) },
      detector: MeetingDetector(),
      pipeline: pipeline,
      makeDependencies: makeDependencies,
      models: models,
      speakerMemory: memory,
      handover: nil,
      sweep: RetentionSweep(store: store),
      calendar: CalendarService(eventStore: eventStore),
      loginItem: LoginItemController(),
      permissions: PermissionsService(eventStore: eventStore),
      updater: updater,
      clock: ContinuousClock())
    environment.startupWarnings = warnings
    do {
      let identity = try IdentityKeychain.loadOrCreate(
        commonName: "Steno on \(HandoverConfiguration.defaultServiceName())")
      environment.handover = HandoverService(
        configuration: HandoverConfiguration(),
        store: store,
        intake: environment.makeIntake(),
        identity: identity)
    } catch {
      environment.startupWarnings.append("Phone handover is unavailable: \(error)")
    }
    return environment
  }

  /// Beside `-steno-ui-testing`: the preview's `FakeSpeechEngine` sleeps in
  /// every `transcribe` for `uiTestingTranscribeHold` and the sample meeting
  /// is queued for processing over a synthetic recording, so the UI test can
  /// watch the processing card cross the transcribe stage and vanish.
  static let holdTranscribeArgument = "-steno-ui-testing-hold-transcribe"
  /// Per lane, so the sample call stays in transcribe for twice this. The
  /// run starts at launch, before the test has a window; the hold is long
  /// enough that a slow first launch on a hosted runner still finds the
  /// card in transcribe.
  static let uiTestingTranscribeHold: Duration = .seconds(60)

  /// In-memory store seeded with StenoCore's `SampleData` meeting, the
  /// synthetic capture backend, fake engines, `FakeModelDownloader`, a fake
  /// HAL source for the detector and the app-protocol fakes with every
  /// permission granted. No file outside a fresh temporary directory, no
  /// network, no prompts. `handover` stays nil unless a test passes one;
  /// tests that need a failing or device-losing capture pass
  /// `makeCaptureSession`, drive the detector through `processActivity`,
  /// gate or observe the pipeline through `makeSpeechEngine`, `makeDiarizer`
  /// and `makeSummarizer` (each called once per pipeline build) and the
  /// recording start through `calendar`. Without `makeSpeechEngine` the
  /// engine is a `FakeSpeechEngine` whose `onTranscribe` sleeps for
  /// `uiTestingTranscribeHold` under `holdTranscribeArgument`, which also
  /// queues the seeded meeting; without `makeDiarizer` the diarizer is a
  /// `FakeDiarizer`; without `makeSummarizer` the summarizer is a
  /// `FakeSummarizer`. `seed` picks the fixture set (`.sample` is the one
  /// meeting the unit tests count on, `.rich` adds three days of them) or,
  /// nil, leaves the store empty.
  static func preview(
    clock: any Clock<Duration> = ContinuousClock(),
    now: @escaping @Sendable () -> Date = Date.init,
    handover: HandoverService? = nil,
    seed: PreviewSeed.Set? = .sample,
    makeCaptureSession: MakeCaptureSession? = nil,
    processActivity: FakeProcessAudioActivity = FakeProcessAudioActivity(),
    makeSpeechEngine: (@Sendable () -> any SpeechEngine)? = nil,
    makeDiarizer: (@Sendable () -> any Diarizer)? = nil,
    makeSummarizer: (@Sendable () -> any MeetingSummarizer)? = nil,
    calendar: (any CalendarProviding)? = nil
  ) async throws -> AppEnvironment {
    let root = FileManager.default.temporaryDirectory
      .appendingPathComponent("steno-preview-\(UUID().uuidString)", isDirectory: true)
    try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
    let events = MeetingEventBus()
    let store = try MeetingStore.inMemory(events: events)
    let settingsStore = SettingsStore(writer: store.writer)
    var settings = Settings()
    settings.audioFolder = root.appendingPathComponent("audio", isDirectory: true)
    settings.modelsDirectory = root.appendingPathComponent("models", isDirectory: true)
    settings.launchAtLogin = false
    try await settingsStore.save(settings)
    let holdTranscribe: Duration? =
      CommandLine.arguments.contains(holdTranscribeArgument) ? uiTestingTranscribeHold : nil
    if let seed {
      try await PreviewSeed.seed(store, set: seed, audioFolder: settings.audioFolder)
      if holdTranscribe != nil {
        try await PreviewSeed.queueForProcessing(store, audioFolder: settings.audioFolder)
      }
    }

    let models = ModelStore(
      directory: settings.modelsDirectory, downloader: FakeModelDownloader())
    let memory = CosineSpeakerMemory(store: store)
    let makeSpeechEngine: @Sendable () -> any SpeechEngine =
      makeSpeechEngine ?? {
        var engine = FakeSpeechEngine()
        if let holdTranscribe {
          engine.onTranscribe = { try await ContinuousClock().sleep(for: holdTranscribe) }
        }
        return engine
      }
    let makeDiarizer: @Sendable () -> any Diarizer = makeDiarizer ?? { FakeDiarizer() }
    let makeSummarizer: @Sendable () -> any MeetingSummarizer =
      makeSummarizer ?? { FakeSummarizer() }
    let makeDependencies: MakeDependencies = { _, _ in
      PipelineDependencies(
        decoder: AVFoundationAudioCodec(),
        speechEngine: makeSpeechEngine(),
        diarizer: makeDiarizer(),
        speakerMemory: memory,
        cleaner: PassthroughCleaner(),
        summarizer: makeSummarizer(),
        dispatcher: DeliveryCoordinator(store: store, settings: settingsStore, now: now),
        store: store,
        settings: settingsStore,
        events: events,
        now: now)
    }
    let pipeline = ProcessingPipeline(dependencies: try makeDependencies(settings, nil))
    return AppEnvironment(
      store: store,
      settings: settingsStore,
      secrets: FileSecretStore(
        url: root.appendingPathComponent("secrets.json"), environment: [:]),
      events: events,
      makeCaptureSession: makeCaptureSession ?? { configuration in
        try CaptureSession(
          configuration: configuration,
          backend: SyntheticCaptureBackend(
            lanes: configuration.lanes, tone: [.mic: 440, .system: 660, .mixed: 440],
            seconds: 2))
      },
      detector: MeetingDetector(source: processActivity, clock: clock),
      pipeline: pipeline,
      makeDependencies: makeDependencies,
      models: models,
      speakerMemory: memory,
      handover: handover,
      sweep: RetentionSweep(store: store),
      calendar: calendar ?? FakeCalendar(),
      loginItem: FakeLoginItem(),
      permissions: FakePermissions.allGranted(),
      updater: FakeUpdater(),
      clock: clock,
      now: now,
      isPreview: true)
  }

  /// Core's `RecordingIntake` over whatever pipeline is current when a phone
  /// recording completes, so a pipeline reload never strands the listener.
  func makeIntake() -> RecordingIntake {
    RecordingIntake(
      store: store, settings: settings,
      enqueue: { [weak self] meeting, asset in
        guard let pipeline = await MainActor.run(body: { self?.pipeline }) else {
          throw PipelineFailure(stage: .decode, reason: "the app is shutting down")
        }
        try await pipeline.enqueue(meeting, asset: asset)
      },
      now: now)
  }
}

/// StenoCore's sample meeting written into a store, the way the pipeline
/// would have left it: meeting plus asset, participants, transcript and
/// speakers, summary with tasks and decisions. `.rich` adds four synthetic
/// meetings over the two days before it, so the grouped list, the counts
/// and every entry state can be seen and screenshotted.
enum PreviewSeed {
  /// Which fixtures the preview store starts with.
  enum Set: Equatable, Sendable {
    /// The sample meeting alone; the unit tests' counts assume it.
    case sample
    /// The sample meeting plus `richMeetings()`: a processing and a failed
    /// meeting and two more ready ones over three days; the sample stays
    /// newest. The processing meeting carries a synthetic master, so
    /// `resumeUnfinishedProcessing()` runs it at launch as the product
    /// would, instead of failing it for a missing asset.
    case rich
  }

  static func seed(_ store: MeetingStore, set: Set = .sample, audioFolder: URL? = nil)
    async throws
  {
    for person in SampleData.persons() { try await store.save(person) }
    let meeting = SampleData.meeting()
    try await store.save(meeting, asset: SampleData.audioAsset())
    for participant in SampleData.participants() { try await store.save(participant) }
    try await store.replaceTranscript(
      meeting, segments: SampleData.segments(), speakers: SampleData.speakers())
    try await store.replaceSummary(
      meeting, tasks: SampleData.tasks(), decisions: SampleData.decisions().map(\.text))
    guard set == .rich else { return }
    for extra in richMeetings() {
      if extra.state == .processing, let audioFolder {
        try await saveWithSyntheticMaster(extra, store: store, audioFolder: audioFolder)
      } else {
        try await store.save(extra)
      }
    }
  }

  /// The four extra meetings of `.rich`, placed before `anchor` (the sample
  /// meeting's start): two ready meetings with two-bullet summaries, one
  /// processing and one failed, over the two preceding days, with every
  /// `TitleOrigin` the display title distinguishes. Pure, so a test can pin
  /// the set without a store.
  static func richMeetings(before anchor: Date = SampleData.startedAt) -> [Meeting] {
    let hour: TimeInterval = 3_600
    func meeting(
      _ n: Int, title: String, origin: TitleOrigin, hoursBefore: Double, duration: TimeInterval,
      source: MeetingSource, tags: [String], state: MeetingState, bullets: [(String, String)]
    ) -> Meeting {
      let startedAt = anchor.addingTimeInterval(-hoursBefore * hour)
      let summary =
        bullets.isEmpty
        ? nil
        : SummaryDocument(
          templateID: SummaryTemplate.defaultID, language: "de",
          sections: [
            SummarySection(
              id: "executive-summary", heading: "Executive Summary",
              bullets: bullets.map { SummaryBullet(lead: $0.0, text: $0.1) })
          ])
      return Meeting(
        id: SampleData.uuid(n), title: title, startedAt: startedAt, duration: duration,
        language: "de", source: source, tags: tags, state: state, titleOrigin: origin,
        summary: summary, createdAt: startedAt, updatedAt: startedAt.addingTimeInterval(duration))
    }
    return [
      meeting(
        101, title: "Wochenplanung", origin: .summary, hoursBefore: 19, duration: 1_800,
        source: .macInPerson, tags: ["team"], state: .ready,
        bullets: [
          ("Sprintziel", "Die Aufnahme-Ansicht ist bis Freitag im TestFlight."),
          ("Blocker", "Der Export nach Obsidian wartet auf die Vault-Auswahl."),
        ]),
      meeting(
        102, title: "Call 2026-09-23 12:00", origin: .default, hoursBefore: 23, duration: 12,
        source: .macCall, tags: [], state: .processing, bullets: []),
      meeting(
        103, title: "Interview mit Lena", origin: .calendar, hoursBefore: 41, duration: 2_700,
        source: .macCall, tags: ["hiring"], state: .ready,
        bullets: [
          ("Eindruck", "Klare Antworten zu Priorisierung und Teamarbeit."),
          ("Nächster Schritt", "Zweites Gespräch mit dem Design-Team vereinbaren."),
        ]),
      meeting(
        104, title: "Call 2026-09-22 11:00", origin: .default, hoursBefore: 48, duration: 600,
        source: .macCall, tags: [],
        state: .failed(
          reason:
            "The LLM endpoint did not answer.\nRe-run the summary once it is reachable."),
        bullets: []),
    ]
  }

  /// The sample meeting as a recording the pipeline still has to process:
  /// the meeting `.queued`, so `resumeUnfinishedProcessing()` starts it at
  /// launch, over a synthetic master under `audioFolder`. The seeded
  /// transcript and summary stay until the run replaces them; the UI test
  /// looks for the template heading both carry.
  static func queueForProcessing(_ store: MeetingStore, audioFolder: URL) async throws {
    try await saveWithSyntheticMaster(
      SampleData.meeting(state: .queued), store: store, audioFolder: audioFolder)
  }

  /// Saves `meeting` with a synthetic two-lane master under `audioFolder`
  /// in the recording layout. The master is 16 kHz Int16 WAV, one channel
  /// per lane, which `AVFoundationAudioCodec` decodes and mixes down without
  /// a CAF writer. The sample meeting keeps the sample asset's id; any other
  /// meeting gets its own.
  private static func saveWithSyntheticMaster(
    _ meeting: Meeting, store: MeetingStore, audioFolder: URL
  ) async throws {
    let layout = RecordingLayout(audioFolder: audioFolder, meetingID: meeting.id)
    try layout.createDirectories()
    var asset = SampleData.audioAsset()
    if meeting.id != SampleData.meetingID {
      asset.id = UUID()
      asset.meetingID = meeting.id
    }
    asset.format = .wav16kInt16
    asset.url = layout.master(.wav16kInt16)
    asset.sidecars16k = [:]
    asset.mixdownURL = nil
    let samples = tone(seconds: meeting.duration, channels: asset.lanes.count)
    try WAVWriter.data(
      samples, sampleRate: Int(AudioBuffer16k.sampleRate), channels: asset.lanes.count
    ).write(to: asset.url, options: .atomic)
    try await store.save(meeting, asset: asset)
  }

  /// `seconds` of a 440 Hz sine at half scale, the same on every channel,
  /// interleaved.
  private static func tone(seconds: TimeInterval, channels: Int) -> [Int16] {
    let rate = AudioBuffer16k.sampleRate
    let frames = Int(seconds * rate)
    var samples: [Float] = []
    samples.reserveCapacity(frames * channels)
    for frame in 0..<frames {
      let value = Float(0.5 * sin(2 * Double.pi * 440 * Double(frame) / rate))
      for _ in 0..<channels { samples.append(value) }
    }
    return WAVWriter.int16(samples)
  }
}
