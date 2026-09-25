import Foundation

#if canImport(Accelerate)
  import Accelerate
#endif

/// Vector helpers for speaker embeddings. vDSP where Accelerate exists,
/// plain loops elsewhere; both paths give the same answers.
public enum Embeddings {
  /// The same direction with unit length; the zero vector stays zero.
  public static func normalised(_ vector: [Float]) -> [Float] {
    let magnitude = norm(vector)
    guard magnitude > 0 else { return vector }
    #if canImport(Accelerate)
      var scale = 1 / magnitude
      var result = [Float](repeating: 0, count: vector.count)
      vDSP_vsmul(vector, 1, &scale, &result, 1, vDSP_Length(vector.count))
      return result
    #else
      return vector.map { $0 / magnitude }
    #endif
  }

  /// Cosine similarity in `-1...1`. Both inputs should be unit length; the
  /// result is divided by the norms anyway so a slightly denormalised vector
  /// does not skew a threshold. Zero when the lengths differ or a vector is
  /// zero.
  public static func cosine(_ lhs: [Float], _ rhs: [Float]) -> Float {
    guard lhs.count == rhs.count, !lhs.isEmpty else { return 0 }
    let denominator = norm(lhs) * norm(rhs)
    guard denominator > 0 else { return 0 }
    return max(-1, min(1, dot(lhs, rhs) / denominator))
  }

  public static func dot(_ lhs: [Float], _ rhs: [Float]) -> Float {
    guard lhs.count == rhs.count else { return 0 }
    #if canImport(Accelerate)
      var result: Float = 0
      vDSP_dotpr(lhs, 1, rhs, 1, &result, vDSP_Length(lhs.count))
      return result
    #else
      return zip(lhs, rhs).reduce(0) { $0 + $1.0 * $1.1 }
    #endif
  }

  public static func norm(_ vector: [Float]) -> Float {
    dot(vector, vector).squareRoot()
  }

  /// Weighted mean of vectors of equal length, normalised. Empty input or
  /// a zero total weight gives an empty vector.
  public static func weightedMean(_ vectors: [[Float]], weights: [Float]) -> [Float] {
    guard let first = vectors.first, vectors.count == weights.count else { return [] }
    let total = weights.reduce(0, +)
    guard total > 0 else { return [] }
    var sum = [Float](repeating: 0, count: first.count)
    for (vector, weight) in zip(vectors, weights) where vector.count == first.count {
      for index in sum.indices { sum[index] += vector[index] * weight }
    }
    return normalised(sum.map { $0 / total })
  }
}
