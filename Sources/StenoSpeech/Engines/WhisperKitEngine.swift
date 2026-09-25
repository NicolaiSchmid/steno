#if canImport(WhisperKit)
  import Foundation
  import StenoCore

  // Scoped imports: WhisperKit also exports `WordTiming`, and
  // `StenoCore.WordTiming` would resolve to the `StenoCore` version enum, so
  // only the five names this file needs are brought in.
  import class WhisperKit.TranscriptionResult
  import class WhisperKit.WhisperKit
  import class WhisperKit.WhisperKitConfig
  import struct WhisperKit.DecodingOptions
  import struct WhisperKit.ModelComputeOptions

  /// `WhisperKit` is a non-Sendable class with async methods: its instance
  /// can neither be returned into the actor nor be sent back out for a call.
  /// The box owns it and is created and used only by the actor. What makes
  /// `@unchecked Sendable` true is not the actor (it is re-entrant across the
  /// awaited `transcribe`) but the caller: core's pipeline transcribes one
  /// lane at a time, so no two calls are ever in flight on one engine. A
  /// second concurrent caller would need an in-flight guard here (follow-up).
  private final class WhisperKitBox: @unchecked Sendable {
    private let kit: WhisperKit

    init(_ config: WhisperKitConfig) async throws {
      kit = try await WhisperKit(config)
    }

    func transcribe(_ samples: [Float], options: DecodingOptions) async throws
      -> [TranscriptionResult]
    {
      try await kit.transcribe(audioArray: samples, decodeOptions: options)
    }

    func detectLanguage(_ samples: [Float]) async throws -> String {
      try await kit.detectLangauge(audioArray: samples).language
    }
  }

  /// Whisper large-v3 turbo through WhisperKit. Whisper can be pinned per
  /// call, not per segment, and unpinned it flips whole windows on Denglish
  /// and sometimes translates, so the meeting language is decided first: the
  /// pipeline's `hint` when there is one, else a majority vote of
  /// `detectLangauge` over the three most energetic 30 s windows. Segments
  /// the model considers silence (`noSpeechProb` above the threshold) are
  /// dropped.
  actor WhisperKitEngine: SpeechEngine {
    nonisolated let id = SpeechEngineID.whisperKitLargeV3Turbo.rawValue
    nonisolated let supportedLanguages = SpeechEngineID.whisperKitLargeV3Turbo.supportedLanguages

    private let asset = ModelAsset.whisperLargeV3Turbo
    private let models: ModelStore
    private var whisper: WhisperKitBox?
    private let mapping = WhisperMapping()

    init(models: ModelStore) {
      self.models = models
    }

    func prepare() async throws {
      _ = try await loaded()
    }

    /// Downloads the asset when needed, then loads it. WhisperKit is told
    /// the variant (`ModelAsset.modelFolder`), its `downloadBase` (the
    /// asset's framework root) and the model folder itself; `download:
    /// false` keeps the framework off the network.
    private func loaded() async throws -> WhisperKitBox {
      if let whisper { return whisper }
      try await models.ensureInstalled(asset)
      let config = WhisperKitConfig(
        model: asset.modelFolder,
        downloadBase: models.frameworkRoot(for: asset),
        modelFolder: models.directory(for: asset).path,
        computeOptions: ModelComputeOptions(
          melCompute: .cpuAndGPU, audioEncoderCompute: .cpuAndNeuralEngine,
          textDecoderCompute: .cpuAndNeuralEngine),
        verbose: false,
        logLevel: .none,
        prewarm: false,
        load: true,
        download: false)
      let whisper = try await WhisperKitBox(config)
      self.whisper = whisper
      return whisper
    }

    func transcribe(_ audio: AudioBuffer16k, hint: Locale.Language?) async throws -> [RawSegment] {
      let whisper = try await loaded()
      guard !audio.samples.isEmpty else { return [] }
      let pinned = try await WhisperMapping.pinnedLanguage(samples: audio.samples, hint: hint) {
        try await whisper.detectLanguage(Array($0))
      }
      var options = DecodingOptions(
        task: .transcribe, language: pinned, usePrefillPrompt: true, detectLanguage: false,
        skipSpecialTokens: true, wordTimestamps: true, chunkingStrategy: .vad)
      options.noSpeechThreshold = mapping.noSpeechThreshold
      let results = try await whisper.transcribe(audio.samples, options: options)
      return mapping.segments(from: Self.whisperSegments(results), pinned: pinned)
    }

    /// The framework's segments flattened into the module's own; every
    /// decision about them lives in `WhisperMapping`.
    static func whisperSegments(_ results: [TranscriptionResult]) -> [WhisperSegment] {
      results.flatMap { result in
        result.segments.map { segment in
          WhisperSegment(
            start: TimeInterval(segment.start), end: TimeInterval(segment.end),
            text: segment.text, noSpeechProb: segment.noSpeechProb,
            words: (segment.words ?? []).map {
              TimedWord(
                text: $0.word, start: TimeInterval($0.start), end: TimeInterval($0.end),
                confidence: $0.probability)
            })
        }
      }
    }
  }
#endif
