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
    guard let meetings = try? await store.meetings(limit: 200) else { return }
    for meeting in meetings where meeting.state == .recording {
      try? await store.setState(
        .failed(reason: "Recording was interrupted before it finished."), meetingID: meeting.id,
        now: now())
    }
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
    let store = try MeetingStore.onDisk(at: paths.databaseURL)
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
    let events = MeetingEventBus()
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
        cleaner: llm?.cleaner ?? PassthroughCleaner(),
        summarizer: llm?.summarizer ?? FakeSummarizer(),
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

  /// In-memory store seeded with StenoCore's `SampleData` meeting, the
  /// synthetic capture backend, fake engines, `FakeModelDownloader`, a fake
  /// HAL source for the detector and the app-protocol fakes with every
  /// permission granted. No file outside a fresh temporary directory, no
  /// network, no prompts. `handover` stays nil unless a test passes one;
  /// tests that need a failing or device-losing capture pass
  /// `makeCaptureSession`, drive the detector through `processActivity`,
  /// gate the pipeline through `makeSpeechEngine` (called once per pipeline
  /// build) and the recording start through `calendar`.
  static func preview(
    clock: any Clock<Duration> = ContinuousClock(),
    now: @escaping @Sendable () -> Date = Date.init,
    handover: HandoverService? = nil,
    seed: Bool = true,
    makeCaptureSession: MakeCaptureSession? = nil,
    processActivity: FakeProcessAudioActivity = FakeProcessAudioActivity(),
    makeSpeechEngine: @escaping @Sendable () -> any SpeechEngine = { FakeSpeechEngine() },
    calendar: (any CalendarProviding)? = nil
  ) async throws -> AppEnvironment {
    let root = FileManager.default.temporaryDirectory
      .appendingPathComponent("steno-preview-\(UUID().uuidString)", isDirectory: true)
    try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
    let store = try MeetingStore.inMemory()
    let settingsStore = SettingsStore(writer: store.writer)
    var settings = Settings()
    settings.audioFolder = root.appendingPathComponent("audio", isDirectory: true)
    settings.modelsDirectory = root.appendingPathComponent("models", isDirectory: true)
    settings.launchAtLogin = false
    try await settingsStore.save(settings)
    if seed { try await PreviewSeed.seed(store) }

    let events = MeetingEventBus()
    let models = ModelStore(
      directory: settings.modelsDirectory, downloader: FakeModelDownloader())
    let memory = CosineSpeakerMemory(store: store)
    let makeDependencies: MakeDependencies = { _, _ in
      PipelineDependencies(
        decoder: AVFoundationAudioCodec(),
        speechEngine: makeSpeechEngine(),
        diarizer: FakeDiarizer(),
        speakerMemory: memory,
        cleaner: PassthroughCleaner(),
        summarizer: FakeSummarizer(),
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
/// speakers, summary with tasks and decisions.
enum PreviewSeed {
  static func seed(_ store: MeetingStore) async throws {
    for person in SampleData.persons() { try await store.save(person) }
    let meeting = SampleData.meeting()
    try await store.save(meeting, asset: SampleData.audioAsset())
    for participant in SampleData.participants() { try await store.save(participant) }
    try await store.replaceTranscript(
      meeting, segments: SampleData.segments(), speakers: SampleData.speakers())
    try await store.replaceSummary(
      meeting, tasks: SampleData.tasks(), decisions: SampleData.decisions().map(\.text))
  }
}
