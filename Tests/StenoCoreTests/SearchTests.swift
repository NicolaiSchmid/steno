import Foundation
import Testing

@testable import StenoCore

@Suite struct SearchTests {
  static func store() async throws -> MeetingStore {
    let store = try MeetingStore.inMemory()
    var meeting = SampleData.meeting()
    meeting.summary = nil
    try await store.save(meeting)
    for person in SampleData.persons() { try await store.save(person) }
    let segments = [
      TranscriptSegment(
        id: SampleData.uuid(40), meetingID: SampleData.meetingID, start: 0, end: 2, lane: .mixed,
        text: "Jérôme prüft das Budget.", rawText: "jerome prüft das budget"),
      TranscriptSegment(
        id: SampleData.uuid(41), meetingID: SampleData.meetingID, start: 2, end: 4, lane: .mixed,
        text: "Das Budget ist knapp, sagt Jérôme, sehr knapp.", rawText: "das budget ist knapp"),
      TranscriptSegment(
        id: SampleData.uuid(42), meetingID: SampleData.meetingID, start: 4, end: 6, lane: .mixed,
        text: "Wir vertagen den Zeitplan.", rawText: "wir vertagen den zeitplan"),
    ]
    try await store.replaceTranscript(
      meetingID: SampleData.meetingID, segments: segments, speakers: [])
    return store
  }

  @Test func diacriticsAndCaseAreFolded() async throws {
    let store = try await Self.store()
    let hits = try await store.search("jerome")
    #expect(hits.map(\.segmentID) == [SampleData.uuid(40), SampleData.uuid(41)])
    #expect(hits.first?.snippet.contains("[Jérôme]") == true)
    #expect(try await store.search("JÉRÔME").count == 2)
  }

  @Test func allTokensMustMatch() async throws {
    let store = try await Self.store()
    #expect(try await store.search("Budget knapp").map(\.segmentID) == [SampleData.uuid(41)])
    #expect(try await store.search("Budget Zeitplan").isEmpty)
    #expect(try await store.search("").isEmpty)
    #expect(try await store.search("   ").isEmpty)
  }

  @Test func meetingTitleAndSummaryAreSearchable() async throws {
    let store = try await Self.store()
    let title = try await store.search("Produktstrategie")
    #expect(title.map(\.meetingID) == [SampleData.meetingID])
    #expect(title.first?.segmentID == nil)
    #expect(title.first?.snippet.contains("[Produktstrategie]") == true)
    try await store.replaceSummary(
      meetingID: SampleData.meetingID, output: SampleData.summaryOutput(), templateID: "default")
    let summary = try await store.search("Oktober")
    #expect(summary.map(\.segmentID) == [nil])
  }

  @Test func limitAndRankOrder() async throws {
    let store = try await Self.store()
    let hits = try await store.search("Budget", limit: 1)
    #expect(hits.count == 1)
    let all = try await store.search("Budget")
    #expect(all.count == 2)
    #expect(all[0].rank <= all[1].rank)
    #expect(all.map(\.segmentID).contains(SampleData.uuid(40)))
    #expect(all.map(\.segmentID).contains(SampleData.uuid(41)))
  }
}
