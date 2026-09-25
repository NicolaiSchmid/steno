#if canImport(FluidAudio)
  import FluidAudio
  import Foundation
  import StenoCore

  /// Parakeet TDT through FluidAudio's `AsrManager`. Runs unpinned: the
  /// model's `Language` parameter only filters Latin against Cyrillic
  /// script, so German and English both pass and code switching comes out
  /// mixed, which is what Denglish needs. Tokens are joined into words,
  /// words into segments, segments tagged with a language.
  public actor ParakeetEngine: SpeechEngine {
    public nonisolated let id: String
    public nonisolated let supportedLanguages: Set<Locale.Language>

    private let engine: SpeechEngineID
    private let models: ModelStore
    private var manager: AsrManager?
    private let aggregator = TokenAggregator()
    private let segmenter = TranscriptSegmenter()
    private let tagger = LanguageTagger()

    /// `id` is one of the three Parakeet engines. The German fine-tune has
    /// the v3 layout and loads like v3 from its own asset directory.
    public init(id engine: SpeechEngineID, models: ModelStore) {
      self.engine = engine
      self.models = models
      id = engine.rawValue
      supportedLanguages = engine.supportedLanguages
    }

    /// Downloads the asset when needed, then compiles and loads the models
    /// from their directory. `loadLocal` never touches the network, so a
    /// corrupt install fails here instead of re-downloading behind our back.
    public func prepare() async throws {
      guard manager == nil else { return }
      try await models.ensureInstalled(engine.asset)
      let loaded = try AsrModels.loadLocal(
        from: models.directory(for: engine.asset),
        version: engine == .parakeetUltra ? .ultra : .v3, encoderPrecision: .int8)
      let manager = AsrManager(config: .default)
      try await manager.loadModels(loaded)
      self.manager = manager
    }

    public func transcribe(_ audio: AudioBuffer16k, hint: Locale.Language?) async throws
      -> [RawSegment]
    {
      try await prepare()
      guard let manager else { throw SpeechEngineError.notPrepared(id) }
      guard !audio.samples.isEmpty else { return [] }
      let layers = await manager.decoderLayerCount
      var state = try TdtDecoderState(decoderLayers: layers)
      let result = try await manager.transcribe(audio.samples, decoderState: &state, language: nil)
      let tokens = (result.tokenTimings ?? []).map {
        TimedWord(text: $0.token, start: $0.startTime, end: $0.endTime, confidence: $0.confidence)
      }
      var segments = segmenter.segments(fromWords: aggregator.words(from: tokens))
      let text = result.text.trimmingCharacters(in: .whitespacesAndNewlines)
      if segments.isEmpty, !text.isEmpty {
        segments = [RawSegment(start: 0, end: audio.duration, text: text)]
      }
      return tagger.tag(segments, hint: hint.map(LanguageTag.init))
    }
  }
#endif
