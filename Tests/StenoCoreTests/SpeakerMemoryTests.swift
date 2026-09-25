import Foundation
import Testing

@testable import StenoCore

/// The provided `SpeakerMemory.match` (threshold plus a 0.05 margin over the
/// runner-up) and the in-memory fake's running-mean enrol.
@Suite struct SpeakerMemoryTests {
  static func tilted(_ first: Float, _ second: Float) -> Embedding {
    var values = [Float](repeating: 0, count: Embedding.dimension)
    values[0] = first
    values[1] = second
    return Embedding(values)
  }

  @Test func matchNeedsTheThresholdAndAMarginOverTheRunnerUp() async throws {
    let memory = InMemorySpeakerMemory(people: SampleData.persons())

    let clear = try #require(try await memory.match(Self.tilted(0.8, 0.6), threshold: 0.6))
    #expect(clear.person.id == SampleData.personNicolaiID)
    #expect(abs(clear.similarity - 0.8) < 0.0001)

    // 0.717 for Nicolai against 0.697 for Jérôme: both above the threshold,
    // less than 0.05 apart, so nobody is suggested.
    let close = Self.tilted(0.72, 0.70)
    #expect(try await memory.match(close, threshold: 0.6) == nil)
    #expect(
      try await memory.match(close, threshold: 0.6, margin: 0.01)?.person.id
        == SampleData.personNicolaiID)

    #expect(try await memory.match(Self.tilted(0.8, 0.6), threshold: 0.81) == nil)
    #expect(try await memory.match(SampleData.embedding(axis: 5), threshold: 0.6) == nil)
    #expect(
      try await InMemorySpeakerMemory().match(SampleData.embedding(axis: 0), threshold: 0)
        == nil)

    let ranked = try await memory.candidates(for: Self.tilted(0.6, 0.8), limit: 5)
    #expect(ranked.map(\.person.id) == [SampleData.personJeromeID, SampleData.personNicolaiID])
    #expect(try await memory.candidates(for: Self.tilted(0.6, 0.8), limit: 1).count == 1)
  }

  @Test func enrolFoldsIntoARunningMeanWithACappedSampleCount() async throws {
    let memory = InMemorySpeakerMemory(people: SampleData.persons(), maxSamples: 2)
    let jerome = SampleData.persons()[0]
    try await memory.enroll(SampleData.embedding(axis: 0), as: jerome)
    var updated = try #require(await memory.people[jerome.id])
    #expect(updated.sampleCount == 2)
    var embedding = try #require(updated.embedding)
    #expect(abs(embedding.values[0] - 0.7071) < 0.001)
    #expect(abs(embedding.values[1] - 0.7071) < 0.001)

    // Nicolai has three samples but the cap weighs the known mean as two.
    let nicolai = SampleData.persons()[1]
    try await memory.enroll(SampleData.embedding(axis: 1), as: nicolai)
    updated = try #require(await memory.people[nicolai.id])
    #expect(updated.sampleCount == 3)
    embedding = try #require(updated.embedding)
    #expect(abs(embedding.values[0] - 0.8944) < 0.001)
    #expect(abs(embedding.values[1] - 0.4472) < 0.001)

    let newcomer = Person(
      id: SampleData.uuid(12), displayName: "Anna", createdAt: SampleData.createdAt)
    try await memory.enroll(Embedding([3, 4]), as: newcomer)
    updated = try #require(await memory.people[newcomer.id])
    #expect(updated.sampleCount == 1)
    #expect(updated.embedding == Embedding([0.6, 0.8]))
    #expect(await memory.enrolments.map(\.personID) == [jerome.id, nicolai.id, newcomer.id])
  }
}
