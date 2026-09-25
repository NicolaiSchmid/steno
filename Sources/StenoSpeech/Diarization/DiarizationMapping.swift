import Foundation
import StenoCore

/// Turns the diarizer's turns and chunks into `StenoCore.DiarizationResult`:
/// one `SpeakerCluster` per speaker label that has a turn, labelled
/// "Speaker n" in order of first speech, with merged ranges, the normalised
/// cluster embedding and the sample clip. Pure, so the mapping is tested
/// without models.
///
/// The turns are the framework's final word on who spoke when (frame voting
/// and post-processing over the whole recording); the chunks are the
/// per-window embeddings that fed clustering, and a chunk's span is the
/// local speaker's extent inside a ten-second window, not a turn. A cluster
/// label that appears only in chunks lost every frame vote, so it gets no
/// speaker: promoting it would invent a speaker whose range overlaps the real
/// ones and whose sample clip plays somebody else (seen with the real model
/// on a two-voice `say` fixture, where it produced a third speaker spanning
/// 4.00 to 9.35 s over the two real ones).
enum DiarizationMapping {
  /// `chunks` arrive without a quality (the framework reports none per
  /// chunk); each takes the quality of the turn it overlaps most first.
  static func result(turns: [SpeakerTurn], chunks raw: [ClusterChunk]) -> DiarizationResult {
    let chunks = assigningQuality(to: raw, from: turns)
    let sortedTurns = turns.filter { $0.duration > 0 }.sorted { $0.start < $1.start }
    var order: [String] = []
    var turnsByLabel: [String: [SpeakerTurn]] = [:]
    for turn in sortedTurns {
      if turnsByLabel[turn.speakerLabel] == nil { order.append(turn.speakerLabel) }
      turnsByLabel[turn.speakerLabel, default: []].append(turn)
    }

    let clusters = order.enumerated().map { index, label -> SpeakerCluster in
      let ownTurns = turnsByLabel[label] ?? []
      let ownChunks = chunks.filter { $0.speakerLabel == label }
      let ranges = merged(ownTurns.map { $0.start...max($0.start, $0.end) })
      // Without chunks the turns carry the quality (and no embedding).
      let scored =
        ownChunks.isEmpty
        ? ownTurns.map {
          ClusterChunk(
            speakerLabel: label, start: $0.start, end: $0.end, embedding: [], quality: $0.quality)
        }
        : ownChunks
      let choice = SampleClipPicker.pick(ranges: ranges, chunks: scored)
      return SpeakerCluster(
        label: "Speaker \(index + 1)",
        ranges: ranges,
        embedding: ClusterEmbedding.embedding(of: ownChunks),
        clusterConfidence: choice.clusterConfidence,
        sampleClipRange: choice.range)
    }
    return DiarizationResult(clusters: clusters)
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

  /// Each chunk takes the quality of the turn of its speaker it overlaps
  /// most, 1 when none overlaps.
  static func assigningQuality(to chunks: [ClusterChunk], from turns: [SpeakerTurn])
    -> [ClusterChunk]
  {
    chunks.map { chunk in
      var chunk = chunk
      let best = turns.filter { $0.speakerLabel == chunk.speakerLabel }
        .max { overlap($0, chunk) < overlap($1, chunk) }
      chunk.quality = best.map { overlap($0, chunk) > 0 ? $0.quality : 1 } ?? 1
      return chunk
    }
  }

  private static func overlap(_ turn: SpeakerTurn, _ chunk: ClusterChunk) -> TimeInterval {
    max(0, min(turn.end, chunk.end) - max(turn.start, chunk.start))
  }
}
