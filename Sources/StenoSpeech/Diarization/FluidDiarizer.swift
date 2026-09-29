import Foundation
import StenoCore

/// Knobs of the offline diarizer that Steno exposes. Everything else stays
/// at FluidAudio's community defaults; the sample clip length is core's
/// contract (`SampleClipPicker.targetSeconds`), not a knob.
public struct FluidDiarizerConfig: Sendable, Equatable {
  /// Passed to `OfflineDiarizerConfig.clustering.threshold`: a Euclidean cut
  /// on unit embeddings, larger merges more. 0.8 instead of the community
  /// 0.6 since the calibration on real calls
  /// (`.plans/2026-09-29-speaker-calibration.md`): the same substantive
  /// clusters on every call, fewer fragments for the refinement pass.
  public var clusteringThreshold: Double
  public var minSpeakers: Int?
  public var maxSpeakers: Int?
  /// Whether `ClusterRefinement` runs after the mapping. Off only for the
  /// sweep tool, which reports the raw clusters.
  public var refines: Bool

  public init(
    clusteringThreshold: Double = 0.8, minSpeakers: Int? = nil, maxSpeakers: Int? = nil,
    refines: Bool = true
  ) {
    self.clusteringThreshold = clusteringThreshold
    self.minSpeakers = minSpeakers
    self.maxSpeakers = maxSpeakers
    self.refines = refines
  }

  public static let `default` = FluidDiarizerConfig()
}

