import Foundation
import StenoCore
import Testing

@testable import StenoSpeech

/// Downloads real models and runs the engines and the diarizer over the
/// `say` fixtures. Opt-in: set `STENO_MODEL_TESTS=1` (Parakeet v3 and the
/// diarizer, about 0.5 GB) and `STENO_MODEL_TESTS_WHISPER=1` for WhisperKit
/// (another 1.6 GB). Models land in `STENO_MODEL_TESTS_DIR` when set, else
/// in a temporary directory that is removed afterwards. Real-time factors
/// are printed, never asserted.
@Suite(.serialized) struct ModelIntegrationTests {
  static let environment = ProcessInfo.processInfo.environment
  static let enabled = environment["STENO_MODEL_TESTS"] == "1"
  static let whisperEnabled = environment["STENO_MODEL_TESTS_WHISPER"] == "1"
  static let skipMessage: Comment =
    "set STENO_MODEL_TESTS=1 to download Parakeet v3 and the diarizer and run the engines"
  static let whisperSkipMessage: Comment =
    "set STENO_MODEL_TESTS=1 and STENO_MODEL_TESTS_WHISPER=1 to download WhisperKit large-v3 turbo (1.6 GB)"

  static func modelStore() throws -> (ModelStore, cleanup: () -> Void) {
    if let path = environment["STENO_MODEL_TESTS_DIR"] {
      return (ModelStore(directory: URL(fileURLWithPath: path, isDirectory: true)), {})
    }
    let directory = try Fixtures.temporaryDirectory("models")
    return (
      ModelStore(directory: directory), { try? FileManager.default.removeItem(at: directory) }
    )
  }

