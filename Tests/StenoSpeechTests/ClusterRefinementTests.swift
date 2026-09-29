import Foundation
import StenoCore
import Testing

@testable import StenoSpeech

/// Reads the speaker off the samples: every range of one speaker in these
/// tests is filled with one constant, and the embedding of a slice is the
/// unit vector whose axes carry how much of each constant it holds. Two
/// slices of the same constant have cosine 1, of different constants 0, and
/// a mixed slice sits in between with the mix as weights. Records every
/// slice it saw.
private actor FakeSliceEmbedder: SliceEmbedder {
  private(set) var seen: [TimeInterval] = []

  func embedding(of audio: AudioBuffer16k) async throws -> Embedding? {
    seen.append(audio.duration)
    var values = [Float](repeating: 0, count: Embedding.dimension)
    for sample in audio.samples where sample > 0 {
      values[Int(sample)] += 1
    }
    return values.contains { $0 > 0 } ? Embedding(values).normalized() : nil
  }
}

private func seconds(_ seconds: Double) -> Int { Int(seconds * AudioBuffer16k.sampleRate) }

/// Audio where `layout` says which speaker constant fills each range; the
/// rest is zero (silence).
private func audio(_ layout: [(Float, ClosedRange<TimeInterval>)], duration: TimeInterval)
  -> AudioBuffer16k
{
  var samples = [Float](repeating: 0, count: seconds(duration))
  for (speaker, range) in layout {
    for index in seconds(range.lowerBound)..<min(seconds(range.upperBound), samples.count) {
      samples[index] = speaker
    }
  }
  return AudioBuffer16k(samples: samples)
}

private func cluster(
  _ label: String, _ ranges: [ClosedRange<TimeInterval>], axis: Int, confidence: Float = 1
) -> SpeakerCluster {
  var values = [Float](repeating: 0, count: Embedding.dimension)
  values[axis] = 1
  return SpeakerCluster(
    label: label, ranges: ranges, embedding: Embedding(values), clusterConfidence: confidence,
    sampleClipRange: ranges.first)
}

@Suite struct ClusterRefinementTests {
  /// The 1:1 shape from the calibration corpus: one long cluster and one
  /// cluster of the same voice's interjections that the window embeddings
  /// put on another axis. Re-embedded over their own speech they are one
  /// voice, so they merge; the merged cluster keeps the big one's clip.
  @Test func interjectionsOfTheSameVoiceMergeIntoTheMainCluster() async throws {
    let main: [ClosedRange<TimeInterval>] = [0...60, 100...160]
    let asides: [ClosedRange<TimeInterval>] = [70...90, 170...185]
    let buffer = audio((main + asides).map { (1, $0) }, duration: 200)
    let embedder = FakeSliceEmbedder()
    let refined = try await ClusterRefinement.refine(
      [cluster("Speaker 1", main, axis: 5), cluster("Speaker 2", asides, axis: 9)],
      in: buffer, embedder: embedder)
    #expect(refined.count == 1)
    #expect(refined.first?.label == "Speaker 1")
    #expect(refined.first?.ranges == [0...60, 70...90, 100...160, 170...185])
    #expect(refined.first?.sampleClipRange == 0...60)
    // The embedding is the re-embedded union, not the window mean.
    #expect(refined.first?.embedding?.values[1] ?? 0 > 0.99)
    // Two clusters embedded, then the union once.
    #expect(await embedder.seen.count == 3)
  }

  /// Two people who each speak for long: different constants, cosine 0,
  /// nothing merges, and the labels follow the order of first speech.
  @Test func differentVoicesStayApartAndAreLabelledByFirstSpeech() async throws {
    let anna: [ClosedRange<TimeInterval>] = [40...100]
    let ben: [ClosedRange<TimeInterval>] = [0...35, 110...150]
    let buffer = audio([(1, anna[0]), (2, ben[0]), (2, ben[1])], duration: 160)
    let refined = try await ClusterRefinement.refine(
      [cluster("Speaker 1", anna, axis: 1), cluster("Speaker 2", ben, axis: 2)],
      in: buffer, embedder: FakeSliceEmbedder())
    #expect(refined.map(\.label) == ["Speaker 1", "Speaker 2"])
    #expect(refined[0].ranges == ben, "Ben opens the recording")
    #expect(refined[1].ranges == anna)
  }

  /// A cluster under thirty seconds is nobody on its own: it joins the
  /// substantive cluster it sounds like (the big cluster's embedding, clip
  /// and confidence stay), and one that sounds like nobody is dropped.
  @Test func smallClustersAreAbsorbedOrDropped() async throws {
    let anna: [ClosedRange<TimeInterval>] = [0...60]
    let annaAside: [ClosedRange<TimeInterval>] = [70...80]
    let noise: [ClosedRange<TimeInterval>] = [90...95]
    let buffer = audio([(1, anna[0]), (1, annaAside[0]), (3, noise[0])], duration: 100)
    let refined = try await ClusterRefinement.refine(
      [
        cluster("Speaker 1", anna, axis: 1, confidence: 0.9),
        cluster("Speaker 2", annaAside, axis: 7),
        cluster("Speaker 3", noise, axis: 8),
      ],
      in: buffer, embedder: FakeSliceEmbedder())
    #expect(refined.count == 1)
    #expect(refined[0].ranges == [0...60, 70...80])
    #expect(refined[0].clusterConfidence == 0.9)
    #expect(refined[0].sampleClipRange == 0...60)
  }

