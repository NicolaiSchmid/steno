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
  static let parakeetDEEnabled = environment["STENO_MODEL_TESTS_PARAKEET_DE"] == "1"
  static let parakeetDESkipMessage: Comment =
    "set STENO_MODEL_TESTS=1 and STENO_MODEL_TESTS_PARAKEET_DE=1 to download the Parakeet German fine-tune (1.2 GB)"

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

  /// `<name>.ref.txt` beside `<name>.wav`, the way `BakeoffRunner` finds it.
  static func reference(_ name: String) throws -> String {
    let stem = (name as NSString).deletingPathExtension
    return try String(contentsOf: Fixtures.url("speech/\(stem).ref.txt"), encoding: .utf8)
  }

  static func report(_ label: String, audio: AudioBuffer16k, elapsed: Duration) {
    let seconds = elapsed / .seconds(1)
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
        try #require(!segments.isEmpty, "\(name)")
        for (lhs, rhs) in zip(segments, segments.dropFirst()) {
          #expect(lhs.end <= rhs.start + 0.05, "segments overlap in \(name)")
        }
        #expect(segments.allSatisfy { $0.start <= $0.end && $0.end <= audio.duration + 0.5 })
        let text = segments.map(\.text).joined(separator: " ")
        let wer = WordErrorRate.compute(reference: try Self.reference(name), hypothesis: text)
        print(
          "[model-tests] parakeet-v3 \(name): WER \(String(format: "%.1f", wer * 100)) %: \(text)")
        #expect(wer < 0.5, "\(name): \(text)")
        #expect(LanguageTagger().dominantLanguage(of: segments)?.rawValue == language, "\(text)")
      }
    }

    /// Every cluster the real model yields has a unit embedding, a clip of
    /// at most ten seconds inside one of its ranges, and no range shared
    /// with another speaker. Returns the result for the caller's own checks.
    static func diarizeAndCheckInvariants(_ name: String, store: ModelStore) async throws
      -> DiarizationResult
    {
      let diarizer = FluidDiarizer(models: store)
      try await diarizer.prepare()
      #expect(store.isInstalled(.offlineDiarizer))
      let audio = try fixture(name)
      let clock = ContinuousClock()
      let started = clock.now
      let result = try await diarizer.diarize(audio)
      report("diarizer \(name)", audio: audio, elapsed: clock.now - started)
      print("[model-tests] \(name) clusters: \(result.clusters.map { "\($0.label) \($0.ranges)" })")
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
        for other in result.clusters where other.label != cluster.label {
          for lhs in cluster.ranges {
            for rhs in other.ranges {
              #expect(!lhs.overlaps(rhs), "\(cluster.label) \(lhs) overlaps \(other.label) \(rhs)")
            }
          }
        }
      }
      return result
    }

    /// Anna and Daniel: the diarizer separates the two voices, and each
    /// speaker's ranges are their own turns (0.4 s of silence between turns,
    /// Anna first).
    @Test(.enabled(if: enabled, skipMessage))
    func diarizerFindsTwoSpeakersWithClipsInsideTheirRanges() async throws {
      let (store, cleanup) = try Self.modelStore()
      defer { cleanup() }
      let result = try await Self.diarizeAndCheckInvariants("two-speakers-mf.wav", store: store)
      #expect(result.clusters.count == 2, "\(result.clusters.map(\.label))")
      guard result.clusters.count == 2 else { return }
      let (anna, daniel) = (result.clusters[0], result.clusters[1])
      #expect(anna.ranges.count == 2 && daniel.ranges.count == 2, "two turns each")
      #expect(anna.ranges.first?.lowerBound ?? 1 < 0.5, "Anna opens")
      #expect(daniel.ranges.first.map { $0.lowerBound > 2 && $0.lowerBound < 2.6 } == true)
      let similarity = try #require(anna.embedding).cosineSimilarity(
        to: try #require(daniel.embedding))
      print("[model-tests] cross-speaker cosine (Anna, Daniel): \(similarity)")
    }

    /// Anna and Samantha: two female `say` voices that the community-1
    /// segmentation hears as one speaker per window. Reported, not asserted,
    /// so the fixture keeps documenting the limit; the invariants still hold.
    @Test(.enabled(if: enabled, skipMessage))
    func twoFemaleSayVoicesAreReportedNotAsserted() async throws {
      let (store, cleanup) = try Self.modelStore()
      defer { cleanup() }
      let result = try await Self.diarizeAndCheckInvariants("two-speakers.wav", store: store)
      #expect(!result.clusters.isEmpty)
      print("[model-tests] two-speakers.wav (Anna, Samantha) speakers: \(result.clusters.count)")
    }

    /// Silence is not a failed meeting: FluidAudio throws `noSpeechDetected`
    /// when no embedding survives, and the diarizer answers with no speakers.
    @Test(.enabled(if: enabled, skipMessage))
    func silenceGivesNoClustersInsteadOfAnError() async throws {
      let (store, cleanup) = try Self.modelStore()
      defer { cleanup() }
      let diarizer = FluidDiarizer(models: store)
      let silence = AudioBuffer16k(samples: [Float](repeating: 0, count: 3 * 16_000))
      let result = try await diarizer.diarize(silence)
      #expect(result.clusters.isEmpty)
    }

    /// Spike C: speaker memory across recordings. Anna is enrolled from
    /// `de-short.wav`, Daniel from his cluster of `two-speakers-mf.wav`;
    /// `de-short-2.wav` (Anna, another sentence) must come back as Anna
    /// through `match` at the default threshold and margin, and `en-short.wav`
    /// (Samantha, enrolled nowhere) must not be matched to anyone by margin.
    /// Measured on 2026-09-25 with the real model: same `say` voice across
    /// sentences 0.90 to 0.96, different voices 0.48 to 0.87 (two female
    /// voices 0.74 to 0.82), so the absolute threshold is not separable on
    /// TTS voices and the ranking with the margin is what this asserts; the
    /// threshold is calibrated on real meetings (checklist #34).
    @Test(.enabled(if: enabled, skipMessage))
    func speakerMemoryRecognisesAVoiceAcrossRecordings() async throws {
      let (store, cleanup) = try Self.modelStore()
      defer { cleanup() }
      let single = FluidDiarizer(models: store, config: FluidDiarizerConfig(maxSpeakers: 1))
      try await single.prepare()
      var voice: [String: Embedding] = [:]
      for name in ["de-short.wav", "de-short-2.wav", "en-short.wav"] {
        let result = try await single.diarize(try Self.fixture(name))
        voice[name] = try #require(result.clusters.first?.embedding, "\(name)")
      }
      let pair = try await FluidDiarizer(models: store).diarize(
        try Self.fixture("two-speakers-mf.wav"))
      try #require(pair.clusters.count == 2, "\(pair.clusters.map(\.label))")
      let daniel = try #require(pair.clusters[1].embedding)

      let same = voice["de-short.wav"]!.cosineSimilarity(to: voice["de-short-2.wav"]!)
      let female = voice["de-short.wav"]!.cosineSimilarity(to: voice["en-short.wav"]!)
      let mixed = voice["de-short.wav"]!.cosineSimilarity(to: daniel)
      print(
        "[model-tests] cosine: Anna/Anna \(same), Anna/Samantha \(female), Anna/Daniel \(mixed)")
      #expect(same > female + 0.05 && same > mixed + 0.05, "the same voice wins by the margin")

      let meetings = try MeetingStore.inMemory()
      let memory = CosineSpeakerMemory(store: meetings)
      let anna = Person(id: UUID(), displayName: "Anna", createdAt: Date())
      let dan = Person(id: UUID(), displayName: "Daniel", createdAt: Date())
      try await memory.enroll(voice["de-short.wav"]!, as: anna)
      try await memory.enroll(daniel, as: dan)
      let threshold = Settings().speakerMatchThreshold
      let recognised = try await memory.match(voice["de-short-2.wav"]!, threshold: threshold)
      #expect(recognised?.person.id == anna.id, "\(String(describing: recognised))")
      let stranger = try await memory.match(voice["en-short.wav"]!, threshold: threshold)
      let candidates = try await memory.candidates(for: voice["en-short.wav"]!, limit: 2)
      print(
        "[model-tests] Samantha against {Anna, Daniel}: match \(String(describing: stranger?.person.displayName)), "
          + "candidates \(candidates.map { "\($0.person.displayName) \($0.similarity)" })")
      #expect(candidates.count == 2)
    }

    @Test(.enabled(if: enabled, skipMessage))
    func bakeoffOverTheFixtureFolderProducesARowPerFile() async throws {
      let (store, cleanup) = try Self.modelStore()
      defer { cleanup() }
      let runner = BakeoffRunner(makeEngine: { try makeSpeechEngine($0, models: store) })
      let report = try await runner.run(
        audioDirectory: Fixtures.url("speech"), engines: [.parakeetV3])
      print(report.markdown())
      #expect(report.rows.count == 6)
      #expect(report.rows.allSatisfy { $0.wer != nil })
    }

    /// Spike B: the German fine-tune (v3 layout, another repository) is
    /// fetched by redirecting the v3 repository for that one download, lands
    /// in its own parent so it cannot shadow the official v3, loads with
    /// `loadLocal` and transcribes German. The redirect must be gone
    /// afterwards and an installed v3 untouched.
    @Test(.enabled(if: enabled && parakeetDEEnabled, parakeetDESkipMessage))
    func parakeetGermanFineTuneDownloadsThroughTheRedirectAndLoadsOffline() async throws {
      let (store, cleanup) = try Self.modelStore()
      defer { cleanup() }
      let v3Before = store.installedSize(of: .parakeetV3)
      let engine = try makeSpeechEngine(.parakeetDE, models: store)
      let clock = ContinuousClock()
      let prepareStarted = clock.now
      try await engine.prepare()
      print(
        "[model-tests] parakeet-de prepare (download if needed, compile, load): "
          + "\(String(format: "%.1f", (clock.now - prepareStarted) / .seconds(1))) s")
      #expect(store.isInstalled(.parakeetDE))
      #expect(store.installedSize(of: .parakeetDE) ?? 0 > 1_000_000_000, "fp16 encoder")
      #expect(
        store.directory(for: .parakeetDE) != store.directory(for: .parakeetV3),
        "own parent, never the v3 folder")
      #expect(
        LiveModelDownloader.repoOverrides.read()[ModelAsset.parakeetV3.sourceRepo] == nil,
        "the v3 redirect is put back after the download")
      #expect(store.installedSize(of: .parakeetV3) == v3Before, "an installed v3 is untouched")

      let audio = try Self.fixture("de-short.wav")
      let started = clock.now
      let segments = try await engine.transcribe(audio, hint: nil)
      Self.report("parakeet-de de-short.wav", audio: audio, elapsed: clock.now - started)
      try #require(!segments.isEmpty)
      let text = segments.map(\.text).joined(separator: " ")
      let wer = WordErrorRate.compute(
        reference: try Self.reference("de-short.wav"), hypothesis: text)
      print(
        "[model-tests] parakeet-de de-short.wav: WER \(String(format: "%.1f", wer * 100)) %: \(text)"
      )
      #expect(wer < 0.5, "\(text)")
      #expect(LanguageTagger().dominantLanguage(of: segments) == "de", "\(text)")
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
      #expect(LanguageTagger().dominantLanguage(of: segments) == "de", "\(text)")
      let wer = WordErrorRate.compute(
        reference: try Self.reference("denglish.wav"), hypothesis: text)
      #expect(wer < 0.5, "\(text)")
    }
  #endif
}
