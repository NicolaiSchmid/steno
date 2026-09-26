import Foundation
import Testing

@testable import StenoCore

/// `speakerNameSuggestion` rows (#78): written with the summary, one per
/// speaker, gone with a confirmation, the speaker or the meeting.
@Suite struct SpeakerNameSuggestionTests {
  static let jerome = SpeakerNameSuggestion(
    speakerID: SampleData.speakerTwoID, name: "Jérôme", confidence: 0.8,
    evidence: "Speaker 1 spricht Speaker 2 mit Jérôme an.")

  @Test func replaceSummaryWritesTheSuggestionsThatNameAKnownSpeaker() async throws {
    let store = try await MeetingStoreTests.populated()
    #expect(try await store.nameSuggestions(meetingID: SampleData.meetingID).isEmpty)
    let weaker = SpeakerNameSuggestion(
      speakerID: SampleData.speakerTwoID, name: "Jerome", confidence: 0.4, evidence: "weaker")
    let nameless = SpeakerNameSuggestion(
      speakerID: SampleData.speakerOneID, name: nil, confidence: 0.9, evidence: "no evidence")
    let blank = SpeakerNameSuggestion(
      speakerID: SampleData.speakerOneID, name: "  ", confidence: 0.9, evidence: "blank")
    let stranger = SpeakerNameSuggestion(
      speakerID: SampleData.uuid(999), name: "Nobody", confidence: 1, evidence: "not a speaker")

    try await store.replaceSummary(
      SampleData.meeting(), tasks: [], decisions: [],
      speakerNames: [Self.jerome, weaker, nameless, blank, stranger])

    #expect(try await store.nameSuggestions(meetingID: SampleData.meetingID) == [Self.jerome])
    #expect(try await store.nameSuggestions(meetingID: SampleData.uuid(2)).isEmpty)
  }

  @Test func theNextSummaryReplacesTheSuggestions() async throws {
    let store = try await MeetingStoreTests.populated()
    try await store.replaceSummary(
      SampleData.meeting(), tasks: [], decisions: [], speakerNames: [Self.jerome])
    let nicolai = SpeakerNameSuggestion(
      speakerID: SampleData.speakerOneID, name: "Nicolai", confidence: 0.7, evidence: "intro")
    try await store.replaceSummary(
      SampleData.meeting(), tasks: [], decisions: [], speakerNames: [nicolai])
    #expect(try await store.nameSuggestions(meetingID: SampleData.meetingID) == [nicolai])
    try await store.replaceSummary(SampleData.meeting(), tasks: [], decisions: [])
    #expect(try await store.nameSuggestions(meetingID: SampleData.meetingID).isEmpty)
  }

  @Test func confirmClearsTheConfirmedSpeakersSuggestionOnly() async throws {
    let store = try await MeetingStoreTests.populated()
    let nicolai = SpeakerNameSuggestion(
      speakerID: SampleData.speakerOneID, name: "Nicolai", confidence: 0.7, evidence: "intro")
    try await store.replaceSummary(
      SampleData.meeting(), tasks: [], decisions: [], speakerNames: [Self.jerome, nicolai])
    let memory = InMemorySpeakerMemory(people: SampleData.persons())

    try await store.confirm(
      speakerID: SampleData.speakerTwoID, person: SampleData.persons()[0], memory: memory)

    #expect(try await store.nameSuggestions(meetingID: SampleData.meetingID) == [nicolai])
  }

  @Test func suggestionsGoWithTheSpeakerAndTheMeeting() async throws {
    let store = try await MeetingStoreTests.populated()
    let nicolai = SpeakerNameSuggestion(
      speakerID: SampleData.speakerOneID, name: "Nicolai", confidence: 0.7, evidence: "intro")
    try await store.replaceSummary(
      SampleData.meeting(), tasks: [], decisions: [], speakerNames: [Self.jerome, nicolai])

    try await store.mergeSpeakers(
      SampleData.speakerTwoID, into: SampleData.speakerOneID, meetingID: SampleData.meetingID)
    #expect(try await store.nameSuggestions(meetingID: SampleData.meetingID) == [nicolai])

    try await store.replaceTranscript(SampleData.meeting(), segments: [], speakers: [])
    #expect(try await store.nameSuggestions(meetingID: SampleData.meetingID).isEmpty)

    try await store.replaceTranscript(
      SampleData.meeting(), segments: [], speakers: SampleData.speakers())
    try await store.replaceSummary(
      SampleData.meeting(), tasks: [], decisions: [], speakerNames: [Self.jerome])
    try await store.delete(meetingID: SampleData.meetingID)
    let remaining = try await store.writer.read { db in
      try Int.fetchOne(db, sql: "SELECT count(*) FROM speakerNameSuggestion")
    }
    #expect(remaining == 0)
  }
}
