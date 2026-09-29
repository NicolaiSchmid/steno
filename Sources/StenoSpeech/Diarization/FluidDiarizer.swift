import Foundation
import StenoCore

/// Steno's diarizer settings: the clustering knobs handed to FluidAudio's
/// offline pipeline and whether the refinement pass runs after it.
/// Everything else in the pipeline stays at FluidAudio's community defaults;
/// the sample clip length is core's contract (`SampleClipPicker.targetSeconds`),
/// not a knob.
public struct FluidDiarizerConfig: Sendable, Equatable {
  /// Passed to `OfflineDiarizerConfig.clustering.threshold`: a Euclidean cut
  /// on unit embeddings, larger merges more. 0.8 instead of the community
  /// 0.6 since the calibration on real calls
  /// (`.plans/2026-09-29-speaker-calibration.md`): the same substantive
  /// clusters on every call, fewer fragments for the refinement pass.
  public var clusteringThreshold: Double
  public var minSpeakers: Int?
  public var maxSpeakers: Int?
  /// Whether `ClusterRefinement` runs after the mapping; off, the clusters
  /// come back as the mapping produced them (what the sweep tool reports
  /// with `--no-refinement`).
  public var refinesClusters: Bool

  public init(
    clusteringThreshold: Double = 0.8, minSpeakers: Int? = nil, maxSpeakers: Int? = nil,
    refinesClusters: Bool = true
  ) {
    self.clusteringThreshold = clusteringThreshold
    self.minSpeakers = minSpeakers
    self.maxSpeakers = maxSpeakers
    self.refinesClusters = refinesClusters
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
  /// instance to the generic executor. The box owns the manager; the actor
  /// holds two boxes (the full pipeline and the one-speaker instance) and
  /// the models they share. What makes `@unchecked Sendable` true is not
  /// the actor (it is re-entrant across the awaited `process`) but
  /// `FluidDiarizer.diarize`, which runs its calls one after another, so no
  /// two `process` calls are ever in flight on one box even when the
  /// pipeline processes two meetings at once.
  private final class OfflineDiarizerBox: @unchecked Sendable {
    private let manager: OfflineDiarizerManager

    init(config: OfflineDiarizerConfig, models: OfflineDiarizerModels) {
      manager = OfflineDiarizerManager(config: config)
      manager.initialize(models: models)
    }

    /// Runs the pipeline, copies the framework result field for field into
    /// the module's own turns and chunks and hands them to
    /// `DiarizationMapping`, so no FluidAudio type appears in a signature
    /// and every decision about them lives there.
    func process(_ samples: [Float]) async throws -> DiarizationResult {
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
      return DiarizationMapping.result(turns: turns, chunks: chunks)
    }
  }

  /// A box capped at one speaker over a slice of speech: whatever it hears
  /// is one voice, and the cluster embedding of that voice is the slice's
  /// embedding. `noSpeechDetected` and audio under one embedding window are
  /// nil.
  private struct SingleSpeakerEmbedder: SliceEmbedder {
    let box: OfflineDiarizerBox

    func embedding(of audio: AudioBuffer16k) async throws -> Embedding? {
      guard audio.duration >= FluidDiarizer.minimumAudioSeconds else { return nil }
      do {
        return try await box.process(audio.samples).clusters.first?.embedding
      } catch OfflineDiarizationError.noSpeechDetected {
        return nil
      }
    }
  }

  /// The pyannote community-1 offline pipeline through FluidAudio's
  /// `OfflineDiarizerManager`, wrapped in an actor because the manager is
  /// not `Sendable`. Chunk embeddings are exposed so the cluster embedding
  /// is a normalised mean of unit vectors, not the VBx centroid. That mean
  /// is what `ClusterRefinement` starts from; after the pass each
  /// substantive cluster carries the embedding of its own concatenated
  /// speech instead.
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
    /// One load for every caller, however many arrive while it runs.
    private var loadingModels: Task<OfflineDiarizerModels, any Error>?
    /// The most recent `diarize` call; the next one waits for it, so the
    /// boxes see one `process` at a time (the actor itself is re-entrant).
    private var lastDiarization: Task<Void, Never>?

    init(models: ModelStore, config: FluidDiarizerConfig = .default) {
      self.models = models
      self.config = config
    }

    func prepare() async throws {
      _ = try await loaded()
    }

    /// The full pipeline at `config`, built on first use. The check repeats
    /// after the await because a concurrent `prepare` may have built it.
    private func loaded() async throws -> OfflineDiarizerBox {
      if let manager { return manager }
      let built = try await makePipeline(
        threshold: config.clusteringThreshold, minSpeakers: config.minSpeakers,
        maxSpeakers: config.maxSpeakers)
      if let manager { return manager }
      manager = built
      return built
    }

    /// The pipeline behind the slice embedder: the same models, the
    /// clustering forced to one speaker (`numSpeakers` is what FluidAudio
    /// resolves min = max = 1 to), the threshold at the community default.
    private func singleSpeaker() async throws -> OfflineDiarizerBox {
      if let single { return single }
      let built = try await makePipeline(threshold: nil, minSpeakers: 1, maxSpeakers: 1)
      if let single { return single }
      single = built
      return built
    }

    /// FluidAudio's defaults with the clustering knobs set and the chunk
    /// embeddings exposed, over the shared models.
    private func makePipeline(threshold: Double?, minSpeakers: Int?, maxSpeakers: Int?)
      async throws -> OfflineDiarizerBox
    {
      var fluidConfig = OfflineDiarizerConfig.default
      if let threshold { fluidConfig.clustering.threshold = threshold }
      fluidConfig.clustering.minSpeakers = minSpeakers
      fluidConfig.clustering.maxSpeakers = maxSpeakers
      fluidConfig.exposeChunkEmbeddings = true
      return OfflineDiarizerBox(config: fluidConfig, models: try await loadModels())
    }

    /// Downloads the asset when needed, then loads the models once from the
    /// framework root (`load(from:)` takes the parent of the repository
    /// folder). A failed load is forgotten, so the next call retries.
    private func loadModels() async throws -> OfflineDiarizerModels {
      if let loadingModels { return try await loadingModels.value }
      let store = models
      let task = Task {
        try await store.ensureInstalled(.offlineDiarizer)
        return try await OfflineDiarizerModels.load(
          from: store.frameworkRoot(for: .offlineDiarizer))
      }
      loadingModels = task
      do {
        return try await task.value
      } catch {
        loadingModels = nil
        throw error
      }
    }

    /// Silence, room noise or a lane nobody spoke on is not a failed meeting:
    /// FluidAudio throws `noSpeechDetected` when no embedding survives, and
    /// that becomes a result with no speakers, like audio under
    /// `minimumAudioSeconds`. With `config.refinesClusters`, the mapped
    /// clusters then go through `ClusterRefinement` over the one-speaker
    /// instance, a single cluster included, so every stored embedding is of
    /// the same kind. Calls run one after another: the pipeline processes
    /// meetings concurrently, and the boxes take one `process` at a time.
    func diarize(_ audio: AudioBuffer16k) async throws -> DiarizationResult {
      guard audio.duration >= Self.minimumAudioSeconds else {
        return DiarizationResult(clusters: [])
      }
      let previous = lastDiarization
      let run = Task {
        await previous?.value
        return try await self.diarizeNow(audio)
      }
      lastDiarization = Task { _ = try? await run.value }
      return try await run.value
    }

    private func diarizeNow(_ audio: AudioBuffer16k) async throws -> DiarizationResult {
      let manager = try await loaded()
      let mapped: DiarizationResult
      do {
        mapped = try await manager.process(audio.samples)
      } catch OfflineDiarizationError.noSpeechDetected {
        return DiarizationResult(clusters: [])
      }
      guard config.refinesClusters, !mapped.clusters.isEmpty else { return mapped }
      let embedder = SingleSpeakerEmbedder(box: try await singleSpeaker())
      let refined = try await ClusterRefinement.refine(
        mapped.clusters, in: audio, embedder: embedder)
      return DiarizationResult(clusters: refined)
    }
  }
#endif
