import Foundation
import StenoCore
import StenoLLM
import Testing

/// The one real-pipeline test across modules. Core creates it with fakes
/// everywhere; each module workstream's last step replaces its own fake with
/// the real type. No models, no network: the LLM passes run against
/// `StubChatServer` on loopback, fed from `Tests/Fixtures/llm/responses/`.
@Suite struct EndToEndTests {
  static let cleanupUsage = LLMUsage(promptTokens: 300, completionTokens: 120, requests: 1)
  static let summaryUsage = LLMUsage(promptTokens: 900, completionTokens: 250, requests: 1)

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
    // StenoLLM's real cleaner and summarizer on the loopback stub: the
    // cleanup answer echoes every segment capitalised, the summary answer
    // is the canned analysis in `llm/responses/e2e-summary.json`.
    let server = try StubChatServer()
    defer { server.stop() }
    let summaryBody = try String(
      contentsOf: Fixtures.url("llm/responses/e2e-summary.json"), encoding: .utf8)
    let echo = Scripts.cleanupEcho(usage: Self.cleanupUsage) { _, text in
      text.prefix(1).uppercased() + text.dropFirst() + "."
    }
    server.respond { request in
      request.purpose == "summary"
        ? Scripts.completion(summaryBody, usage: Self.summaryUsage) : echo(request)
    }
    let endpoint = LLMEndpoint(baseURL: server.baseURL, model: "stub-model")
    let client = OpenAICompatibleClient(endpoint: endpoint, apiKey: nil, retry: .none)
    let cleaner = LLMTranscriptCleaner(model: client, endpoint: endpoint)
    let summarizer = LLMMeetingSummarizer(
      model: client, endpoint: endpoint, timeZone: TimeZone(identifier: "UTC")!)
    let vault = FakeDestination(
      root: directory.appendingPathComponent("vault", isDirectory: true))
    let dispatcher = FakeDeliveryDispatcher(store: store, destinations: [vault], now: { now })
    let pipeline = ProcessingPipeline(
      dependencies: PipelineDependencies(
        decoder: WAVAudioDecoder(),
        speechEngine: FakeSpeechEngine(),
        diarizer: FakeDiarizer(),
        speakerMemory: InMemorySpeakerMemory(people: SampleData.persons()),
        cleaner: cleaner,
        summarizer: summarizer,
        dispatcher: dispatcher,
        store: store,
        settings: settingsStore,
        events: events,
        now: { now }))

    let stream = await events.subscribe()
    let meeting = Meeting(
      id: SampleData.meetingID, title: "Produktstrategie", startedAt: SampleData.startedAt,
      duration: 6, source: .macCall, calendarEventID: "event-1", state: .recording,
      createdAt: SampleData.createdAt, updatedAt: SampleData.createdAt)
    // Placed the way the capture writer would: master and sidecars in the
    // meeting folder, so the pipeline's clips and mixdown land beside them.
    let layout = RecordingLayout(audioFolder: settings.audioFolder, meetingID: meeting.id)
    try layout.createDirectories()
    try FileManager.default.copyItem(
      at: Fixtures.url("audio/conversation-two-lane-6s.wav"), to: layout.master(.wav16kInt16))
    try FileManager.default.copyItem(
      at: Fixtures.url("audio/conversation-mic-6s.wav"), to: layout.sidecar(.mic))
    try FileManager.default.copyItem(
      at: Fixtures.url("audio/conversation-system-6s.wav"), to: layout.sidecar(.system))
    let asset = AudioAsset(
      id: SampleData.uuid(70), meetingID: meeting.id, url: layout.master(.wav16kInt16),
      format: .wav16kInt16, lanes: [.mic, .system],
      sidecars16k: [.mic: layout.sidecar(.mic), .system: layout.sidecar(.system)],
      retention: .keepDays(30))

    try await pipeline.enqueue(meeting, asset: asset)
    await pipeline.waitUntilIdle()

    let stored = try #require(try await store.meeting(id: meeting.id))
    #expect(stored.state == .ready)
    #expect(stored.title == "Produktstrategie", "a calendar title is kept")
    #expect(server.requests.map(\.purpose) == ["cleanup", "summary"])
    #expect(stored.llmUsage == Self.cleanupUsage + Self.summaryUsage)
    #expect(stored.summary?.sections.map(\.id) == ["executive-summary", "full-summary"])

    let deliveries = try await store.deliveries(meetingID: meeting.id)
    #expect(deliveries.count == 1)
    #expect(deliveries.first?.status == .delivered)
    let receipt = try #require(deliveries.first?.receipt)
    #expect(receipt.files.map(\.relativePath) == ["meeting.json"])
    #expect(receipt.rendererVersion == FakeDestination.rendererVersion)

    let json = try Data(contentsOf: vault.exportURL(meetingID: meeting.id))
    let export = try StenoJSON.decode(MeetingExport.self, from: json)
    #expect(export.schemaVersion == MeetingExport.currentSchemaVersion)
    #expect(export.meeting.state == .ready)
    #expect(export.segments.count == 12)
    #expect(
      export.segments.allSatisfy {
        $0.text == $0.rawText.prefix(1).uppercased() + $0.rawText.dropFirst() + "."
      })
    #expect(export.segments.allSatisfy { $0.rawText.hasPrefix("fake segment") })
    #expect(export.tasks.map(\.text) == ["Budgetzahlen prüfen."])
    #expect(export.decisions.map(\.text) == ["Der Kern wird priorisiert."])
    #expect(export.speakers.map(\.clusterLabel) == ["Me", "Speaker 1", "Speaker 2"])
    #expect(receipt.files.first?.sha256 == ContentHash.sha256(json))
    #expect(!SummaryMarkdown.render(export).isEmpty)
    try Snapshot.assert(
      SummaryMarkdown.render(export), matches: "snapshots/e2e/mac-call-summary.md")

    // Everything posted so far, read up to a sentinel so a failed run can
    // never hang the test.
    let sentinel = MeetingEvent.speakersNeedReview(meetingID: meeting.id, speakerIDs: [])
    await events.post(sentinel)
    var iterator = stream.makeAsyncIterator()
    var collected: [MeetingEvent] = []
    while let event = await iterator.next(), event != sentinel { collected.append(event) }
    try #require(collected.count == 11, "ten stage starts and one review request")
    let stages = collected.compactMap { event -> PipelineStage? in
      if case .progress(_, let stage) = event { return stage }
      return nil
    }
    #expect(stages == PipelineStage.allCases)
    #expect(
      collected[8]
        == .speakersNeedReview(meetingID: meeting.id, speakerIDs: export.speakers.map(\.id)))
  }
}
