import Foundation
import StenoCore
import Testing

/// The one real-pipeline test across modules. Core creates it with fakes
/// everywhere; each module workstream's last step replaces its own fake with
/// the real type. No models, no network.
@Suite struct EndToEndTests {
  @Test func macCallFixtureLandsInVault() async throws {
    let directory = try Fixtures.temporaryDirectory("e2e")
    defer { try? FileManager.default.removeItem(at: directory) }
    let now = SampleData.updatedAt

    let store = try MeetingStore.inMemory()
    let settingsStore = SettingsStore(writer: store.writer)
    var settings = Settings()
    settings.audioFolder = directory.appendingPathComponent("audio", isDirectory: true)
    try await settingsStore.save(settings)
    for person in SampleData.persons() { try await store.save(person) }

    let events = MeetingEventBus()
    let cleaner = PassthroughCleaner()
    let summarizer = FakeSummarizer()
    let vault = RecordingDestination(
      root: directory.appendingPathComponent("vault", isDirectory: true))
    let dispatcher = RecordingDispatcher(store: store, destinations: [vault], now: { now })
    let pipeline = ProcessingPipeline(
      dependencies: PipelineDependencies(
        decoder: WAVAudioDecoder(),
        speechEngine: FakeSpeechEngine(),
        diarizer: FakeDiarizer(),
        speakerMemory: InMemorySpeakerMemory(people: SampleData.persons()),
        cleaner: cleaner,
        summarizer: summarizer,
        delivery: dispatcher,
        store: store,
        settings: settingsStore,
        events: events,
        now: { now }))

    let stream = await events.subscribe()
    let meeting = Meeting(
      id: SampleData.meetingID, title: "Produktstrategie", startedAt: SampleData.startedAt,
      duration: 6, source: .macCall, calendarEventID: "event-1", state: .recording,
      createdAt: SampleData.createdAt, updatedAt: SampleData.createdAt)
    let asset = AudioAsset(
      id: SampleData.uuid(70), meetingID: meeting.id,
      url: Fixtures.url("audio/conversation-two-lane-6s.wav"), format: .wav16kInt16,
      lanes: [.mic, .system],
      sidecars16k: [
        .mic: Fixtures.url("audio/conversation-mic-6s.wav"),
        .system: Fixtures.url("audio/conversation-system-6s.wav"),
      ],
      retention: .keepDays(30))

    try await pipeline.enqueue(meeting, asset: asset)
    await pipeline.waitUntilIdle()

    let stored = try #require(try await store.meeting(id: meeting.id))
    #expect(stored.state == .ready)
    #expect(stored.title == "Produktstrategie", "a calendar title is kept")
    #expect(stored.llmUsage == cleaner.usage + summarizer.usage)

    let deliveries = try await store.deliveries(meetingID: meeting.id)
    #expect(deliveries.count == 1)
    #expect(deliveries.first?.status == .delivered)
    let receipt = try #require(deliveries.first?.receipt)
    #expect(receipt.files.map(\.relativePath) == ["meeting.json"])
    #expect(receipt.rendererVersion == RecordingDestination.rendererVersion)

    let json = try Data(contentsOf: vault.exportURL(meetingID: meeting.id))
    let export = try StenoJSON.decode(MeetingExport.self, from: json)
    #expect(export.schemaVersion == MeetingExport.currentSchemaVersion)
    #expect(export.meeting.state == .ready)
    #expect(export.segments.count == 12)
    #expect(export.speakers.map(\.clusterLabel) == ["Me", "Speaker 1", "Speaker 2"])
    #expect(receipt.files.first?.sha256 == ContentHash.sha256(json))
    #expect(!SummaryMarkdown.render(export).isEmpty)

    guard stored.state == .ready else { return }
    var iterator = stream.makeAsyncIterator()
    var collected: [MeetingEvent] = []
    for _ in 0..<11 {
      if let event = await iterator.next() { collected.append(event) }
    }
    let stages = collected.compactMap { event -> PipelineStage? in
      if case .progress(_, let stage, _) = event { return stage }
      return nil
    }
    #expect(stages == PipelineStage.allCases)
    #expect(
      collected[8]
        == .speakersNeedReview(meetingID: meeting.id, speakerIDs: export.speakers.map(\.id)))
  }
}
