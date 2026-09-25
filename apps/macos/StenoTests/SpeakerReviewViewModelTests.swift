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
    let personID = try XCTUnwrap(speaker.personID)
    let personOptional = try await environment.store.person(id: personID)
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

  func testMergeWithItselfIsANoOp() async throws {
    let environment = try await TestSupport.environment()
    let model = try await makeModel(environment)
    await model.mergeSpeakers(SampleData.speakerTwoID, into: SampleData.speakerTwoID)
    XCTAssertEqual(model.allSpeakers.count, 2)
  }

  /// Done on an untouched sheet leaves the vault alone; after a confirm it
  /// re-exports once, and a second Done does nothing more.
  func testFinishRedeliversOnlyAfterAChangeAndOnlyOnce() async throws {
    let environment = try await TestSupport.environment()
    let vault = try TestSupport.temporaryDirectory("steno-vault")
    defer { try? FileManager.default.removeItem(at: vault) }
    try await environment.updateSettings {
      $0.obsidian = ObsidianSettings(
        vaultPath: vault.path, peopleFolder: nil, includeAudio: false, taskTag: nil)
    }

    let untouched = try await makeModel(environment)
    XCTAssertFalse(untouched.didChange)
    await untouched.finish()
    var deliveries = try await environment.store.deliveries(meetingID: SampleData.meetingID)
    XCTAssertTrue(deliveries.isEmpty, "nothing changed, nothing re-exported")

    let model = try await makeModel(environment)
    await model.acceptSuggestion(SampleData.speakerTwoID)
    XCTAssertTrue(model.didChange)
    await model.finish()
    await model.finish()
    XCTAssertNil(model.error, model.error ?? "")
    deliveries = try await environment.store.deliveries(meetingID: SampleData.meetingID)
    XCTAssertEqual(deliveries.map(\.destinationID), ["obsidian-folder"], "one delivery row")
    XCTAssertEqual(deliveries.first?.status, .delivered)
    XCTAssertTrue(
      FileManager.default.fileExists(atPath: vault.appendingPathComponent("Meetings").path))
  }

  /// The merge picker starts with no target, so Merge without a choice is a
  /// no-op; a chosen target merges and the choice is dropped with the card.
  func testMergeNeedsAChosenTarget() async throws {
    let environment = try await TestSupport.environment()
    let model = try await makeModel(environment)
    let card = try XCTUnwrap(model.unresolved.first)
    XCTAssertNil(model.mergeTargets[card.id], "no default target")
    XCTAssertEqual(model.mergeCandidates(for: card).map(\.id), [SampleData.speakerOneID])
    await model.mergeIntoChosenTarget(card.id)
    XCTAssertEqual(model.allSpeakers.count, 2, "nothing merged without a choice")
    XCTAssertFalse(model.didChange)

    model.mergeTargets[card.id] = SampleData.speakerOneID
    await model.mergeIntoChosenTarget(card.id)
    XCTAssertEqual(model.allSpeakers.map(\.id), [SampleData.speakerOneID])
    XCTAssertTrue(model.didChange)
    XCTAssertTrue(model.mergeTargets.isEmpty, "the merged card's choice is gone")
  }

  func testPlayingAMissingClipReportsAnError() async throws {
    let environment = try await TestSupport.environment()
    let model = try await makeModel(environment)
    model.play(SampleData.speakerTwoID)
    XCTAssertNotNil(model.error)
    XCTAssertNil(model.playing)
  }

  func testCandidatesAreRankedBestFirst() async throws {
    let environment = try await TestSupport.environment()
    let model = try await makeModel(environment)
    let card = try XCTUnwrap(model.unresolved.first)
    // Speaker 2's embedding is the unit vector on axis 1, Jérôme's too;
    // Nicolai's is orthogonal.
    XCTAssertEqual(
      card.candidates.map(\.person.id), [SampleData.personJeromeID, SampleData.personNicolaiID])
    let similarities = card.candidates.map(\.similarity)
    XCTAssertEqual(similarities, similarities.sorted(by: >), "best first")
    XCTAssertEqual(similarities.first ?? 0, 1, accuracy: 0.001)
    XCTAssertEqual(similarities.last ?? 1, 0, accuracy: 0.001)
    XCTAssertEqual(
      model.displayName(card.speaker), "Jérôme", "a suggested speaker reads as the person")
  }

  func testConfirmEnrolsExactlyOnceAndFinishNeverEnrols() async throws {
    let environment = try await TestSupport.environment()
    let model = try await makeModel(environment)
    let before = try await environment.store.person(id: SampleData.personJeromeID)
    XCTAssertEqual(before?.sampleCount, 1)

    await model.acceptSuggestion(SampleData.speakerTwoID)
    XCTAssertNil(model.error, model.error ?? "")
    var jerome = try await environment.store.person(id: SampleData.personJeromeID)
    XCTAssertEqual(jerome?.sampleCount, 2, "one confirm, one enrolment")

    await model.acceptSuggestion(SampleData.speakerTwoID)
    let person = try XCTUnwrap(jerome)
    await model.assign(SampleData.speakerTwoID, person: person)
    await model.name(SampleData.speakerTwoID, "Jérôme")
    jerome = try await environment.store.person(id: SampleData.personJeromeID)
    XCTAssertEqual(
      jerome?.sampleCount, 2,
      "a confirmed speaker has no card, so the sheet cannot confirm it again")
    XCTAssertNil(model.error, model.error ?? "")

    await model.finish()
    await model.finish()
    jerome = try await environment.store.person(id: SampleData.personJeromeID)
    XCTAssertEqual(jerome?.sampleCount, 2, "finish re-exports, it does not enrol")
    XCTAssertTrue(model.didChange)
  }

  func testConfirmRemovesTheSampleClip() async throws {
    let environment = try await TestSupport.environment()
    let folder = try TestSupport.temporaryDirectory("steno-clips")
    defer { try? FileManager.default.removeItem(at: folder) }
    let clip = folder.appendingPathComponent("speaker-2.wav")
    try Data("RIFF".utf8).write(to: clip)
    var speaker = try XCTUnwrap(SampleData.speakers().first { $0.id == SampleData.speakerTwoID })
    speaker.sampleClipURL = clip
    try await environment.store.save(speaker)

    let model = try await makeModel(environment)
    XCTAssertEqual(model.unresolved.first?.clipURL, clip)
    await model.name(SampleData.speakerTwoID, "Anna")
    XCTAssertNil(model.error, model.error ?? "")
    XCTAssertFalse(FileManager.default.fileExists(atPath: clip.path), "confirm deletes the clip")
    let speakers = try await environment.store.speakers(meetingID: SampleData.meetingID)
    XCTAssertNil(speakers.first { $0.id == SampleData.speakerTwoID }?.sampleClipURL)
  }

  func testBlankNamesAndUnknownSpeakersAreIgnored() async throws {
    let environment = try await TestSupport.environment()
    let model = try await makeModel(environment)
    await model.name(SampleData.speakerTwoID, "   ")
    XCTAssertFalse(model.isDone, "a blank name confirms nothing")
    await model.acceptSuggestion(UUID())
    await model.name(UUID(), "Ghost")
    XCTAssertNil(model.error, "a speaker the sheet does not show is a no-op")
    let people = try await environment.store.persons()
    XCTAssertEqual(people.count, 2, "no person was created for it")
    XCTAssertFalse(model.isDone)
  }
}