  /// Without a single substantive cluster (a nine-second fixture) nothing is
  /// re-embedded and the mapping's clusters come back untouched.
  @Test func shortRecordingsAreLeftAlone() async throws {
    let clusters = [
      cluster("Speaker 1", [0...2, 4.6...6.6], axis: 1),
      cluster("Speaker 2", [2.3...4.3, 7...9.4], axis: 2),
    ]
    let embedder = FakeSliceEmbedder()
    let refined = try await ClusterRefinement.refine(
      clusters, in: audio([], duration: 10), embedder: embedder)
    #expect(refined == clusters)
    #expect(await embedder.seen.isEmpty)
  }

  /// Merging is greedy by the highest cosine and re-embeds each union, so
  /// three fragments of one voice collapse into one cluster while a second
  /// voice survives; the embedder never sees more than the cap per slice.
  @Test func mergesGreedilyAndCapsTheEmbeddedSpeech() async throws {
    let one: [[ClosedRange<TimeInterval>]] = [[0...200], [210...250], [260...300]]
    let two: [ClosedRange<TimeInterval>] = [310...400]
    let layout = one.flatMap { $0 }.map { (Float(1), $0) } + two.map { (Float(2), $0) }
    let buffer = audio(layout, duration: 410)
    let embedder = FakeSliceEmbedder()
    let refined = try await ClusterRefinement.refine(
      [
        cluster("Speaker 1", one[0], axis: 3), cluster("Speaker 2", one[1], axis: 4),
        cluster("Speaker 3", one[2], axis: 5), cluster("Speaker 4", two, axis: 6),
      ],
      in: buffer, rules: ClusterRefinement.Rules(maximumEmbedSeconds: 120), embedder: embedder)
    #expect(refined.map(\.label) == ["Speaker 1", "Speaker 2"])
    #expect(refined[0].ranges == [0...200, 210...250, 260...300])
    #expect(refined[1].ranges == two)
    let longest = await embedder.seen.max() ?? 0
    #expect(longest <= 120 + 1e-6)
  }

  /// The concatenation keeps time order, clips to the buffer and stops at
  /// the cap.
  @Test func concatenationOrdersClipsAndCaps() {
    let buffer = audio([(1, 0...10), (2, 10...20)], duration: 20)
    let joined = ClusterRefinement.concatenated([15...30, 2...4], in: buffer, cap: 6)
    #expect(abs(joined.duration - 6) < 1e-6)
    #expect(joined.samples.prefix(seconds(2)).allSatisfy { $0 == 1 })
    #expect(joined.samples.suffix(seconds(4)).allSatisfy { $0 == 2 })
  }

  /// The plan's constants: 30 s to count as a speaker, 180 s embedded at
  /// most, merge at 0.60, absorb at 0.30. Without this a drifted default
  /// would pass every relative test above and still change real results.
  @Test func rulesDefaultToThePlanConstants() {
    let rules = ClusterRefinement.Rules.default
    #expect(rules.minimumSeconds == 30 && rules.maximumEmbedSeconds == 180)
    #expect(rules.mergeThreshold == 0.60 && rules.absorbThreshold == 0.30)
  }

  /// Exactly thirty seconds of speech is a speaker; a hair under is not.
  /// With `>` in place of `>=` both clusters would be small, nothing would
  /// be substantive and the pair would come back untouched.
  @Test func thirtySecondsOfSpeechIsTheSubstantiveCut() async throws {
    let anna: [ClosedRange<TimeInterval>] = [0...30]
    let ben: [ClosedRange<TimeInterval>] = [40...69.9]
    let buffer = audio([(1, anna[0]), (2, ben[0])], duration: 80)
    let refined = try await ClusterRefinement.refine(
      [cluster("Speaker 1", anna, axis: 1), cluster("Speaker 2", ben, axis: 2)],
      in: buffer, embedder: FakeSliceEmbedder())
    #expect(refined.count == 1)
    #expect(refined.first?.ranges == anna, "Ben's 29.9 s sound like nobody and are dropped")
  }

  /// When the bigger half of a merge is the later cluster, `joined` keeps
  /// its label and clip, so the pass must hand labels out again: the union
  /// opens the recording and is "Speaker 1", the other voice moves up to
  /// "Speaker 2".
  @Test func labelsAreReassignedWhenAMergeKeepsTheLaterLabel() async throws {
    let opening: [ClosedRange<TimeInterval>] = [0...35]
    let other: [ClosedRange<TimeInterval>] = [40...100]
    let main: [ClosedRange<TimeInterval>] = [110...200]
    let buffer = audio([(1, opening[0]), (2, other[0]), (1, main[0])], duration: 210)
    let refined = try await ClusterRefinement.refine(
      [
        cluster("Speaker 1", opening, axis: 1), cluster("Speaker 2", other, axis: 2),
        cluster("Speaker 3", main, axis: 3),
      ],
      in: buffer, embedder: FakeSliceEmbedder())
    #expect(refined.map(\.label) == ["Speaker 1", "Speaker 2"])
    #expect(refined[0].ranges == [0...35, 110...200])
    #expect(refined[0].sampleClipRange == main[0], "the clip follows the bigger half")
    #expect(refined[1].ranges == other)
  }

