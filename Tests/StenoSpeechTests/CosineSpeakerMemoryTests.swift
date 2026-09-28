import Foundation
import StenoCore
import Testing

@testable import StenoSpeech

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

  @Test func embeddingsOfAnotherDimensionAreSkippedInRanking() async throws {
    let (store, anna, ben) = try await makeStore()
    let odd = Person(
      id: SampleData.uuid(4), displayName: "Odd", embedding: Embedding([1, 0, 0]),
      sampleCount: 7, createdAt: SampleData.createdAt)
    try await store.save(odd)
    let memory = CosineSpeakerMemory(store: store)
    let ranked = try await memory.candidates(for: embedding(0), limit: 10)
    #expect(ranked.map(\.person.id) == [anna.id, ben.id], "the 3-dim row cannot be compared")
  }

  /// The manual check made deterministic: confirming a speaker in meeting
  /// one through the store enrols them, and the same voice in meeting two is
  /// suggested to that person by `match`.
  @Test func confirmingInOneMeetingSuggestsThePersonInTheNext() async throws {
    let store = try MeetingStore.inMemory()
    let memory = CosineSpeakerMemory(store: store)
    let voice = embedding(5, 0.1)
    let meetingOne = SampleData.meeting()
    try await store.save(meetingOne)
    let speaker = Speaker(
      id: SampleData.uuid(30), meetingID: meetingOne.id, clusterLabel: "Speaker 1",
      embedding: voice, clusterConfidence: 0.9)
    try await store.save(speaker)
    let dora = Person(id: SampleData.uuid(9), displayName: "Dora", createdAt: SampleData.createdAt)
    #expect(try await memory.match(voice, threshold: 0.6) == nil, "nobody is known yet")

    try await store.confirm(speakerID: speaker.id, person: dora)
    let stored = try #require(try await store.person(id: dora.id))
    #expect(stored.sampleCount == 1)
    #expect(stored.embedding == voice.normalized())

    // Meeting two: the same voice, a little different.
    let again = embedding(5, 0.2)
    let match = try #require(try await memory.match(again, threshold: 0.6))
    #expect(match.person.id == dora.id)
    #expect(match.similarity > 0.99)
    // A stranger is not.
    #expect(try await memory.match(embedding(6), threshold: 0.6) == nil)
  }

  @Test func mergedPeopleRankAsOneWithTheRecomputedVoice() async throws {
    // Anna: one confirmed speaker on axis 0; Ben: four on axis 1.
    let (store, anna, ben) = try await makeStore()
    let meeting = SampleData.meeting()
    try await store.save(meeting)
    var speakers = [
      Speaker(
        id: SampleData.uuid(40), meetingID: meeting.id, clusterLabel: "Speaker 1",
        assignment: .confirmed(personID: anna.id), embedding: embedding(0), clusterConfidence: 0.9)
    ]
    for index in 0..<4 {
      speakers.append(
        Speaker(
          id: SampleData.uuid(41 + index), meetingID: meeting.id,
          clusterLabel: "Speaker \(index + 2)", assignment: .confirmed(personID: ben.id),
          embedding: embedding(1), clusterConfidence: 0.9))
    }
    try await store.replaceTranscript(meeting, segments: [], speakers: speakers)
    let memory = CosineSpeakerMemory(store: store)

    try await store.mergePersons(keep: anna.id, remove: ben.id)

    let ranked = try await memory.candidates(for: embedding(1), limit: 10)
    #expect(ranked.map(\.person.id) == [anna.id], "Ben is gone")
    let kept = try #require(try await store.person(id: anna.id))
    #expect(kept.sampleCount == 5)
    let values = try #require(kept.embedding?.values)
    #expect(abs(values[1] / values[0] - 4) < 1e-4, "four samples of Ben against one of Anna")
    #expect(ranked[0].similarity > 0.9, "Ben's voice now matches Anna")
  }

}
