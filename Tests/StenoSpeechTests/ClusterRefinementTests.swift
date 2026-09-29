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
    guard !audio.samples.isEmpty else { return nil }
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
}
