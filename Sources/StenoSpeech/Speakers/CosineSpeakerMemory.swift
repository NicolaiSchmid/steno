import Foundation
import StenoCore

/// `SpeakerMemory` over the persons in a `MeetingStore`: cosine ranking
/// against every stored embedding, and a running-mean enrol capped at
/// `maxSamples` so a voice can drift. The threshold and margin live in the
/// protocol's provided `match`; this type only ranks and averages.
public actor CosineSpeakerMemory: SpeakerMemory {
  public let store: MeetingStore
  public let maxSamples: Int

  public init(store: MeetingStore, maxSamples: Int = 50) {
    self.store = store
    self.maxSamples = maxSamples
  }

  /// Best first; ties broken by person id so the order is stable.
  public func candidates(for embedding: Embedding, limit: Int) async throws -> [SpeakerMatch] {
    guard limit > 0 else { return [] }
    let probe = embedding.normalized()
    return try await store.persons()
      .compactMap { person -> SpeakerMatch? in
        guard let known = person.embedding, known.values.count == probe.values.count else {
          return nil
        }
        return SpeakerMatch(person: person, similarity: known.cosineSimilarity(to: probe))
      }
      .sorted { lhs, rhs in
        if lhs.similarity != rhs.similarity { return lhs.similarity > rhs.similarity }
        return lhs.person.id.uuidString < rhs.person.id.uuidString
      }
      .prefix(limit)
      .map { $0 }
  }

  /// `e' = normalise((e * n + x̂) / (n + 1))` with `n` capped at `maxSamples`
  /// and `x̂` the sample at unit length, so a caller's scale never becomes a
  /// weight. Saves the person (inserting them when the store does not know
  /// them).
  public func enroll(_ embedding: Embedding, as person: Person) async throws {
    let sample = embedding.normalized()
    var updated = try await store.person(id: person.id) ?? person
    let count = min(max(updated.sampleCount, 0), maxSamples)
    if let known = updated.embedding, count > 0, known.values.count == sample.values.count {
      updated.embedding = Embedding.weightedMean(known, weight: Float(count), sample, weight: 1)
    } else {
      updated.embedding = sample
    }
    updated.sampleCount = count + 1
    try await store.save(updated)
  }
}
