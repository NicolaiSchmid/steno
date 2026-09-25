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
  /// The box owns it, is created and used only by the actor, and the actor
  /// serialises every call, which is what makes `@unchecked Sendable` true.
  final class WhisperKitBox: @unchecked Sendable {
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
  public actor WhisperKitEngine: SpeechEngine {
    public static let noSpeechThreshold: Float = WhisperMapping.noSpeechThreshold

    public nonisolated let id = SpeechEngineID.whisperKitLargeV3Turbo.rawValue
    public nonisolated let supportedLanguages = SpeechEngineID.whisperKitLargeV3Turbo
      .supportedLanguages

    private let asset = ModelAsset.whisperLargeV3Turbo
    private let models: ModelStore
    private var whisper: WhisperKitBox?
    private let mapping = WhisperMapping()

    public init(models: ModelStore) {
      self.models = models
    }

    /// WhisperKit is told the variant (`ModelAsset.modelFolder`), its
    /// `downloadBase` (the asset's framework root) and the model folder
    /// itself; `download: false` keeps the framework off the network.
    public func prepare() async throws {
      guard whisper == nil else { return }
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
      whisper = try await WhisperKitBox(config)
    }

    public func transcribe(_ audio: AudioBuffer16k, hint: Locale.Language?) async throws
      -> [RawSegment]
    {
      try await prepare()
      guard let whisper else { throw SpeechEngineError.notPrepared(id) }
      guard !audio.samples.isEmpty else { return [] }
      let pinned = try await WhisperMapping.pinnedLanguage(samples: audio.samples, hint: hint) {
        try await whisper.detectLanguage(Array($0))
      }
      var options = DecodingOptions(
        task: .transcribe, language: pinned, usePrefillPrompt: true, detectLanguage: false,
        skipSpecialTokens: true, wordTimestamps: true, chunkingStrategy: .vad)
      options.noSpeechThreshold = Self.noSpeechThreshold
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
