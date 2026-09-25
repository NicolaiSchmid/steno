import Foundation
import StenoCore

/// StenoCore's `DiarizationResult` and `Diarizer`: FluidAudio exports types
/// of the same names, and `StenoCore.DiarizationResult` would resolve to the
/// `StenoCore` version enum, so files that import FluidAudio use these.
/// Public because they appear in `FluidDiarizer`'s public signatures.
public typealias CoreDiarizationResult = DiarizationResult
public typealias CoreDiarizer = Diarizer

/// Turns the diarizer's turns and chunks into `StenoCore.DiarizationResult`:
/// one `SpeakerCluster` per speaker label, labelled "Speaker n" in order of
/// first speech, with merged ranges, the normalised cluster embedding and
/// the sample clip. Pure, so the mapping is tested without models.
enum DiarizationMapping {
  static func result(
    turns: [SpeakerTurn], chunks: [ClusterChunk],
    targetSeconds: TimeInterval = 10, minimumSeconds: TimeInterval = 3
  ) -> CoreDiarizationResult {
    let sortedTurns = turns.filter { $0.duration > 0 }.sorted { $0.start < $1.start }
    var order: [String] = []
    var turnsByLabel: [String: [SpeakerTurn]] = [:]
    for turn in sortedTurns {
      if turnsByLabel[turn.speakerLabel] == nil { order.append(turn.speakerLabel) }
      turnsByLabel[turn.speakerLabel, default: []].append(turn)
    }
    // Speakers that only have chunks (no turn survived post-processing)
    // still get a cluster, after the ones that spoke.
    for chunk in chunks.sorted(by: { $0.start < $1.start })
    where turnsByLabel[chunk.speakerLabel] == nil && !order.contains(chunk.speakerLabel) {
      order.append(chunk.speakerLabel)
    }

    let clusters = order.enumerated().map { index, label -> SpeakerCluster in
      let ownTurns = turnsByLabel[label] ?? []
      let ownChunks = chunks.filter { $0.speakerLabel == label }
      var ranges = merged(ownTurns.map { $0.start...max($0.start, $0.end) })
      if ranges.isEmpty { ranges = merged(ownChunks.map(\.range)) }
      let choice = SampleClipPicker.pick(
        ranges: ranges, chunks: ownChunks, targetSeconds: targetSeconds,
        minimumSeconds: minimumSeconds)
      var confidence = choice.clusterConfidence
      if ownChunks.isEmpty {
        // No chunk quality to average: fall back to the turns' own scores.
        let total = ownTurns.reduce(0.0) { $0 + $1.duration }
        if total > 0 {
          confidence = Float(ownTurns.reduce(0.0) { $0 + Double($1.quality) * $1.duration } / total)
          if let longest = ranges.max(by: { length($0) < length($1) }),
            length(longest) < minimumSeconds
          {
            confidence /= 2
          }
        }
      }
      return SpeakerCluster(
        label: "Speaker \(index + 1)",
        ranges: ranges,
        embedding: ClusterEmbedding.embedding(of: ownChunks),
        clusterConfidence: max(0, min(1, confidence)),
        sampleClipRange: choice.range)
    }
    return CoreDiarizationResult(clusters: clusters)
  }

  /// Sorted ranges with touching or overlapping ones joined.
  static func merged(_ ranges: [ClosedRange<TimeInterval>]) -> [ClosedRange<TimeInterval>] {
    var result: [ClosedRange<TimeInterval>] = []
    for range in ranges.sorted(by: { $0.lowerBound < $1.lowerBound }) {
      if let last = result.last, range.lowerBound <= last.upperBound {
        result[result.count - 1] = last.lowerBound...max(last.upperBound, range.upperBound)
      } else {
        result.append(range)
      }
    }
    return result
  }

  static func length(_ range: ClosedRange<TimeInterval>) -> TimeInterval {
    range.upperBound - range.lowerBound
  }

  /// Chunk quality is not reported per chunk by the framework: each chunk
  /// takes the quality of the turn it overlaps most, 1 when none overlaps.
  static func chunks(
    _ windows: [(label: String, start: TimeInterval, end: TimeInterval, embedding: [Float])],
    turns: [SpeakerTurn]
  ) -> [ClusterChunk] {
    windows.map { window in
      let own = turns.filter { $0.speakerLabel == window.label }
      let best = own.max { lhs, rhs in
        overlap(lhs, window.start, window.end) < overlap(rhs, window.start, window.end)
      }
      let quality = best.flatMap { overlap($0, window.start, window.end) > 0 ? $0.quality : nil }
      return ClusterChunk(
        speakerLabel: window.label, start: window.start, end: window.end,
        embedding: window.embedding, quality: quality ?? 1)
    }
  }

  private static func overlap(_ turn: SpeakerTurn, _ start: TimeInterval, _ end: TimeInterval)
    -> TimeInterval
  {
    max(0, min(turn.end, end) - max(turn.start, start))
  }
}
