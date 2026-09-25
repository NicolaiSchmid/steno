import Foundation
import Testing

@testable import StenoCore

/// A pipeline over an in-memory store, temp audio folder and every fake,
/// with hooks to swap single dependencies.
struct PipelineHarness {
  let directory: URL
  let store: MeetingStore
  let settingsStore: SettingsStore
  let events: MeetingEventBus
  let engine: FakeSpeechEngine
  let diarizer: FakeDiarizer
  let memory: InMemorySpeakerMemory
  let cleaner: any TranscriptCleaner
  let summarizer: FakeSummarizer
  let destination: FakeDestination
  let dispatcher: FakeDeliveryDispatcher
  let pipeline: ProcessingPipeline
  var settings: Settings

  static let now = SampleData.updatedAt

  init(
    engine: FakeSpeechEngine = FakeSpeechEngine(),
    diarizer: FakeDiarizer = FakeDiarizer(),
    memory: InMemorySpeakerMemory = InMemorySpeakerMemory(people: SampleData.persons()),
    cleaner: any TranscriptCleaner = PassthroughCleaner(),
    summarizer: FakeSummarizer = FakeSummarizer(),
    retention: AudioRetention = .keepDays(30),
    sharedStore: MeetingStore? = nil
  ) async throws {
    directory = try Fixtures.temporaryDirectory("pipeline")
    store = try sharedStore ?? MeetingStore.inMemory()
    settingsStore = SettingsStore(writer: store.writer)
    settings = Settings()
    settings.audioFolder = directory.appendingPathComponent("audio", isDirectory: true)
    settings.defaultRetention = retention
    try await settingsStore.save(settings)
    // The production SpeakerMemory reads persons from the store, so every
    // person the in-memory fake can suggest must exist as a row.
    for person in SampleData.persons() { try await self.store.save(person) }
    events = MeetingEventBus()
    self.engine = engine
    self.diarizer = diarizer
    self.memory = memory
    self.cleaner = cleaner
    self.summarizer = summarizer
    destination = FakeDestination(
      root: directory.appendingPathComponent("vault", isDirectory: true))
    dispatcher = FakeDeliveryDispatcher(
      store: store, destinations: [destination], now: { Self.now })
    pipeline = ProcessingPipeline(
      dependencies: PipelineDependencies(
        decoder: WAVAudioDecoder(), speechEngine: engine, diarizer: diarizer, speakerMemory: memory,
        cleaner: cleaner, summarizer: summarizer, dispatcher: dispatcher, store: store,
        settings: settingsStore, events: events, now: { Self.now }))
  }

  func cleanUp() {
    try? FileManager.default.removeItem(at: directory)
  }

  /// A queued mac call with the two-lane fixture as master and per-lane
  /// sidecars, or a one-lane in-person recording. The fixtures are copied
  /// into `audioFolder/<meetingID>/` (the `RecordingLayout`) the way the
  /// capture writer, the intake and `steno process` place them, so clips and
  /// the mixdown land beside the master and never in `Tests/Fixtures/`.
  func meeting(source: MeetingSource, retention: AudioRetention = .keepDays(30)) throws -> (
    Meeting, AudioAsset
  ) {
    let meeting = Meeting(
      id: SampleData.meetingID, title: "Untitled", startedAt: SampleData.startedAt, duration: 6,
      source: source, state: .recording, createdAt: SampleData.createdAt,
      updatedAt: SampleData.createdAt)
    let layout = RecordingLayout(audioFolder: settings.audioFolder, meetingID: meeting.id)
    try layout.createDirectories()
    func place(_ fixture: String, at url: URL) throws {
      if FileManager.default.fileExists(atPath: url.path) {
        try FileManager.default.removeItem(at: url)
      }
      try FileManager.default.copyItem(at: Fixtures.url(fixture), to: url)
    }
    let master = layout.master(.wav16kInt16)
    try place("audio/conversation-two-lane-6s.wav", at: master)
    let asset: AudioAsset
    switch source {
    case .macCall:
      try place("audio/conversation-mic-6s.wav", at: layout.sidecar(.mic))
      try place("audio/conversation-system-6s.wav", at: layout.sidecar(.system))
      asset = AudioAsset(
        id: SampleData.uuid(70), meetingID: meeting.id, url: master, format: .wav16kInt16,
        lanes: [.mic, .system],
        sidecars16k: [.mic: layout.sidecar(.mic), .system: layout.sidecar(.system)],
        retention: retention)
    case .macInPerson, .phone:
      asset = AudioAsset(
        id: SampleData.uuid(70), meetingID: meeting.id, url: master, format: .wav16kInt16,
        lanes: [.mixed], retention: retention)
    }
    return (meeting, asset)
  }
}
