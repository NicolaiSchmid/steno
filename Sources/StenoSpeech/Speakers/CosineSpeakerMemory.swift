import Foundation
import StenoCore

/// `SpeakerMemory` over the persons in a `MeetingStore`: cosine ranking
/// against every stored embedding, and a running-mean enrol whose weight is
/// capped at `maxSamples` so a voice can drift. The threshold and margin
/// live in the protocol's provided `match`; this type only ranks and
/// averages.
public actor CosineSpeakerMemory: SpeakerMemory {
  private let store: MeetingStore
  private let maxSamples: Int

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

  /// `e' = normalise((e * w + x̂) / (w + 1))` with `w = min(sampleCount,
  /// maxSamples)` and `x̂` the sample at unit length, so a caller's scale is
  /// never a weight. `sampleCount` keeps counting (it is also the weight
  /// core's `mergePersons` uses); only the weight here is capped. Saves the
  /// person (inserting them when the store does not know them).
  public func enroll(_ embedding: Embedding, as person: Person) async throws {
    let sample = embedding.normalized()
    var updated = try await store.person(id: person.id) ?? person
    let weight = Float(min(max(updated.sampleCount, 0), maxSamples))
    if let known = updated.embedding, weight > 0, known.values.count == sample.values.count {
      updated.embedding = Embedding.weightedMean(known, weight: weight, sample, weight: 1)
    } else {
      updated.embedding = sample
    }
    updated.sampleCount = max(updated.sampleCount, 0) + 1
    try await store.save(updated)
  }
}
