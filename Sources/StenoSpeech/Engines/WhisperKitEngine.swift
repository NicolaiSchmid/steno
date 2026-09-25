#if canImport(WhisperKit)
  import Foundation
  import StenoCore
  import WhisperKit

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
    public static let defaultVariant = "openai_whisper-large-v3-v20240930_turbo"
    public static let noSpeechThreshold: Float = 0.6

    public nonisolated let id = SpeechEngineID.whisperKitLargeV3Turbo.rawValue
    public nonisolated let supportedLanguages = SpeechEngineID.whisperKitLargeV3Turbo
      .supportedLanguages

    private let variant: String
    private let models: ModelStore
    private var whisper: WhisperKitBox?
    private let tagger = LanguageTagger()

    public init(variant: String = WhisperKitEngine.defaultVariant, models: ModelStore) {
      self.variant = variant
      self.models = models
    }

    /// `downloadBase` is the `whisperkit/` folder: the asset directory minus
    /// WhisperKit's own `models/<repo>/<variant>` suffix.
    static func downloadBase(for assetDirectory: URL) -> URL {
      assetDirectory
        .deletingLastPathComponent()  // variant
        .deletingLastPathComponent()  // whisperkit-coreml
        .deletingLastPathComponent()  // argmaxinc
        .deletingLastPathComponent()  // models
    }

    public func prepare() async throws {
      guard whisper == nil else { return }
      try await models.ensureInstalled(.whisperLargeV3Turbo)
      let directory = models.directory(for: .whisperLargeV3Turbo)
      let config = WhisperKitConfig(
        model: variant,
        downloadBase: Self.downloadBase(for: directory),
        modelFolder: directory.path,
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
      let pinned = try await decideLanguage(audio, hint: hint, whisper: whisper)
      var options = DecodingOptions(
        task: .transcribe, language: pinned, usePrefillPrompt: true, detectLanguage: false,
        skipSpecialTokens: true, wordTimestamps: true, chunkingStrategy: .vad)
      options.noSpeechThreshold = Self.noSpeechThreshold
      let results = try await whisper.transcribe(audio.samples, options: options)
      let segments = Self.segments(
        from: results, language: pinned.map { LanguageTag(rawValue: $0) })
      return tagger.tag(segments, hint: pinned.map { LanguageTag(rawValue: $0) })
    }

    /// Whisper's two-letter code for the hint, or the detected majority.
    private func decideLanguage(
      _ audio: AudioBuffer16k, hint: Locale.Language?, whisper: WhisperKitBox
    )
      async throws -> String?
    {
      if let hint, let code = Self.whisperCode(for: hint) { return code }
      var votes: [String] = []
      for window in WhisperWindowRanking.topWindows(samples: audio.samples) {
        votes.append(try await whisper.detectLanguage(Array(audio.samples[window.samples])))
      }
      return WhisperWindowRanking.majority(votes)
    }

    /// `en-US` becomes `en`; a language Whisper does not know gives nil so the
    /// model is not forced into a code it has no token for.
    static func whisperCode(for language: Locale.Language) -> String? {
      guard let code = LanguageTag(language).rawValue.split(separator: "-").first else {
        return nil
      }
      let tag = LanguageTag(rawValue: String(code))
      return WhisperLanguages.all.contains(tag) ? tag.rawValue : nil
    }

    static func segments(from results: [TranscriptionResult], language: LanguageTag?)
      -> [RawSegment]
    {
      var segments: [RawSegment] = []
      for result in results {
        for segment in result.segments {
          guard segment.noSpeechProb <= noSpeechThreshold else { continue }
          let text = segment.text.trimmingCharacters(in: .whitespacesAndNewlines)
          guard !text.isEmpty else { continue }
          let words = segment.words?.compactMap { word -> CoreWordTiming? in
            let trimmed = word.word.trimmingCharacters(in: .whitespacesAndNewlines)
            guard !trimmed.isEmpty else { return nil }
            return CoreWordTiming(
              word: trimmed, start: TimeInterval(word.start), end: TimeInterval(word.end))
          }
          segments.append(
            RawSegment(
              start: TimeInterval(segment.start),
              end: TimeInterval(max(segment.start, segment.end)),
              text: text, language: language, wordTimings: words?.isEmpty == false ? words : nil))
        }
      }
      return segments.sorted { $0.start < $1.start }
    }
  }
#endif
