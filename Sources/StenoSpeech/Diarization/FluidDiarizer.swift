import Foundation
import StenoCore

/// Knobs of the offline diarizer that Steno exposes. Everything else stays
/// at FluidAudio's community defaults.
public struct FluidDiarizerConfig: Sendable, Equatable {
  /// Passed to `OfflineDiarizerConfig.clustering.threshold`.
  public var clusteringThreshold: Double
  public var minSpeakers: Int?
  public var maxSpeakers: Int?
  public var sampleClipSeconds: TimeInterval
  public var minimumClipSeconds: TimeInterval

  public init(
    clusteringThreshold: Double = 0.6, minSpeakers: Int? = nil, maxSpeakers: Int? = nil,
    sampleClipSeconds: TimeInterval = 10, minimumClipSeconds: TimeInterval = 3
  ) {
    self.clusteringThreshold = clusteringThreshold
    self.minSpeakers = minSpeakers
    self.maxSpeakers = maxSpeakers
    self.sampleClipSeconds = sampleClipSeconds
    self.minimumClipSeconds = minimumClipSeconds
  }

  public static let `default` = FluidDiarizerConfig()
}

#if canImport(FluidAudio)
  import FluidAudio

  /// The pyannote community-1 offline pipeline through FluidAudio's
  /// `OfflineDiarizerManager`, wrapped in an actor because the manager is
  /// not `Sendable`. Chunk embeddings are exposed so the cluster embedding
  /// is a normalised mean of unit vectors, not the VBx centroid.
  public actor FluidDiarizer: Diarizer {
    public let config: FluidDiarizerConfig
    private let models: ModelStore
    private var manager: OfflineDiarizerManager?

    public init(models: ModelStore, config: FluidDiarizerConfig = .default) {
      self.models = models
      self.config = config
    }

    public func prepare() async throws {
      guard manager == nil else { return }
      try await models.ensureInstalled(.offlineDiarizer)
      // `load(from:)` takes the parent of the repository folder.
      let parent = models.directory(for: .offlineDiarizer).deletingLastPathComponent()
      let loaded = try await OfflineDiarizerModels.load(from: parent)
      var fluidConfig = OfflineDiarizerConfig.default
      fluidConfig.clustering.threshold = config.clusteringThreshold
      fluidConfig.clustering.minSpeakers = config.minSpeakers
      fluidConfig.clustering.maxSpeakers = config.maxSpeakers
      fluidConfig.exposeChunkEmbeddings = true
      let manager = OfflineDiarizerManager(config: fluidConfig)
      manager.initialize(models: loaded)
      self.manager = manager
    }

    public func diarize(_ audio: AudioBuffer16k) async throws -> CoreDiarizationResult {
      try await prepare()
      guard let manager else { throw SpeechEngineError.notPrepared("fluid-diarizer") }
      guard !audio.samples.isEmpty else { return CoreDiarizationResult(clusters: []) }
      let result = try await manager.process(audio: audio.samples)
      let turns = result.segments.map {
        SpeakerTurn(
          speakerLabel: $0.speakerId, start: TimeInterval($0.startTimeSeconds),
          end: TimeInterval($0.endTimeSeconds), quality: $0.qualityScore)
      }
      let windows = (result.chunkEmbeddings ?? []).map {
        (
          label: $0.speakerId, start: $0.startTimeSeconds, end: $0.endTimeSeconds,
          embedding: $0.embedding256
        )
      }
      return DiarizationMapping.result(
        turns: turns, chunks: DiarizationMapping.chunks(windows, turns: turns),
        targetSeconds: config.sampleClipSeconds, minimumSeconds: config.minimumClipSeconds)
    }
  }
#endif

/// The FluidAudio offline diarizer over one `ModelStore`; throws where the
/// framework is missing so callers without it (Linux) fail at wiring time.
public func makeDiarizer(models: ModelStore, config: FluidDiarizerConfig = .default) throws
  -> any Diarizer
{
  #if canImport(FluidAudio)
    return FluidDiarizer(models: models, config: config)
  #else
    throw SpeechEngineError.unavailable(.parakeetV3)
  #endif
}
