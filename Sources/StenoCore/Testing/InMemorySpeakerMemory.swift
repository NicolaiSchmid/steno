import Foundation

/// A `SpeakerMemory` over an in-memory list of people: cosine ranking and a
/// running-mean enrol, plus a record of every enrol call for assertions.
public actor InMemorySpeakerMemory: SpeakerMemory {
  public struct Enrolment: Sendable, Equatable {
    public var embedding: Embedding
    public var personID: UUID
  }

  public private(set) var people: [UUID: Person]
  public private(set) var enrolments: [Enrolment] = []
  public let maxSamples: Int

  public init(people: [Person] = [], maxSamples: Int = 50) {
    self.people = Dictionary(uniqueKeysWithValues: people.map { ($0.id, $0) })
    self.maxSamples = maxSamples
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

  public func enroll(_ embedding: Embedding, as person: Person) async throws {
    var updated = people[person.id] ?? person
    let count = min(updated.sampleCount, maxSamples)
    if let known = updated.embedding, count > 0 {
      updated.embedding = Embedding.weightedMean(known, weight: Float(count), embedding, weight: 1)
    } else {
      updated.embedding = embedding.normalized()
    }
    updated.sampleCount = count + 1
    people[person.id] = updated
    enrolments.append(Enrolment(embedding: embedding, personID: person.id))
  }
}
