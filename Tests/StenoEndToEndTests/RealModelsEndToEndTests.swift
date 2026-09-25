import Foundation
import StenoCore
import StenoSpeech
import Testing

/// Speech step 8's opt-in acceptance: the pipeline over `two-speakers.wav`
/// with the real Parakeet v3 engine, the FluidAudio diarizer and cosine
/// speaker memory over the store, the way `steno process --engine
/// parakeet-v3` wires them. Set `STENO_MODEL_TESTS=1` (about 0.5 GB of
/// downloads, a Mac); `STENO_MODEL_TESTS_DIR` keeps the models between runs.
@Suite(.serialized) struct RealModelsEndToEndTests {
  static let environment = ProcessInfo.processInfo.environment
  static let enabled = environment["STENO_MODEL_TESTS"] == "1"
  static let skipMessage: Comment =
    "set STENO_MODEL_TESTS=1 to run the pipeline over two-speakers.wav with Parakeet v3 and the diarizer"

  #if canImport(FluidAudio) && canImport(WhisperKit)
    @Test(.enabled(if: enabled, skipMessage))
    func twoSpeakersFixtureRunsThroughTheRealEnginesToReady() async throws {
      let directory = try Fixtures.temporaryDirectory("e2e-models")
      defer { try? FileManager.default.removeItem(at: directory) }
      let modelsDirectory =
        Self.environment["STENO_MODEL_TESTS_DIR"].map {
          URL(fileURLWithPath: $0, isDirectory: true)
        }
        ?? directory.appendingPathComponent("models", isDirectory: true)
      let models = ModelStore(directory: modelsDirectory)

      let store = try MeetingStore.inMemory()
      let settingsStore = SettingsStore(writer: store.writer)
      var settings = Settings()
      settings.audioFolder = directory.appendingPathComponent("audio", isDirectory: true)
      try await settingsStore.save(settings)

      let pipeline = ProcessingPipeline(
        dependencies: PipelineDependencies(
          decoder: WAVAudioDecoder(),
          speechEngine: try makeSpeechEngine(.parakeetV3, models: models),
          diarizer: try makeDiarizer(models: models),
          speakerMemory: CosineSpeakerMemory(store: store),
          cleaner: PassthroughCleaner(),
          summarizer: FakeSummarizer(),
          dispatcher: FakeDeliveryDispatcher(store: store, destinations: []),
          store: store,
          settings: settingsStore,
          events: MeetingEventBus()))

      let source = Fixtures.url("speech/two-speakers.wav")
      let info = try WAVAudioDecoder.info(source)
      let duration = Double(info.frameCount) / Double(info.sampleRate)
      let meetingID = UUID()
      let layout = RecordingLayout(audioFolder: settings.audioFolder, meetingID: meetingID)
      try layout.createDirectories()
      try FileManager.default.copyItem(at: source, to: layout.master(.wav16kInt16))
      let now = Date()
      let meeting = Meeting(
        id: meetingID, title: "two-speakers", startedAt: now.addingTimeInterval(-duration),
        duration: duration, source: .macInPerson, state: .queued, createdAt: now, updatedAt: now)
      let asset = AudioAsset(
        id: UUID(), meetingID: meetingID, url: layout.master(.wav16kInt16), format: .wav16kInt16,
        lanes: [.mixed], retention: .keepForever)

      try await pipeline.enqueue(meeting, asset: asset)
      await pipeline.waitUntilIdle()

      let stored = try #require(try await store.meeting(id: meetingID))
      #expect(stored.state == .ready, "\(stored.state)")
      let export = try await store.export(meetingID: meetingID)
      try #require(!export.segments.isEmpty)
      let text = export.segments.map(\.text).joined(separator: " ")
      let reference = try String(
        contentsOf: Fixtures.url("speech/two-speakers.ref.txt"), encoding: .utf8)
      let wer = WordErrorRate.compute(reference: reference, hypothesis: text)
      print(
        "[model-tests] pipeline two-speakers.wav: WER \(String(format: "%.1f", wer * 100)) %: \(text)"
      )
      #expect(wer < 0.5, "\(text)")
      let speakers = try await store.speakers(meetingID: meetingID)
      print("[model-tests] speakers: \(speakers.map { "\($0.clusterLabel) \($0.assignment)" })")
      #expect(speakers.count == 2, "\(speakers.map(\.clusterLabel))")
      #expect(speakers.allSatisfy { $0.embedding != nil && $0.sampleClipRange != nil })
      #expect(models.isInstalled(.parakeetV3) && models.isInstalled(.offlineDiarizer))
    }
  #endif
}
