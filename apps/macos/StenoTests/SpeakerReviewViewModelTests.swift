import StenoCore
import XCTest

@MainActor
final class SpeakerReviewViewModelTests: XCTestCase {
  private func makeModel(_ environment: AppEnvironment) async throws -> SpeakerReviewViewModel {
    let export = try await environment.store.export(meetingID: SampleData.meetingID)
    let model = SpeakerReviewViewModel(export: export, environment: environment)
    await model.load()
    return model
  }

  func testUnresolvedCardsAndCandidates() async throws {
    let environment = try await TestSupport.environment()
    let model = try await makeModel(environment)
    XCTAssertEqual(model.unresolved.map(\.speaker.clusterLabel), ["Speaker 2"])
    XCTAssertEqual(model.attendees.map(\.displayName), ["Jérôme"])
    let card = try XCTUnwrap(model.unresolved.first)
    XCTAssertFalse(card.candidates.isEmpty, "cosine candidates over the store's people")
    let suggestion = try XCTUnwrap(model.suggestion(for: card))
    XCTAssertEqual(suggestion.person.id, SampleData.personJeromeID)
    XCTAssertEqual(suggestion.similarity, 0.72)
  }

  func testNamingCreatesAPersonAndConfirms() async throws {
    let environment = try await TestSupport.environment()
    let model = try await makeModel(environment)
    await model.name(SampleData.speakerTwoID, "  Anna ")
    XCTAssertNil(model.error, model.error ?? "")
    XCTAssertTrue(model.isDone)
    let speakers = try await environment.store.speakers(meetingID: SampleData.meetingID)
    let speaker = try XCTUnwrap(speakers.first { $0.id == SampleData.speakerTwoID })
    XCTAssertTrue(speaker.assignment.isConfirmed)
    let personOptional = try await environment.store.person(id: speaker.personID!)
    let person = try XCTUnwrap(personOptional)
    XCTAssertEqual(person.displayName, "Anna")
    XCTAssertEqual(person.sampleCount, 1, "confirm enrolled the embedding")
  }

  func testNamingAnExistingNameReusesThePerson() async throws {
    let environment = try await TestSupport.environment()
    let model = try await makeModel(environment)
    await model.name(SampleData.speakerTwoID, "jérôme")
    let speakers = try await environment.store.speakers(meetingID: SampleData.meetingID)
    XCTAssertEqual(
      speakers.first { $0.id == SampleData.speakerTwoID }?.personID, SampleData.personJeromeID)
    let people = try await environment.store.persons()
    XCTAssertEqual(people.count, 2, "no third person")
  }

  func testAcceptSuggestionConfirmsTheSuggestedPerson() async throws {
    let environment = try await TestSupport.environment()
    let model = try await makeModel(environment)
    await model.acceptSuggestion(SampleData.speakerTwoID)
    let speakers = try await environment.store.speakers(meetingID: SampleData.meetingID)
    XCTAssertEqual(
      speakers.first { $0.id == SampleData.speakerTwoID }?.assignment,
      .confirmed(personID: SampleData.personJeromeID))
  }

  func testAssignAttendeeCreatesPersonWithEmail() async throws {
    let environment = try await TestSupport.environment()
    // Rename the attendee so no existing person matches.
    var participant = SampleData.participants()[0]
    participant.displayName = "Maya"
    participant.email = "maya@example.com"
    try await environment.store.save(participant)
    let model = try await makeModel(environment)
    let attendee = try XCTUnwrap(model.attendees.first { $0.displayName == "Maya" })
    await model.assign(SampleData.speakerTwoID, attendee: attendee)
    let speakers = try await environment.store.speakers(meetingID: SampleData.meetingID)
    let personID = try XCTUnwrap(speakers.first { $0.id == SampleData.speakerTwoID }?.personID)
    let personOptional = try await environment.store.person(id: personID)
    let person = try XCTUnwrap(personOptional)
    XCTAssertEqual(person.displayName, "Maya")
    XCTAssertEqual(person.email, "maya@example.com")
  }

  func testSkipLeavesTheAssignmentUnchanged() async throws {
    let environment = try await TestSupport.environment()
    let model = try await makeModel(environment)
    model.skip(SampleData.speakerTwoID)
    XCTAssertTrue(model.unresolved.isEmpty)
    XCTAssertEqual(model.cards.count, 1, "the card still exists")
    let speakers = try await environment.store.speakers(meetingID: SampleData.meetingID)
    XCTAssertEqual(
      speakers.first { $0.id == SampleData.speakerTwoID }?.assignment,
      .suggested(personID: SampleData.personJeromeID, similarity: 0.72))
  }

  func testMergeSpeakersMovesSegmentsAndDeletesTheSource() async throws {
    let environment = try await TestSupport.environment()
    let model = try await makeModel(environment)
    await model.mergeSpeakers(SampleData.speakerTwoID, into: SampleData.speakerOneID)
    XCTAssertNil(model.error, model.error ?? "")
    let export = try await environment.store.export(meetingID: SampleData.meetingID)
    XCTAssertEqual(export.speakers.map(\.id), [SampleData.speakerOneID])
    XCTAssertEqual(
      export.segments.filter { $0.speakerID == SampleData.speakerOneID }.count, 2,
      "the merged cluster's segment moved")
    XCTAssertTrue(model.isDone)
    XCTAssertEqual(model.allSpeakers.count, 1)
  }

  func testMergePersonsRepointsSpeakers() async throws {
    let environment = try await TestSupport.environment()
    let model = try await makeModel(environment)
    await model.mergePersons(keep: SampleData.personNicolaiID, remove: SampleData.personJeromeID)
    XCTAssertNil(model.error, model.error ?? "")
    let remaining = try await environment.store.persons()
    XCTAssertEqual(remaining.map(\.id), [SampleData.personNicolaiID])
    let speakers = try await environment.store.speakers(meetingID: SampleData.meetingID)
    XCTAssertEqual(
      speakers.first { $0.id == SampleData.speakerTwoID }?.personID, SampleData.personNicolaiID)
  }

  func testMergeWithItselfIsANoOp() async throws {
    let environment = try await TestSupport.environment()
    let model = try await makeModel(environment)
    await model.mergeSpeakers(SampleData.speakerTwoID, into: SampleData.speakerTwoID)
    await model.mergePersons(keep: SampleData.personJeromeID, remove: SampleData.personJeromeID)
    XCTAssertEqual(model.allSpeakers.count, 2)
    let people = try await environment.store.persons()
    XCTAssertEqual(people.count, 2)
  }

  func testFinishRedeliversExactlyOnce() async throws {
    let environment = try await TestSupport.environment()
    let model = try await makeModel(environment)
    await model.finish()
    await model.finish()
    XCTAssertEqual(model.redeliveries, 1)
    XCTAssertTrue(model.finished)
  }

  func testPlayingAMissingClipReportsAnError() async throws {
    let environment = try await TestSupport.environment()
    let model = try await makeModel(environment)
    model.play(SampleData.speakerTwoID)
    XCTAssertNotNil(model.error)
    XCTAssertNil(model.playing)
  }
}