  static func fixture(_ name: String) throws -> AudioBuffer16k {
    let url = Fixtures.url("speech/\(name)")
    try #require(
      FileManager.default.fileExists(atPath: url.path),
      "Tests/Fixtures/speech/\(name) is missing; see Tests/Fixtures/README.md")
    return try WAVAudioDecoder.read(url)
  }

  static func reference(_ name: String) throws -> String {
    try String(contentsOf: Fixtures.url("speech/\(name).ref.txt"), encoding: .utf8)
  }

  static func report(_ label: String, audio: AudioBuffer16k, elapsed: Duration) {
    let seconds = BakeoffRunner.seconds(elapsed)
    print(
      "[model-tests] \(label): \(String(format: "%.2f", audio.duration)) s audio in "
        + "\(String(format: "%.2f", seconds)) s, RTF \(String(format: "%.1f", audio.duration / max(seconds, 1e-9)))"
    )
  }

  #if canImport(FluidAudio) && canImport(WhisperKit)
    @Test(.enabled(if: enabled, skipMessage))
    func parakeetTranscribesGermanAndEnglish() async throws {
      let (store, cleanup) = try Self.modelStore()
      defer { cleanup() }
      let engine = try makeSpeechEngine(.parakeetV3, models: store)
      try await engine.prepare()
      #expect(store.isInstalled(.parakeetV3))
      #expect(store.installedSize(of: .parakeetV3) ?? 0 > 300_000_000)

      for (name, language) in [("de-short.wav", "de"), ("en-short.wav", "en")] {
        let audio = try Self.fixture(name)
        let clock = ContinuousClock()
        let started = clock.now
        let segments = try await engine.transcribe(audio, hint: nil)
        Self.report("parakeet-v3 \(name)", audio: audio, elapsed: clock.now - started)
        try #require(!segments.isEmpty, name)
        for (lhs, rhs) in zip(segments, segments.dropFirst()) {
          #expect(lhs.end <= rhs.start + 0.05, "segments overlap in \(name)")
        }
        #expect(segments.allSatisfy { $0.start <= $0.end && $0.end <= audio.duration + 0.5 })
        let text = segments.map(\.text).joined(separator: " ")
        let wer = WordErrorRate.compute(reference: try Self.reference(name), hypothesis: text)
        print(
          "[model-tests] parakeet-v3 \(name): WER \(String(format: "%.1f", wer * 100)) %: \(text)")
        #expect(wer < 0.5, "\(name): \(text)")
        #expect(LanguageTagger().dominantLanguage(of: segments)?.rawValue == language, text)
      }
    }

    @Test(.enabled(if: enabled, skipMessage))
    func diarizerFindsTwoSpeakersWithClipsInsideTheirRanges() async throws {
      let (store, cleanup) = try Self.modelStore()
      defer { cleanup() }
      let diarizer = FluidDiarizer(models: store)
      try await diarizer.prepare()
      #expect(store.isInstalled(.offlineDiarizer))
      let audio = try Self.fixture("two-speakers.wav")
      let clock = ContinuousClock()
      let started = clock.now
      let result = try await diarizer.diarize(audio)
      Self.report("diarizer two-speakers.wav", audio: audio, elapsed: clock.now - started)
      print("[model-tests] clusters: \(result.clusters.map { "\($0.label) \($0.ranges)" })")
      #expect(result.clusters.count == 2, "\(result.clusters.map(\.label))")
      for cluster in result.clusters {
        let embedding = try #require(cluster.embedding)
        #expect(embedding.values.count == Embedding.dimension)
        #expect(abs(embedding.magnitude - 1) < 1e-3)
        let clip = try #require(cluster.sampleClipRange)
        #expect(clip.upperBound - clip.lowerBound <= 10)
        #expect(
          cluster.ranges.contains {
            $0.lowerBound <= clip.lowerBound + 1e-6 && clip.upperBound <= $0.upperBound + 1e-6
          }, "\(cluster.label) clip \(clip) outside \(cluster.ranges)")
      }
      if result.clusters.count == 2, let a = result.clusters[0].embedding,
        let b = result.clusters[1].embedding
      {
        let similarity = Embeddings.cosine(a.values, b.values)
        print("[model-tests] cross-speaker cosine: \(similarity)")
        #expect(similarity < 0.6, "two `say` voices should not match at the default threshold")
      }
    }

    /// Spike C: the same voice across two recordings scores above the match
    /// threshold, two different voices below it.
    @Test(.enabled(if: enabled, skipMessage))
    func sameVoiceAcrossRecordingsScoresAboveThreshold() async throws {
      let (store, cleanup) = try Self.modelStore()
      defer { cleanup() }
      let diarizer = FluidDiarizer(models: store, config: FluidDiarizerConfig(maxSpeakers: 1))
      try await diarizer.prepare()
      var embeddings: [String: Embedding] = [:]
      for name in ["de-short.wav", "de-short-2.wav", "en-short.wav"] {
        let result = try await diarizer.diarize(try Self.fixture(name))
        embeddings[name] = try #require(result.clusters.first?.embedding, name)
      }
      let same = Embeddings.cosine(
        embeddings["de-short.wav"]!.values, embeddings["de-short-2.wav"]!.values)
      let different = Embeddings.cosine(
        embeddings["de-short.wav"]!.values, embeddings["en-short.wav"]!.values)
      print("[model-tests] same voice \(same), different voices \(different)")
      #expect(same > 0.6)
      #expect(different < 0.5)
    }

    @Test(.enabled(if: enabled, skipMessage))
    func bakeoffOverTheFixtureFolderProducesFiveRows() async throws {
      let (store, cleanup) = try Self.modelStore()
      defer { cleanup() }
      let runner = BakeoffRunner(engineProvider: { try makeSpeechEngine($0, models: store) })
      let report = try await runner.run(
        audioDirectory: Fixtures.url("speech"), engines: [.parakeetV3])
      print(report.markdown())
      #expect(report.rows.count == 5)
      #expect(report.rows.allSatisfy { $0.wer != nil })
    }

    @Test(.enabled(if: enabled && whisperEnabled, whisperSkipMessage))
    func whisperKitPinnedToTheHintKeepsOneLanguage() async throws {
      let (store, cleanup) = try Self.modelStore()
      defer { cleanup() }
      let engine = try makeSpeechEngine(.whisperKitLargeV3Turbo, models: store)
      try await engine.prepare()
      let audio = try Self.fixture("denglish.wav")
      let clock = ContinuousClock()
      let started = clock.now
      let segments = try await engine.transcribe(audio, hint: LanguageTag("de").language)
      Self.report("whisperkit denglish.wav", audio: audio, elapsed: clock.now - started)
      try #require(!segments.isEmpty)
      let text = segments.map(\.text).joined(separator: " ")
      print("[model-tests] whisperkit denglish.wav: \(text)")
      #expect(LanguageTagger().dominantLanguage(of: segments) == "de", text)
      let wer = WordErrorRate.compute(
        reference: try Self.reference("denglish.wav"), hypothesis: text)
      #expect(wer < 0.5, text)
    }
  #else
    @Test(
      .enabled(
        if: false, "STENO_MODEL_TESTS need FluidAudio and WhisperKit, which only build on macOS"))
    func modelTestsNeedTheFrameworks() {}
  #endif
}
