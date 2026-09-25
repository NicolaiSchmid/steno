import Foundation
import StenoCore
import Testing

@testable import StenoSpeech

private func vector(_ axis: Int, scale: Float = 1) -> [Float] {
  var values = [Float](repeating: 0, count: Embedding.dimension)
  values[axis] = scale
  return values
}

private func chunk(
  _ label: String, _ start: TimeInterval, _ end: TimeInterval, axis: Int, quality: Float = 0.9,
  scale: Float = 1
) -> ClusterChunk {
  ClusterChunk(
    speakerLabel: label, start: start, end: end, embedding: vector(axis, scale: scale),
    quality: quality)
}

@Suite struct ClusterEmbeddingTests {
  @Test func meanIsDurationWeightedAndUnitLength() throws {
    let chunks = [
      chunk("S1", 0, 3, axis: 0, scale: 5),  // not unit length on purpose
      chunk("S1", 3, 4, axis: 1),
    ]
    let embedding = try #require(ClusterEmbedding.embedding(of: chunks))
    #expect(embedding.values.count == Embedding.dimension)
    #expect(abs(embedding.magnitude - 1) < 1e-5)
    #expect(embedding.values[0] > embedding.values[1], "three seconds beat one")
    // Each chunk is brought to unit length first, so only the durations
    // weigh: axis 0 gets 3, axis 1 gets 1. The norm of 5 plays no part.
    #expect(abs(embedding.values[0] / embedding.values[1] - 3) < 1e-3)
  }

  /// FluidAudio's `embedding256` is the raw WeSpeaker output: two windows of
  /// equal length with norms 1 and 20 must weigh the same, or a few loud or
  /// clipped windows steer the whole cluster.
  @Test func aHighNormChunkDoesNotSteerTheMean() throws {
    let chunks = [chunk("S1", 0, 2, axis: 0), chunk("S1", 2, 4, axis: 1, scale: 20)]
    let embedding = try #require(ClusterEmbedding.embedding(of: chunks))
    #expect(abs(embedding.values[0] - embedding.values[1]) < 1e-5)
    #expect(abs(embedding.values[0] - 0.7071) < 1e-3)
  }

  @Test func chunksWithTheWrongDimensionAreIgnored() {
    let odd = ClusterChunk(speakerLabel: "S1", start: 0, end: 1, embedding: [1, 0], quality: 1)
    #expect(ClusterEmbedding.embedding(of: [odd]) == nil)
    #expect(ClusterEmbedding.embedding(of: []) == nil)
    #expect(ClusterEmbedding.embedding(of: [chunk("S1", 1, 1, axis: 0)]) == nil, "zero duration")
  }
}

@Suite struct SampleClipPickerTests {
  @Test func picksTheLongestRangeCappedToTenSecondsAroundTheBestChunk() throws {
    let ranges: [ClosedRange<TimeInterval>] = [0...4, 10...40, 50...52]
    let chunks = [
      chunk("S1", 12, 14, axis: 0, quality: 0.5),
      chunk("S1", 30, 32, axis: 0, quality: 0.95),
      chunk("S1", 50, 52, axis: 0, quality: 0.99),  // outside the longest range
    ]
    let choice = SampleClipPicker.pick(ranges: ranges, chunks: chunks)
    let clip = try #require(choice.range)
    #expect(clip.upperBound - clip.lowerBound == 10)
    #expect(clip.lowerBound == 26 && clip.upperBound == 36, "centred on the 30...32 chunk")
    #expect(clip.lowerBound >= 10 && clip.upperBound <= 40)
    #expect(abs(choice.clusterConfidence - (0.5 * 2 + 0.95 * 2 + 0.99 * 2) / 6) < 1e-5)
  }

  @Test func clipIsShiftedInsideTheRangeNearItsEdges() throws {
    let choice = SampleClipPicker.pick(
      ranges: [0...20], chunks: [chunk("S1", 0, 1, axis: 0, quality: 1)])
    let clip = try #require(choice.range)
    #expect(clip == 0...10)
    let late = SampleClipPicker.pick(
      ranges: [0...20], chunks: [chunk("S1", 19, 20, axis: 0, quality: 1)])
    #expect(late.range == 10...20)
  }

