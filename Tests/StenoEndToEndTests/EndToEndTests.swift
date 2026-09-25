import Foundation
import StenoAdapters
import StenoCore
import StenoSpeech
import Testing

@testable import StenoAudio

/// The one real-pipeline test across modules. Core created it with fakes
/// everywhere; each module workstream's last step replaces its own fake with
/// the real type. Delivery runs through the real `DeliveryCoordinator` and
/// `ObsidianFolderDestination` into a temp vault; speaker suggestions come
/// from the real `CosineSpeakerMemory` over the store (the engine and the
/// diarizer stay fakes). No models, no network.
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

  static let berlin = TimeZone(identifier: "Europe/Berlin")!

  @Test func macCallFixtureLandsInVault() async throws {
    let directory = try Fixtures.temporaryDirectory("e2e")
    defer { try? FileManager.default.removeItem(at: directory) }
    let now = SampleData.updatedAt

    let store = try MeetingStore.inMemory()
    let settingsStore = SettingsStore(writer: store.writer)
    let vault = directory.appendingPathComponent("vault", isDirectory: true)
    try FileManager.default.createDirectory(at: vault, withIntermediateDirectories: true)
    var settings = Settings()
    settings.audioFolder = directory.appendingPathComponent("audio", isDirectory: true)
    settings.obsidian = ObsidianSettings(
      vaultPath: vault.path, peopleFolder: "People", includeAudio: true, taskTag: "task")
    try await settingsStore.save(settings)
    for person in SampleData.persons() { try await store.save(person) }

    let events = MeetingEventBus()
    let cleaner = PassthroughCleaner()
    let summarizer = FakeSummarizer()
    // The stored settings decide the destination, as in the app; the time
    // zone is pinned so the goldens hold on every machine.
    let dispatcher = DeliveryCoordinator(
      store: store, settings: settingsStore,
      destinations: { settings in
        settings.obsidian.map { [ObsidianFolderDestination(settings: $0, timeZone: Self.berlin)] }
          ?? []
      },
      now: { now })
    let pipeline = ProcessingPipeline(
      dependencies: PipelineDependencies(
        decoder: Self.decoder,
        speechEngine: FakeSpeechEngine(),
        diarizer: FakeDiarizer(),
        speakerMemory: CosineSpeakerMemory(store: store),
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
    #expect(deliveries.first?.destinationID == ObsidianFolderDestination.destinationID)
    #expect(deliveries.first?.status == .delivered)
    #expect(deliveries.first?.lastAttemptAt == now)
    let receipt = try #require(deliveries.first?.receipt)
    let folder = "Meetings/2026-09-24-produktstrategie"
    let slug = "2026-09-24-produktstrategie"
    // `audio.m4a` with the real decoder (AAC mixdown), `audio.wav` elsewhere.
    let mixdownFormat = Self.decoder.mixdownFormat
    let mixdown = layout.mixdown(mixdownFormat).lastPathComponent
    #expect(receipt.root == vault.path)
    #expect(receipt.folder == folder)
    #expect(receipt.rendererVersion == ArtifactRenderer.version)
    #expect(
      receipt.files.map(\.relativePath) == [
        "\(folder)/\(slug) - Tasks.md", "\(folder)/\(slug) - Transcript.md", "\(folder)/\(slug).md",
        "\(folder)/\(mixdown)", "\(folder)/meeting.json", "\(folder)/transcript.vtt",
        "People/Jérôme.md", "People/Nicolai.md",
      ])
    for file in receipt.files {
      let data = try Data(contentsOf: vault.appendingPathComponent(file.relativePath))
      #expect(file.sha256 == ContentHash.sha256(data), "\(file.relativePath)")
    }

    let json = try Data(contentsOf: vault.appendingPathComponent("\(folder)/meeting.json"))
    let export = try StenoJSON.decode(MeetingExport.self, from: json)
    #expect(json == (try StenoJSON.encode(export)), "meeting.json is the StenoJSON encoding")
    let current = try await store.export(meetingID: meeting.id)
    #expect(export.meeting == current.meeting)
    #expect(export.segments == current.segments)
    #expect(export.tasks == current.tasks)
    #expect(
      export.audio?.expiresAt == nil && current.audio?.expiresAt != nil,
      "delivery precedes the retention stage, which sets the expiry afterwards")
    #expect(export.schemaVersion == MeetingExport.currentSchemaVersion)
    #expect(export.meeting.state == .ready)
    #expect(export.segments.count == 12)
    #expect(export.speakers.map(\.clusterLabel) == ["Me", "Speaker 1", "Speaker 2"])
    // The fake diarizer's axis embeddings match the pre-enrolled people
    // through the real cosine memory: every "them" speaker is suggested.
    let them = export.speakers.filter { $0.clusterLabel != "Me" }
    #expect(them.count == 2)
    #expect(them.allSatisfy { $0.assignment.kind == .suggested }, "\(them.map(\.assignment))")
    #expect(
      Set(them.compactMap(\.personID)) == Set(SampleData.persons().map(\.id)),
      "each cluster is suggested to its own person")
    #expect(
      try Data(contentsOf: vault.appendingPathComponent("\(folder)/\(mixdown)"))
        == (try Data(contentsOf: layout.mixdown(mixdownFormat))),
      "the mixdown is copied byte for byte")
    #expect(!SummaryMarkdown.render(export).isEmpty)
    try Snapshot.assert(
      SummaryMarkdown.render(export), matches: "snapshots/e2e/mac-call-summary.md")
    for (file, golden) in [
      ("\(folder)/\(slug).md", "folder-note.md"),
      ("\(folder)/\(slug) - Transcript.md", "transcript.md"),
      ("\(folder)/\(slug) - Tasks.md", "tasks.md"),
      ("\(folder)/transcript.vtt", "transcript.vtt"),
      ("People/Jérôme.md", "person-jerome.md"),
      ("People/Nicolai.md", "person-nicolai.md"),
    ] {
      try Snapshot.assert(
        try Data(contentsOf: vault.appendingPathComponent(file)), matches: "snapshots/e2e/\(golden)"
      )
    }

    // Re-export through the one entry point overwrites Steno's files and
    // leaves the user's alone.
    let notes = vault.appendingPathComponent("\(folder)/notes.md")
    try Data("mine\n".utf8).write(to: notes)
    try await pipeline.redeliver(meetingID: meeting.id)
    let again = try #require(try await store.deliveries(meetingID: meeting.id).first?.receipt)
    #expect(again.folder == receipt.folder)
    #expect(again.files.map(\.relativePath) == receipt.files.map(\.relativePath))
    for (before, after) in zip(receipt.files, again.files)
    where !before.relativePath.hasSuffix("meeting.json") {
      #expect(before.sha256 == after.sha256, "\(before.relativePath) is byte-identical")
    }
    #expect(
      again.files[4].sha256 != receipt.files[4].sha256,
      "meeting.json changed: the retention stage set expiresAt after the first delivery")
    #expect(try String(contentsOf: notes, encoding: .utf8) == "mine\n")

    // Everything posted so far, read up to a sentinel so a failed run can
    // never hang the test.
    let sentinel = MeetingEvent.speakersNeedReview(meetingID: meeting.id, speakerIDs: [])
    await events.post(sentinel)
    var iterator = stream.makeAsyncIterator()
    var collected: [MeetingEvent] = []
    while let event = await iterator.next(), event != sentinel { collected.append(event) }
    try #require(collected.count == 12, "ten stage starts, one review request, one re-export")
    let stages = collected.compactMap { event -> PipelineStage? in
      if case .progress(_, let stage) = event { return stage }
      return nil
    }
    #expect(stages == PipelineStage.allCases + [.deliver])
    #expect(
      collected[8]
        == .speakersNeedReview(meetingID: meeting.id, speakerIDs: export.speakers.map(\.id)))
  }
}
