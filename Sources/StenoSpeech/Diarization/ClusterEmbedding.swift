import Foundation
import StenoCore

/// One embedding window of the offline diarizer after clustering: which
/// speaker it was assigned to, when, its 256-dim WeSpeaker vector and the
/// quality of the speech it covers. The framework result is mapped into
/// these first so every mapping test can build them by hand.
struct ClusterChunk: Sendable, Equatable {
  var speakerLabel: String
  var start: TimeInterval
  var end: TimeInterval
  var embedding: [Float]
  var quality: Float

  var duration: TimeInterval { max(0, end - start) }
  var range: ClosedRange<TimeInterval> { start...max(start, end) }
}

/// One "who spoke when" turn from the diarizer, before ranges are merged.
struct SpeakerTurn: Sendable, Equatable {
  var speakerLabel: String
  var start: TimeInterval
  var end: TimeInterval
  var quality: Float

  var duration: TimeInterval { max(0, end - start) }
}

/// The cluster embedding a `Speaker` row stores: chunk vectors averaged
/// with their duration as weight, then L2-normalised. Not the VBx centroid,
/// which is an un-normalised mean and would bias cosine against stored
/// people by cluster purity.
enum ClusterEmbedding {
  static func embedding(of chunks: [ClusterChunk]) -> Embedding? {
    let usable = chunks.filter { $0.embedding.count == Embedding.dimension && $0.duration > 0 }
    guard !usable.isEmpty else { return nil }
    let mean = Embeddings.weightedMean(
      usable.map(\.embedding), weights: usable.map { Float($0.duration) })
    guard mean.count == Embedding.dimension else { return nil }
    return Embedding(mean)
  }
}