  /// A small cluster joins the substantive cluster with the highest cosine,
  /// not the first one over the absorb cut: a ten-second aside that is
  /// mostly Ben with a little Anna (0.83 to Ben, 0.55 to Anna) ends up with
  /// Ben.
  @Test func aSmallClusterJoinsTheClosestSpeakerNotTheFirstOverTheCut() async throws {
    let anna: [ClosedRange<TimeInterval>] = [0...60]
    let ben: [ClosedRange<TimeInterval>] = [70...130]
    let aside: [ClosedRange<TimeInterval>] = [140...150]
    let buffer = audio(
      [(1, anna[0]), (2, ben[0]), (1, 140...144), (2, 144...150)], duration: 160)
    let refined = try await ClusterRefinement.refine(
      [
        cluster("Speaker 1", anna, axis: 1), cluster("Speaker 2", ben, axis: 2),
        cluster("Speaker 3", aside, axis: 3),
      ],
      in: buffer, embedder: FakeSliceEmbedder())
    #expect(refined.map(\.label) == ["Speaker 1", "Speaker 2"])
    #expect(refined[0].ranges == anna)
    #expect(refined[1].ranges == [70...130, 140...150])
  }

  /// The union of two merged clusters can embed to nothing (the one-speaker
  /// pipeline hears no speech in it); the merged cluster then keeps the
  /// embedding of the side with more speech instead of losing it.
  @Test func aUnionThatEmbedsToNothingKeepsTheLargerSidesEmbedding() async throws {
    let big = cluster("Speaker 1", [0...90], axis: 4)
    var small = cluster("Speaker 2", [100...140], axis: 4)
    small.embedding?.values[5] = 1  // cosine 0.71 to `big`, over the merge cut
    let embedder = FakeSliceEmbedder()
    let refined = try await ClusterRefinement.refine(
      [big, small], in: audio([], duration: 150), embedder: embedder)
    #expect(refined.count == 1)
    #expect(refined.first?.label == "Speaker 1")
    #expect(refined.first?.ranges == [0...90, 100...140])
    #expect(refined.first?.embedding == big.embedding)
    // Both clusters and the union were offered to the embedder.
    #expect(await embedder.seen.count == 3)
  }

  /// A substantive cluster that came without an embedding and whose speech
  /// the embedder hears nothing in stays a speaker of its own: the merge
  /// and the absorb skip it, nothing drops it.
  @Test func aSubstantiveClusterWithoutAnEmbeddingSurvivesOnItsOwn() async throws {
    let anna: [ClosedRange<TimeInterval>] = [0...60]
    let silent: [ClosedRange<TimeInterval>] = [70...110]
    let buffer = audio([(1, anna[0])], duration: 120)
    let refined = try await ClusterRefinement.refine(
      [
        cluster("Speaker 1", anna, axis: 1),
        SpeakerCluster(label: "Speaker 2", ranges: silent, clusterConfidence: 0.5),
      ],
      in: buffer, embedder: FakeSliceEmbedder())
    #expect(refined.map(\.label) == ["Speaker 1", "Speaker 2"])
    #expect(refined[1].ranges == silent)
    #expect(refined[1].embedding == nil)
  }

  /// With no substantive cluster carrying an embedding there is nothing to
  /// compare a small cluster against, so it is dropped rather than attached
  /// to an arbitrary speaker.
  @Test func smallClustersAreDroppedWhenNoSpeakerHasAnEmbedding() async throws {
    let silent: [ClosedRange<TimeInterval>] = [0...40]
    let aside: [ClosedRange<TimeInterval>] = [50...60]
    let buffer = audio([(2, aside[0])], duration: 70)
    let refined = try await ClusterRefinement.refine(
      [
        SpeakerCluster(label: "Speaker 1", ranges: silent, clusterConfidence: 1),
        cluster("Speaker 2", aside, axis: 2),
      ],
      in: buffer, embedder: FakeSliceEmbedder())
    #expect(refined.count == 1)
    #expect(refined.first?.ranges == silent)
    #expect(refined.first?.embedding == nil)
  }

  /// Ranges past the end of the lane (a mapping a little longer than the
  /// audio) yield no samples, and a slice without samples is never offered
  /// to the embedder; the cluster keeps the embedding it came with.
  @Test func rangesBeyondTheBufferAreNotEmbedded() async throws {
    let clusters = [cluster("Speaker 1", [100...140], axis: 1)]
    let embedder = FakeSliceEmbedder()
    let refined = try await ClusterRefinement.refine(
      clusters, in: audio([], duration: 50), embedder: embedder)
    #expect(refined == clusters)
    #expect(await embedder.seen.isEmpty)
  }
}
