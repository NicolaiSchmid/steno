import Foundation
import StenoCore

/// `SpeakerMemory` over the persons in a `MeetingStore`: cosine ranking
/// against every stored voice. The threshold and margin live in the
/// protocol's provided `match`; this type only ranks. Voices themselves are
/// written by the store when speakers are confirmed or merged.
public actor CosineSpeakerMemory: SpeakerMemory {
  private let store: MeetingStore

  public init(store: MeetingStore) {
    self.store = store
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
}
