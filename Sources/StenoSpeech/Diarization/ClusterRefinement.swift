import Foundation
import StenoCore

/// Embeds one stretch of speech as a single speaker. `FluidDiarizer` fulfils
/// it with a second pipeline instance capped at one speaker; tests use a
/// fake that reads the speaker off the samples.
protocol SliceEmbedder: Sendable {
  /// nil when the audio holds no usable speech.
  func embedding(of audio: AudioBuffer16k) async throws -> Embedding?
}

/// The post-pass over the mapped clusters decided in
/// `.plans/2026-09-29-speaker-calibration.md`: a speaker's short turns embed
/// far from the same voice speaking at length, because each ten-second
/// window holds little of them, so the pipeline splits every 1:1 into a main
/// cluster and one or two clusters of interjections. Re-embedding each
/// cluster over its own concatenated speech gives comparable vectors; on the
/// calibration corpus the same person then scores 0.68 to 0.94 and different
/// people at most 0.50.
///
/// Rules, in order: clusters with at least `minimumSeconds` of speech are
/// re-embedded and merged greedily, highest cosine first, while a pair
/// reaches `mergeThreshold`; each merged cluster is re-embedded over the
/// union. Clusters under `minimumSeconds` join the substantive cluster they
/// are closest to when that cosine reaches `absorbThreshold`, otherwise
/// they are dropped (their segments stay speaker-less). Labels are handed
/// out again as "Speaker n" in order of first speech. A result without any
/// substantive cluster is returned unchanged, so short fixtures keep their
/// speakers.
enum ClusterRefinement {
  struct Rules: Sendable, Equatable {
    /// Speech a cluster needs to count as a speaker on its own.
    var minimumSeconds: TimeInterval = 30
    /// How much of a cluster's speech is embedded; the first stretches win.
    var maximumEmbedSeconds: TimeInterval = 180
    var mergeThreshold: Float = 0.60
    var absorbThreshold: Float = 0.30

    static let `default` = Rules()
  }

  static func refine(
    _ clusters: [SpeakerCluster], in audio: AudioBuffer16k, rules: Rules = .default,
    embedder: any SliceEmbedder
  ) async throws -> [SpeakerCluster] {
    var substantive = clusters.filter { speechSeconds($0) >= rules.minimumSeconds }
    guard !substantive.isEmpty else { return clusters }
    let small = clusters.filter { speechSeconds($0) < rules.minimumSeconds }

    for index in substantive.indices {
      if let embedding = try await embed(substantive[index].ranges, in: audio, rules, embedder) {
        substantive[index].embedding = embedding
      }
    }

    // Greedy merge, highest cosine first. Each merge re-embeds the union, so
    // the loop runs at most `substantive.count - 1` more embeddings.
    while substantive.count > 1, let pair = closestPair(substantive),
      pair.cosine >= rules.mergeThreshold
    {
      let (keep, drop) = pair.lhs >= pair.rhs ? (pair.rhs, pair.lhs) : (pair.lhs, pair.rhs)
      var merged = joined(substantive[keep], substantive[drop])
      if let embedding = try await embed(merged.ranges, in: audio, rules, embedder) {
        merged.embedding = embedding
      }
      substantive[keep] = merged
      substantive.remove(at: drop)
    }

    for cluster in small {
      let embedding =
        try await embed(cluster.ranges, in: audio, rules, embedder) ?? cluster.embedding
      guard let embedding else { continue }
      let scored = substantive.indices.map { index -> (index: Int, cosine: Float) in
        (index, substantive[index].embedding?.cosineSimilarity(to: embedding) ?? -1)
      }
      guard let best = scored.max(by: { $0.cosine < $1.cosine }),
        best.cosine >= rules.absorbThreshold
      else {
        continue
      }
      // The big cluster keeps its embedding, clip and confidence; only the
      // ranges grow.
      substantive[best.index].ranges = DiarizationMapping.merged(
        substantive[best.index].ranges + cluster.ranges)
    }

    return substantive
      .sorted { firstSpeech($0) < firstSpeech($1) }
      .enumerated()
      .map { index, cluster in
        var cluster = cluster
        cluster.label = "Speaker \(index + 1)"
        return cluster
      }
  }

  static func speechSeconds(_ cluster: SpeakerCluster) -> TimeInterval {
    cluster.ranges.reduce(0) { $0 + $1.upperBound - $1.lowerBound }
  }

  private static func firstSpeech(_ cluster: SpeakerCluster) -> TimeInterval {
    cluster.ranges.map(\.lowerBound).min() ?? .greatestFiniteMagnitude
  }

  /// The cluster's speech, ranges in time order, concatenated up to
  /// `rules.maximumEmbedSeconds`, then embedded. nil when there is no audio
  /// or the embedder hears no speech.
  private static func embed(
    _ ranges: [ClosedRange<TimeInterval>], in audio: AudioBuffer16k, _ rules: Rules,
    _ embedder: any SliceEmbedder
  ) async throws -> Embedding? {
    let speech = concatenated(ranges, in: audio, cap: rules.maximumEmbedSeconds)
    guard !speech.samples.isEmpty else { return nil }
    return try await embedder.embedding(of: speech)
  }

  /// The samples under `ranges`, sorted and clipped to the buffer, joined
  /// until `cap` seconds are collected.
  static func concatenated(
    _ ranges: [ClosedRange<TimeInterval>], in audio: AudioBuffer16k, cap: TimeInterval
  ) -> AudioBuffer16k {
    var samples: [Float] = []
    let limit = Int(cap * AudioBuffer16k.sampleRate)
    samples.reserveCapacity(min(limit, audio.samples.count))
    for range in DiarizationMapping.merged(ranges) {
      let remaining = limit - samples.count
      guard remaining > 0 else { break }
      let slice = audio.slice(range).samples
      samples.append(contentsOf: slice.prefix(remaining))
    }
    return AudioBuffer16k(samples: samples)
  }

  private static func closestPair(_ clusters: [SpeakerCluster])
    -> (lhs: Int, rhs: Int, cosine: Float)?
  {
    var best: (lhs: Int, rhs: Int, cosine: Float)?
    for lhs in clusters.indices {
      guard let left = clusters[lhs].embedding else { continue }
      for rhs in clusters.indices where rhs > lhs {
        guard let right = clusters[rhs].embedding else { continue }
        let cosine = left.cosineSimilarity(to: right)
        if best == nil || cosine > best!.cosine { best = (lhs, rhs, cosine) }
      }
    }
    return best
  }

  /// One cluster from two: ranges joined, the label, clip and confidence of
  /// the one with more speech, the embedding of the larger until the caller
  /// re-embeds the union.
  private static func joined(_ lhs: SpeakerCluster, _ rhs: SpeakerCluster) -> SpeakerCluster {
    let (big, small) = speechSeconds(lhs) >= speechSeconds(rhs) ? (lhs, rhs) : (rhs, lhs)
    return SpeakerCluster(
      label: big.label,
      ranges: DiarizationMapping.merged(lhs.ranges + rhs.ranges),
      embedding: big.embedding ?? small.embedding,
      clusterConfidence: big.clusterConfidence,
      sampleClipRange: big.sampleClipRange ?? small.sampleClipRange)
  }
}