#if canImport(FluidAudio)
  // Scoped imports: FluidAudio also exports `Diarizer` and
  // `DiarizationResult`, and `StenoCore.X` would resolve to the `StenoCore`
  // version enum, so only the four names this file needs are brought in.
  import class FluidAudio.OfflineDiarizerManager
  import enum FluidAudio.OfflineDiarizationError
  import struct FluidAudio.OfflineDiarizerConfig
  import struct FluidAudio.OfflineDiarizerModels

  /// `OfflineDiarizerManager` is a non-Sendable class whose `process` is an
  /// async method: calling it with an actor-owned instance would send that
  /// instance to the generic executor. The box owns the manager and is the
  /// only thing the actor holds. What makes `@unchecked Sendable` true is
  /// not the actor (it is re-entrant across the awaited `process`) but the
  /// caller: core's `Diarize` stage runs one lane per meeting, one meeting
  /// at a time, so no two calls are ever in flight on one diarizer. A second
  /// concurrent caller would need an in-flight guard here (follow-up).
  private final class OfflineDiarizerBox: @unchecked Sendable {
    private let manager: OfflineDiarizerManager

    init(config: OfflineDiarizerConfig, models: OfflineDiarizerModels) {
      manager = OfflineDiarizerManager(config: config)
      manager.initialize(models: models)
    }

    /// Runs the pipeline and copies the framework result field for field
    /// into the module's own turns and chunks, so no FluidAudio type appears
    /// in a signature; every decision about them lives in `DiarizationMapping`.
    func process(_ samples: [Float]) async throws -> (turns: [SpeakerTurn], chunks: [ClusterChunk])
    {
      let result = try await manager.process(audio: samples)
      let turns = result.segments.map {
        SpeakerTurn(
          speakerLabel: $0.speakerId, start: TimeInterval($0.startTimeSeconds),
          end: TimeInterval($0.endTimeSeconds), quality: $0.qualityScore)
      }
      let chunks = (result.chunkEmbeddings ?? []).map {
        ClusterChunk(
          speakerLabel: $0.speakerId, start: $0.startTimeSeconds, end: $0.endTimeSeconds,
          embedding: $0.embedding256)
      }
      return (turns, chunks)
    }
  }

  /// The pipeline capped at one speaker over a slice of speech: whatever it
  /// hears is one voice, and the cluster embedding of that voice is the
  /// slice's embedding. `noSpeechDetected` and audio under one embedding
  /// window are nil.
  private struct SingleSpeakerEmbedder: SliceEmbedder {
    let box: OfflineDiarizerBox

    func embedding(of audio: AudioBuffer16k) async throws -> Embedding? {
      guard audio.duration >= FluidDiarizer.minimumAudioSeconds else { return nil }
      do {
        let (turns, chunks) = try await box.process(audio.samples)
        return DiarizationMapping.result(turns: turns, chunks: chunks).clusters.first?.embedding
      } catch OfflineDiarizationError.noSpeechDetected {
        return nil
      }
    }
  }

  /// The pyannote community-1 offline pipeline through FluidAudio's
  /// `OfflineDiarizerManager`, wrapped in an actor because the manager is
  /// not `Sendable`. Chunk embeddings are exposed so the cluster embedding
  /// is a normalised mean of unit vectors, not the VBx centroid.
  actor FluidDiarizer: Diarizer {
    /// Audio shorter than one embedding window has nothing to cluster;
    /// FluidAudio reports it as `noSpeechDetected`, so it is answered here
    /// without loading the models.
    static let minimumAudioSeconds: TimeInterval = 1

    let config: FluidDiarizerConfig
    private let models: ModelStore
    private var manager: OfflineDiarizerBox?
    /// The one-speaker instance behind `ClusterRefinement`, sharing the
    /// loaded models with `manager`.
    private var single: OfflineDiarizerBox?
    private var loadedModels: OfflineDiarizerModels?

    init(models: ModelStore, config: FluidDiarizerConfig = .default) {
      self.models = models
      self.config = config
    }

    func prepare() async throws {
      _ = try await loaded()
    }

    /// The full pipeline at `config`, built on first use.
    private func loaded() async throws -> OfflineDiarizerBox {
      if let manager { return manager }
      var fluidConfig = OfflineDiarizerConfig.default
      fluidConfig.clustering.threshold = config.clusteringThreshold
      fluidConfig.clustering.minSpeakers = config.minSpeakers
      fluidConfig.clustering.maxSpeakers = config.maxSpeakers
      fluidConfig.exposeChunkEmbeddings = true
      let manager = OfflineDiarizerBox(config: fluidConfig, models: try await loadModels())
      self.manager = manager
      return manager
    }

    /// The slice embedder: the same models, the clustering forced to one
    /// speaker (`numSpeakers` is what FluidAudio resolves min = max = 1 to).
    private func singleSpeaker() async throws -> OfflineDiarizerBox {
      if let single { return single }
      var fluidConfig = OfflineDiarizerConfig.default
      fluidConfig.clustering.minSpeakers = 1
      fluidConfig.clustering.maxSpeakers = 1
      fluidConfig.exposeChunkEmbeddings = true
      let single = OfflineDiarizerBox(config: fluidConfig, models: try await loadModels())
      self.single = single
      return single
    }

    /// Downloads the asset when needed, then loads the models once from the
    /// framework root (`load(from:)` takes the parent of the repository
    /// folder).
    private func loadModels() async throws -> OfflineDiarizerModels {
      if let loadedModels { return loadedModels }
      try await models.ensureInstalled(.offlineDiarizer)
      let loaded = try await OfflineDiarizerModels.load(
        from: models.frameworkRoot(for: .offlineDiarizer))
      loadedModels = loaded
      return loaded
    }

    /// Silence, room noise or a lane nobody spoke on is not a failed meeting:
    /// FluidAudio throws `noSpeechDetected` when no embedding survives, and
    /// that becomes a result with no speakers, like audio under
    /// `minimumAudioSeconds`.
    func diarize(_ audio: AudioBuffer16k) async throws -> DiarizationResult {
      guard audio.duration >= Self.minimumAudioSeconds else {
        return DiarizationResult(clusters: [])
      }
      let manager = try await loaded()
      let mapped: DiarizationResult
      do {
        let (turns, chunks) = try await manager.process(audio.samples)
        mapped = DiarizationMapping.result(turns: turns, chunks: chunks)
      } catch OfflineDiarizationError.noSpeechDetected {
        return DiarizationResult(clusters: [])
      }
      guard config.refines, mapped.clusters.count > 1 else { return mapped }
      let embedder = SingleSpeakerEmbedder(box: try await singleSpeaker())
      let refined = try await ClusterRefinement.refine(
        mapped.clusters, in: audio, embedder: embedder)
      return DiarizationResult(clusters: refined)
    }
  }
#endif
