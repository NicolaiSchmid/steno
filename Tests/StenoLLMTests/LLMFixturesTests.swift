import Foundation
import StenoCore
import Testing

@testable import StenoLLM

/// The committed `llm/transcripts/*.json` files are the generators' values.
/// Compared as decoded values, not bytes, because Darwin and Linux Foundation
/// may print the same Double differently; `STENO_UPDATE_SNAPSHOTS=1`
/// rewrites the files.
@Suite struct LLMFixturesTests {
  static func assertFixture(_ export: MeetingExport, at relative: String) throws {
    if ProcessInfo.processInfo.environment[Snapshot.updateEnvironmentKey] == "1" {
      try Snapshot.assert(try StenoJSON.encode(export), matches: relative)
    }
    let decoded = try StenoJSON.decode(MeetingExport.self, from: Fixtures.data(relative))
    #expect(decoded == export, "\(relative)")
  }

  @Test func denglishStandupMatchesItsFile() throws {
    let export = LLMFixtures.denglishStandup()
    try Self.assertFixture(export, at: "llm/transcripts/denglish-standup.json")
    #expect(export.segments.count == 24)
    #expect(export.speakers.map(\.clusterLabel) == ["Speaker 1", "Speaker 2", "Speaker 3"])
    #expect(Set(export.segments.compactMap(\.speakerID)).count == 3)
    #expect(export.segments.allSatisfy { $0.text == $0.rawText })
    #expect(export.segments.contains { $0.text.contains("git hub") })
    #expect(export.segments.contains { $0.text.contains("kuber netes") })
    #expect(export.meeting.language == "de")
    #expect(export.meeting.templateID == "daily-standup")
    #expect(export.participants.map(\.displayName) == ["Mara", "Jérôme", "Nicolai"])
    for (previous, segment) in zip(export.segments, export.segments.dropFirst()) {
      #expect(segment.start == previous.end)
    }
  }

  @Test func customerCallMatchesItsFileAndIsAnHourLong() throws {
    let export = LLMFixtures.customerCall60min()
    try Self.assertFixture(export, at: "llm/transcripts/customer-call-60min.json")
    #expect(export.segments.count == 900)
    #expect(export.segments.last?.end == 3_600 - 0.25)
    #expect(export.meeting.duration == 3_600)
    #expect(export.speakers.map(\.clusterLabel) == ["Me", "Speaker 1", "Speaker 2"])
    #expect(Set(export.segments.compactMap(\.speakerID)).count == 3)
    #expect(Set(export.segments.map(\.id)).count == 900, "unique segment ids")
    let byLane = Dictionary(grouping: export.segments, by: \.lane)
    #expect(byLane[.mic]?.allSatisfy { $0.speakerID == LLMFixtures.callSpeakerMeID } == true)
    #expect(byLane[.system]?.allSatisfy { $0.speakerID != LLMFixtures.callSpeakerMeID } == true)
  }

  @Test func generatorIsDeterministic() {
    let first = LLMFixtures.customerCall60min()
    let second = LLMFixtures.customerCall60min()
    #expect(first == second)
    let otherSeed = SyntheticTranscript.generate(
      meetingID: LLMFixtures.callMeetingID, seed: 1,
      speakers: [.init(id: LLMFixtures.callSpeakerMeID, lane: .mic, role: .vendor, weight: 1)],
      segmentCount: 10, segmentSeconds: 4)
    #expect(otherSeed.count == 10)
    #expect(otherSeed.map(\.text) != Array(first.segments.prefix(10).map(\.text)))
  }

  @Test func stageInputsComeFromTheExport() {
    let export = LLMFixtures.denglishStandup()
    let cleanup = CleanupInput(export: export)
    #expect(cleanup.segments == export.segments)
    #expect(cleanup.language == "de")
    #expect(cleanup.participants == export.participants)
    #expect(cleanup.speakers == export.speakers)
    #expect(cleanup.knownPeople == export.persons)
    let summary = SummaryInput(export: export)
    #expect(summary.template.id == "daily-standup")
    #expect(summary.meeting == export.meeting)
    let other = SummaryInput(export: export, template: SummaryTemplate.bundled(id: "interview"))
    #expect(other.template.id == "interview")
  }
}
