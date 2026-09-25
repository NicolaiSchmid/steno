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

    private let variant: ParakeetVariant
    private let models: ModelStore
    private var manager: AsrManager?
    private let aggregator = TokenAggregator()
    private let segmenter = TranscriptSegmenter()
    private let tagger = LanguageTagger()

    public init(variant: ParakeetVariant, models: ModelStore) {
      self.variant = variant
      self.models = models
      id = variant.id
      supportedLanguages = Set(variant.supportedLanguageTags.map(\.language))
    }

    /// Downloads the asset when needed, then compiles and loads the models
    /// from their directory. `loadLocal` never touches the network, so a
    /// corrupt install fails here instead of re-downloading behind our back.
    public func prepare() async throws {
      guard manager == nil else { return }
      let directory: URL
      switch variant {
      case .custom(let url, _):
        directory = url
      case .v3, .ultra:
        let asset = variant.asset ?? .parakeetV3
        try await models.ensureInstalled(asset)
        directory = models.directory(for: asset)
      }
      let version: AsrModelVersion = variant == .ultra ? .ultra : .v3
      let loaded = try AsrModels.loadLocal(
        from: directory, version: version, encoderPrecision: .int8)
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
        TimedToken(text: $0.token, start: $0.startTime, end: $0.endTime, confidence: $0.confidence)
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