  @Test func shortLongestRangeHalvesConfidenceAndShortensTheClip() throws {
    let choice = SampleClipPicker.pick(
      ranges: [0...2, 5...6.5], chunks: [chunk("S1", 0, 2, axis: 0, quality: 0.8)])
    #expect(choice.range == 0...2)
    #expect(abs(choice.clusterConfidence - 0.4) < 1e-6)
  }

  @Test func withoutChunksTheClipIsCentredAndConfidenceZero() {
    let choice = SampleClipPicker.pick(ranges: [0...30], chunks: [])
    #expect(choice.range == 10...20)
    #expect(choice.clusterConfidence == 0)
    #expect(
      SampleClipPicker.pick(ranges: [], chunks: []) == .init(range: nil, clusterConfidence: 0))
  }
}

@Suite struct FluidDiarizerMappingTests {
  private func turn(
    _ label: String, _ start: TimeInterval, _ end: TimeInterval, quality: Float = 0.9
  )
    -> SpeakerTurn
  {
    SpeakerTurn(speakerLabel: label, start: start, end: end, quality: quality)
  }

  @Test func clustersAreLabelledInOrderOfFirstSpeechWithMergedRanges() throws {
    let turns = [
      turn("S7", 5, 8), turn("S2", 0, 2), turn("S2", 2, 4.5), turn("S7", 8, 9), turn("S2", 12, 13),
    ]
    let chunks = [
      chunk("S2", 0, 4, axis: 0), chunk("S7", 5, 9, axis: 1), chunk("S2", 12, 13, axis: 0),
    ]
    let result = DiarizationMapping.result(turns: turns, chunks: chunks)
    #expect(result.clusters.map(\.label) == ["Speaker 1", "Speaker 2"])
    let first = result.clusters[0]
    #expect(first.ranges == [0...4.5, 12...13], "touching turns merge, the gap stays")
    #expect(result.clusters[1].ranges == [5...9])
    let embedding = try #require(first.embedding)
    #expect(abs(embedding.magnitude - 1) < 1e-5)
    #expect(embedding.values[0] > 0.99, "axis 0 only")
    #expect(result.clusters[1].embedding?.values[1] ?? 0 > 0.99)
    #expect(first.sampleClipRange == 0...4.5)
    #expect(abs(first.clusterConfidence - 0.9) < 1e-6)
  }

  @Test func chunkQualityComesFromTheOverlappingTurn() {
    let turns = [turn("S1", 0, 5, quality: 0.3), turn("S1", 5, 10, quality: 0.7)]
    let chunks = DiarizationMapping.assigningQuality(
      to: [chunk("S1", 0, 4, axis: 0), chunk("S1", 4, 9, axis: 0), chunk("S1", 20, 21, axis: 0)],
      from: turns)
    #expect(chunks.map(\.quality) == [0.3, 0.7, 1])
    // `result` assigns first: the confidence is the duration-weighted mean of
    // the turns' qualities, whatever the chunks arrived with.
    let result = DiarizationMapping.result(
      turns: turns, chunks: [chunk("S1", 0, 4, axis: 0, quality: 0), chunk("S1", 4, 9, axis: 0)])
    // (0.3 * 4 s + 0.7 * 5 s) / 9 s, spelled out so the type-checker stays quick.
    let expected: Float = 4.7 / 9
    #expect(abs((result.clusters.first?.clusterConfidence ?? 0) - expected) < 1e-5)
  }

  @Test func shortSpeakersArePenalisedAndTurnlessChunkLabelsAreNotSpeakers() {
    let turns = [turn("S1", 0, 2, quality: 1), turn("S1", 4, 5, quality: 1)]
    let chunks = [
      chunk("S1", 0, 2, axis: 0, quality: 1), chunk("S9", 30, 31, axis: 1, quality: 0.5),
    ]
    let result = DiarizationMapping.result(turns: turns, chunks: chunks)
    #expect(result.clusters.map(\.label) == ["Speaker 1"], "S9 lost every frame vote")
    #expect(abs(result.clusters[0].clusterConfidence - 0.5) < 1e-6, "under three seconds: halved")
  }

