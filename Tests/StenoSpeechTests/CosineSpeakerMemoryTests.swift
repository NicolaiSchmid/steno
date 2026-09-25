import Foundation
import StenoCore
import Testing

@testable import StenoSpeech

@Suite struct EmbeddingsTests {
  @Test func normalisedHasUnitLength() {
    let unit = Embeddings.normalised([3, 4])
    #expect(unit == [0.6, 0.8])
    #expect(abs(Embeddings.norm(unit) - 1) < 1e-6)
    #expect(Embeddings.normalised([0, 0]) == [0, 0])
    #expect(Embeddings.normalised([]) == [])
  }

  @Test func cosineOfKnownVectors() {
    #expect(abs(Embeddings.cosine([1, 0], [1, 0]) - 1) < 1e-6)
    #expect(abs(Embeddings.cosine([1, 0], [0, 1])) < 1e-6)
    #expect(abs(Embeddings.cosine([1, 0], [-1, 0]) + 1) < 1e-6)
    #expect(
      abs(Embeddings.cosine([2, 0], [1, 1]) - 0.70710677) < 1e-5, "not unit length still works")
    #expect(Embeddings.cosine([1, 0], [1]) == 0)
    #expect(Embeddings.cosine([0, 0], [1, 0]) == 0)
  }

  @Test func weightedMeanIsNormalised() {
    let mean = Embeddings.weightedMean([[1, 0], [0, 1]], weights: [3, 1])
    #expect(abs(mean[0] - 0.9486833) < 1e-5)
    #expect(abs(mean[1] - 0.31622776) < 1e-5)
    #expect(Embeddings.weightedMean([], weights: []) == [])
    #expect(Embeddings.weightedMean([[1, 0]], weights: [0]) == [])
  }
}

@Suite struct CosineSpeakerMemoryTests {
  private func embedding(_ axis: Int, _ mix: Float = 0, mixAxis: Int = 2) -> Embedding {
    var values = [Float](repeating: 0, count: Embedding.dimension)
    values[axis] = 1
    values[mixAxis] = mix
    return Embedding(values).normalized()
  }

  private func makeStore() async throws -> (MeetingStore, Person, Person) {
    let store = try MeetingStore.inMemory()
    let anna = Person(
      id: SampleData.uuid(1), displayName: "Anna", embedding: embedding(0), sampleCount: 1,
      createdAt: SampleData.createdAt)
    let ben = Person(
      id: SampleData.uuid(2), displayName: "Ben", embedding: embedding(1), sampleCount: 4,
      createdAt: SampleData.createdAt)
    let noVoice = Person(
      id: SampleData.uuid(3), displayName: "Cid", createdAt: SampleData.createdAt)
    for person in [anna, ben, noVoice] { try await store.save(person) }
    return (store, anna, ben)
  }

  @Test func candidatesAreRankedByCosine() async throws {
    let (store, anna, ben) = try await makeStore()
    let memory = CosineSpeakerMemory(store: store)
    let ranked = try await memory.candidates(for: embedding(0, 0.5), limit: 5)
    #expect(ranked.map(\.person.id) == [anna.id, ben.id], "people without a voice are skipped")
    #expect(ranked[0].similarity > ranked[1].similarity)
    #expect(abs(ranked[0].similarity - 0.894_427) < 1e-4)
    #expect(try await memory.candidates(for: embedding(0), limit: 1).count == 1)
    #expect(try await memory.candidates(for: embedding(0), limit: 0).isEmpty)
  }

  @Test func matchHonoursThresholdAndMargin() async throws {
    let (store, anna, _) = try await makeStore()
    let memory = CosineSpeakerMemory(store: store)
    let hit = try await memory.match(embedding(0, 0.2), threshold: 0.6)
    #expect(hit?.person.id == anna.id)
    #expect(try await memory.match(embedding(2), threshold: 0.6) == nil, "below threshold")

    // Equidistant between Anna and Ben: both above threshold, no margin.
    var between = [Float](repeating: 0, count: Embedding.dimension)
    between[0] = 1
    between[1] = 1
    #expect(try await memory.match(Embedding(between), threshold: 0.6) == nil)
    #expect(try await memory.match(Embedding(between), threshold: 0.6, margin: 0) != nil)
  }

  @Test func enrollKeepsARunningMeanWithACap() async throws {
    let (store, anna, _) = try await makeStore()
    let memory = CosineSpeakerMemory(store: store, maxSamples: 3)
    try await memory.enroll(embedding(1), as: anna)
    var stored = try #require(try await store.person(id: anna.id))
    #expect(stored.sampleCount == 2)
    let values = try #require(stored.embedding?.values)
    #expect(abs(values[0] - values[1]) < 1e-6, "one old sample and one new: equal weight")
    #expect(abs(Embeddings.norm(values) - 1) < 1e-5)

    for _ in 0..<5 { try await memory.enroll(embedding(1), as: anna) }
    stored = try #require(try await store.person(id: anna.id))
    #expect(stored.sampleCount == 4, "count is capped at maxSamples + 1")
    let drifted = try #require(stored.embedding?.values)
    #expect(drifted[1] > 0.9, "a capped mean drifts toward the new voice")
  }

  @Test func enrollInsertsAnUnknownPerson() async throws {
    let (store, _, _) = try await makeStore()
    let memory = CosineSpeakerMemory(store: store)
    let newcomer = Person(
      id: SampleData.uuid(9), displayName: "Dora", createdAt: SampleData.createdAt)
    try await memory.enroll(
      Embedding([Float](repeating: 2, count: Embedding.dimension)), as: newcomer)
    let stored = try #require(try await store.person(id: newcomer.id))
    #expect(stored.sampleCount == 1)
    #expect(abs((stored.embedding?.magnitude ?? 0) - 1) < 1e-5, "stored normalised")
    let match = try await memory.match(
      Embedding([Float](repeating: 1, count: Embedding.dimension)), threshold: 0.6)
    #expect(match?.person.id == newcomer.id)
  }
}
