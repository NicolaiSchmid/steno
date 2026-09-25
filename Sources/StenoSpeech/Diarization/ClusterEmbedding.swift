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

/// The cluster embedding a `Speaker` row stores: chunk vectors summed with
/// their duration as weight, then L2-normalised (normalising removes the
/// scale, so dividing by the total weight first would change nothing). Not
/// the VBx centroid, which is an un-normalised mean and would bias cosine
/// against stored people by cluster purity.
enum ClusterEmbedding {
  static func embedding(of chunks: [ClusterChunk]) -> Embedding? {
    var sum = [Float](repeating: 0, count: Embedding.dimension)
    var usable = false
    for chunk in chunks where chunk.embedding.count == Embedding.dimension && chunk.duration > 0 {
      let weight = Float(chunk.duration)
      for index in sum.indices { sum[index] += chunk.embedding[index] * weight }
      usable = true
    }
    return usable ? Embedding(sum).normalized() : nil
  }
}