  /// The shape the real model produced on an Anna + Daniel `say` fixture:
  /// two speakers alternate in the turns, and a third cluster label exists
  /// only in the chunks with a window-sized span over both. It must not
  /// become a speaker, and no two speakers' ranges may overlap.
  @Test func aChunkOnlyLabelSpanningTheWindowDoesNotOverlapTheRealSpeakers() {
    let turns = [
      turn("S1", 0, 1.94), turn("S2", 2.28, 4.35), turn("S1", 4.67, 6.64), turn("S2", 6.98, 9.37),
    ]
    let chunks = [
      chunk("S1", 0, 6.64, axis: 0), chunk("S2", 2.28, 9.37, axis: 1),
      chunk("S3", 4.00, 9.35, axis: 2),
    ]
    let result = DiarizationMapping.result(turns: turns, chunks: chunks)
    #expect(result.clusters.map(\.label) == ["Speaker 1", "Speaker 2"])
    #expect(result.clusters[0].ranges == [0...1.94, 4.67...6.64])
    #expect(result.clusters[1].ranges == [2.28...4.35, 6.98...9.37])
    let all = result.clusters.flatMap { cluster in cluster.ranges.map { (cluster.label, $0) } }
    for lhs in all {
      for rhs in all where lhs.0 != rhs.0 {
        #expect(!lhs.1.overlaps(rhs.1), "\(lhs) overlaps \(rhs)")
      }
    }
  }

  @Test func withoutChunksConfidenceComesFromTurns() {
    let result = DiarizationMapping.result(
      turns: [turn("S1", 0, 6, quality: 0.8), turn("S1", 6, 8, quality: 0.4)], chunks: [])
    #expect(result.clusters.count == 1)
    #expect(result.clusters[0].embedding == nil)
    #expect(abs(result.clusters[0].clusterConfidence - 0.7) < 1e-6)
    #expect(result.clusters[0].sampleClipRange == 0...8)
  }

  @Test func oneSpeakerGivesOneClusterWithTheClipInsideTheRange() throws {
    let turns = [turn("S1", 0.5, 6, quality: 0.8), turn("S1", 6, 14, quality: 0.9)]
    let chunks = [
      chunk("S1", 0.5, 7, axis: 3, quality: 0.8), chunk("S1", 7, 14, axis: 3, quality: 0.9),
    ]
    let result = DiarizationMapping.result(turns: turns, chunks: chunks)
    try #require(result.clusters.count == 1)
    let only = result.clusters[0]
    #expect(only.label == "Speaker 1")
    #expect(only.ranges == [0.5...14])
    let clip = try #require(only.sampleClipRange)
    #expect(clip.upperBound - clip.lowerBound == 10)
    #expect(clip.lowerBound >= 0.5 && clip.upperBound <= 14)
    #expect(clip.contains(10.5), "centred on the better second chunk")
    #expect(abs(only.clusterConfidence - Float((0.8 * 6.5 + 0.9 * 7) / 13.5)) < 1e-5)
    #expect(only.embedding?.values[3] ?? 0 > 0.99)
  }

  /// A two-second recording: everything is under the three-second floor,
  /// so the clip is the whole range and the confidence is halved, and no
  /// clip ever reaches past the audio.
  @Test func shortAudioKeepsClipsInsideTheAudioAndPenalisesEveryone() throws {
    let turns = [turn("S1", 0, 1.2, quality: 1), turn("S2", 1.2, 2, quality: 1)]
    let chunks = [
      chunk("S1", 0, 1.2, axis: 0, quality: 1), chunk("S2", 1.2, 2, axis: 1, quality: 1),
    ]
    let result = DiarizationMapping.result(turns: turns, chunks: chunks)
    try #require(result.clusters.count == 2)
    #expect(result.clusters[0].sampleClipRange == 0...1.2)
    #expect(result.clusters[1].sampleClipRange == 1.2...2)
    #expect(result.clusters.allSatisfy { abs($0.clusterConfidence - 0.5) < 1e-6 })
    #expect(result.clusters.allSatisfy { ($0.sampleClipRange?.upperBound ?? 0) <= 2 })
  }

