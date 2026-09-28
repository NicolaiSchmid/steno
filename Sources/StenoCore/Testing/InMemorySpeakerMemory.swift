import Foundation

/// A `SpeakerMemory` over an in-memory list of people: cosine ranking over
/// the voices it was given, independent of any store.
public actor InMemorySpeakerMemory: SpeakerMemory {
  public private(set) var people: [UUID: Person]

  public init(people: [Person] = []) {
    self.people = Dictionary(uniqueKeysWithValues: people.map { ($0.id, $0) })
  }

  public func candidates(for embedding: Embedding, limit: Int) async throws -> [SpeakerMatch] {
    people.values
      .compactMap { person -> SpeakerMatch? in
        guard let known = person.embedding else { return nil }
        return SpeakerMatch(person: person, similarity: known.cosineSimilarity(to: embedding))
      }
      .sorted { lhs, rhs in
        if lhs.similarity != rhs.similarity { return lhs.similarity > rhs.similarity }
        return lhs.person.id.uuidString < rhs.person.id.uuidString
      }
      .prefix(limit)
      .map { $0 }
  }
}
