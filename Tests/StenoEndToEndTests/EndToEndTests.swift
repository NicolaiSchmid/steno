import Foundation
import StenoAudio
import StenoCore
import Testing

/// The one real-pipeline test across modules. Core creates it with fakes
/// everywhere; each module workstream's last step replaces its own fake with
/// the real type. No models, no network.
@Suite struct EndToEndTests {
  #if canImport(AVFoundation)
    static var decoder: any AudioDecoder { AVFoundationAudioCodec() }

    /// The two 16 kHz fixture lanes upsampled to 48 kHz by sample repetition
    /// and written through the recording writer, no sidecars: every lane the
    /// pipeline decodes goes through the real converter.
    static func makeAsset(layout: RecordingLayout, meetingID: UUID) throws -> AudioAsset {
      let mic = try WAVAudioDecoder.read(Fixtures.url("audio/conversation-mic-6s.wav")).samples
      let system = try WAVAudioDecoder.read(Fixtures.url("audio/conversation-system-6s.wav"))
        .samples
      let writer = try RecordingWriter(layout: layout, lanes: [.mic, .system])
      var micFrame = [Float](repeating: 0, count: 480)
      var systemFrame = [Float](repeating: 0, count: 480)
      for start in stride(from: 0, to: mic.count, by: 160) {
        for index in 0..<160 {
          for repeatIndex in 0..<3 {
            micFrame[index * 3 + repeatIndex] = mic[start + index]
            systemFrame[index * 3 + repeatIndex] = system[start + index]
          }
        }
        try micFrame.withUnsafeBufferPointer { m in
          try systemFrame.withUnsafeBufferPointer { s in
            try writer.write(LaneFrames(frameCount: 480, lanes: [m.baseAddress!, s.baseAddress!]))
          }
        }
      }
      let files = try writer.finish()
      for sidecar in files.sidecars16k.values { try FileManager.default.removeItem(at: sidecar) }
      return AudioAsset(
        id: SampleData.uuid(70), meetingID: meetingID, url: files.master,
        format: .caf48kFloat32, lanes: [.mic, .system], retention: .keepDays(30))
    }
  #else
    static var decoder: any AudioDecoder { WAVAudioDecoder() }

    static func makeAsset(layout: RecordingLayout, meetingID: UUID) throws -> AudioAsset {
      try FileManager.default.copyItem(
        at: Fixtures.url("audio/conversation-two-lane-6s.wav"), to: layout.master(.wav16kInt16))
      try FileManager.default.copyItem(
        at: Fixtures.url("audio/conversation-mic-6s.wav"), to: layout.sidecar(.mic))
      try FileManager.default.copyItem(
        at: Fixtures.url("audio/conversation-system-6s.wav"), to: layout.sidecar(.system))
      return AudioAsset(
        id: SampleData.uuid(70), meetingID: meetingID, url: layout.master(.wav16kInt16),
        format: .wav16kInt16, lanes: [.mic, .system],
        sidecars16k: [.mic: layout.sidecar(.mic), .system: layout.sidecar(.system)],
        retention: .keepDays(30))
    }
  #endif

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
    let vault = FakeDestination(
      root: directory.appendingPathComponent("vault", isDirectory: true))
    let dispatcher = FakeDeliveryDispatcher(store: store, destinations: [vault], now: { now })
    let pipeline = ProcessingPipeline(
      dependencies: PipelineDependencies(
        decoder: Self.decoder,
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
    // Placed the way the capture writer would: the master in the meeting
    // folder, so the pipeline's clips and mixdown land beside it. With
    // AVFoundation the master is the 48 kHz two-lane CAF the recording writer
    // produces and the real decoder resamples each lane; elsewhere the WAV
    // fixtures stand in.
    let layout = RecordingLayout(audioFolder: settings.audioFolder, meetingID: meeting.id)
    try layout.createDirectories()
    let asset = try Self.makeAsset(layout: layout, meetingID: meeting.id)

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
    #expect(receipt.rendererVersion == FakeDestination.rendererVersion)

    let json = try Data(contentsOf: vault.exportURL(meetingID: meeting.id))
    let export = try StenoJSON.decode(MeetingExport.self, from: json)
    #expect(export.schemaVersion == MeetingExport.currentSchemaVersion)
    #expect(export.meeting.state == .ready)
    #expect(export.segments.count == 12)
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
