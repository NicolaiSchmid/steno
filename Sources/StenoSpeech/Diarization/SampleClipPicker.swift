import Foundation

/// Chooses the clip a user hears when naming a speaker, and the cluster
/// confidence stored beside it. The clip is the longest contiguous range of
/// the cluster, capped to `targetSeconds` and centred on the highest-quality
/// chunk inside it. Confidence is the duration-weighted mean chunk quality,
/// halved when the longest range is under `minimumSeconds`: a speaker who
/// never held the floor for three seconds is hard to name and easy to
/// confuse.
struct SampleClipPicker: Sendable {
  struct Choice: Equatable {
    var range: ClosedRange<TimeInterval>?
    var clusterConfidence: Float
  }

  static func pick(
    ranges: [ClosedRange<TimeInterval>], chunks: [ClusterChunk],
    targetSeconds: TimeInterval = 10, minimumSeconds: TimeInterval = 3
  ) -> Choice {
    guard let longest = ranges.max(by: { length($0) < length($1) }) else {
      return Choice(range: nil, clusterConfidence: 0)
    }
    var confidence = meanQuality(chunks)
    if length(longest) < minimumSeconds { confidence /= 2 }

    let clipLength = min(targetSeconds, length(longest))
    let centre: TimeInterval
    if let best = chunks.filter({ overlaps($0.range, longest) }).max(by: {
      if $0.quality != $1.quality { return $0.quality < $1.quality }
      return $0.duration < $1.duration
    }) {
      centre = (max(best.start, longest.lowerBound) + min(best.end, longest.upperBound)) / 2
    } else {
      centre = (longest.lowerBound + longest.upperBound) / 2
    }
    var lower = centre - clipLength / 2
    lower = max(longest.lowerBound, min(lower, longest.upperBound - clipLength))
    let upper = min(longest.upperBound, lower + clipLength)
    return Choice(range: lower...max(lower, upper), clusterConfidence: confidence)
  }

  static func meanQuality(_ chunks: [ClusterChunk]) -> Float {
    let total = chunks.reduce(0.0) { $0 + $1.duration }
    guard total > 0 else { return 0 }
    let weighted = chunks.reduce(0.0) { $0 + Double($1.quality) * $1.duration }
    return Float(max(0, min(1, weighted / total)))
  }

  static func length(_ range: ClosedRange<TimeInterval>) -> TimeInterval {
    range.upperBound - range.lowerBound
  }

  static func overlaps(_ lhs: ClosedRange<TimeInterval>, _ rhs: ClosedRange<TimeInterval>) -> Bool {
    lhs.lowerBound < rhs.upperBound && rhs.lowerBound < lhs.upperBound
  }
}