  /// Core documents `sampleClipRange` as at most ten seconds; the picker's
  /// constants are the one place that number lives and no config can raise it.
  @Test func theClipNeverExceedsCoresTenSecondCap() throws {
    #expect(SampleClipPicker.targetSeconds == 10)
    #expect(SampleClipPicker.minimumSeconds == 3)
    let result = DiarizationMapping.result(
      turns: [turn("S1", 0, 300, quality: 1)], chunks: [chunk("S1", 100, 102, axis: 0)])
    let clip = try #require(result.clusters.first?.sampleClipRange)
    #expect(clip.upperBound - clip.lowerBound == 10)
    #expect(clip == 96...106, "centred on the one chunk")
  }

  /// Invariants over generated diarizations, whatever the shape: labels are
  /// sequential in order of first speech, ranges are sorted and disjoint,
  /// every clip lies inside one of its cluster's ranges and within the
  /// target, embeddings are unit vectors, confidence is in `0...1`. Half the
  /// recordings also carry a chunk whose label has no turn (`S0`, spanning
  /// the window over whoever spoke), the phantom the real model produced;
  /// it must not become a cluster.
  @Test func invariantsHoldOverGeneratedInputs() throws {
    var rng = SplitMix64(seed: 2026)
    for _ in 0..<40 {
      let speakers = Int.random(in: 1...4, using: &rng)
      var turns: [SpeakerTurn] = []
      var chunks: [ClusterChunk] = []
      var cursor = 0.0
      for _ in 0..<Int.random(in: 1...12, using: &rng) {
        let label = "S\(Int.random(in: 1...speakers, using: &rng))"
        let length = Double.random(in: 0.2...12, using: &rng)
        let quality = Float.random(in: 0...1, using: &rng)
        turns.append(
          SpeakerTurn(speakerLabel: label, start: cursor, end: cursor + length, quality: quality))
        if Bool.random(using: &rng) {
          chunks.append(
            ClusterChunk(
              speakerLabel: label, start: cursor, end: cursor + min(length, 5),
              embedding: vector(Int.random(in: 0..<8, using: &rng), scale: 3), quality: 1))
        }
        cursor += length + Double.random(in: 0...1, using: &rng)
      }
      if Bool.random(using: &rng) {
        chunks.append(
          ClusterChunk(
            speakerLabel: "S0", start: max(0, cursor - 10), end: cursor,
            embedding: vector(8, scale: 3), quality: 1))
      }
      let result = DiarizationMapping.result(turns: turns, chunks: chunks)
      var order: [String] = []
      for label in turns.sorted(by: { $0.start < $1.start }).map(\.speakerLabel)
      where !order.contains(label) {
        order.append(label)
      }
      #expect(result.clusters.count == order.count)
      #expect(result.clusters.map(\.label) == order.indices.map { "Speaker \($0 + 1)" })
      let labelled = result.clusters.flatMap { cluster in cluster.ranges.map { (cluster.label, $0) }
      }
      for lhs in labelled {
        for rhs in labelled where lhs.0 != rhs.0 && lhs.1.lowerBound < rhs.1.lowerBound {
          #expect(lhs.1.upperBound <= rhs.1.lowerBound, "speakers overlap: \(lhs) \(rhs)")
        }
      }
      for cluster in result.clusters {
        for (lhs, rhs) in zip(cluster.ranges, cluster.ranges.dropFirst()) {
          #expect(lhs.upperBound < rhs.lowerBound, "ranges sorted and disjoint")
        }
        let clip = try #require(cluster.sampleClipRange)
        #expect(clip.upperBound - clip.lowerBound <= 10 + 1e-9)
        #expect(
          cluster.ranges.contains {
            $0.lowerBound <= clip.lowerBound + 1e-9 && clip.upperBound <= $0.upperBound + 1e-9
          }, "\(clip) outside \(cluster.ranges)")
        #expect(cluster.clusterConfidence >= 0 && cluster.clusterConfidence <= 1)
        if let embedding = cluster.embedding { #expect(abs(embedding.magnitude - 1) < 1e-4) }
      }
    }
  }

  @Test func emptyInputGivesNoClusters() {
    #expect(DiarizationMapping.result(turns: [], chunks: []).clusters.isEmpty)
    #expect(DiarizationMapping.merged([]) == [])
    #expect(DiarizationMapping.merged([3...4, 0...1, 1...2]) == [0...2, 3...4])
  }
}
