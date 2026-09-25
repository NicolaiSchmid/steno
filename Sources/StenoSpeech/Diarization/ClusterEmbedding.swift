import Foundation
import StenoCore

/// The cluster embedding a `Speaker` row stores: each chunk vector brought to
/// unit length, summed with its duration as weight, then L2-normalised
/// (normalising removes the scale, so dividing by the total weight first
/// would change nothing). FluidAudio hands `embedding256` over as the raw
/// WeSpeaker output and normalises only inside its own clustering, so
/// without the first step a few high-norm windows (crosstalk, music,
/// clipping) would steer the mean. Not the VBx centroid, which is an
/// un-normalised mean and would bias cosine against stored people by
/// cluster purity.
enum ClusterEmbedding {
  static func embedding(of chunks: [ClusterChunk]) -> Embedding? {
    var sum = [Float](repeating: 0, count: Embedding.dimension)
    var usable = false
    for chunk in chunks where chunk.embedding.count == Embedding.dimension && chunk.duration > 0 {
      let unit = Embedding(chunk.embedding).normalized().values
      let weight = Float(chunk.duration)
      for index in sum.indices { sum[index] += unit[index] * weight }
      usable = true
    }
    return usable ? Embedding(sum).normalized() : nil
  }
}
