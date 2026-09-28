import StenoCore
import XCTest

@MainActor
final class SpeakersViewModelTests: XCTestCase {
  private func makeModel(_ environment: AppEnvironment) async throws -> SpeakersViewModel {
    let model = SpeakersViewModel(environment: environment)
    try await refresh(model, environment)
    await TestSupport.waitUntil("people loaded") { !model.persons.isEmpty }
    return model
  }

  /// What the store observation does in the app: hand the model the current
  /// export.
  private func refresh(_ model: SpeakersViewModel, _ environment: AppEnvironment) async throws {
    model.update(export: try await environment.store.export(meetingID: SampleData.meetingID))
  }

  private func person(_ option: SpeakerOptions.Option) -> Person? {
    if case .person(let person) = option.kind { return person }
    return nil
  }

  private func createText(_ option: SpeakerOptions.Option) -> String? {
    if case .create(let text) = option.kind { return text }
    return nil
  }

  func testRowsListEverySpeakerInClusterOrder() async throws {
    let environment = try await TestSupport.environment()
    let model = try await makeModel(environment)
    XCTAssertEqual(model.rows.map(\.speaker.clusterLabel), ["Speaker 1", "Speaker 2"])
    let first = try XCTUnwrap(model.rows.first)
    XCTAssertTrue(first.isConfirmed)
    XCTAssertEqual(first.displayName, "Nicolai")
    XCTAssertEqual(first.excerpt, "", "confirmed rows show no excerpt")
    let second = try XCTUnwrap(model.rows.last)
    XCTAssertFalse(second.isConfirmed)
    XCTAssertEqual(second.displayName, "Jérôme", "a suggested speaker reads as the person")
    XCTAssertTrue(
      second.excerpt.hasSuffix("Ich prüfe das Budget bis Freitag."), second.excerpt)
    XCTAssertFalse(second.canPlay, "the sample clip file does not exist")
    XCTAssertEqual(model.unconfirmedCount, 1)
  }

  func testOptionsStartWithTheVoiceMatchAndNeverListTheOwnPerson() async throws {
    let environment = try await TestSupport.environment()
    let model = try await makeModel(environment)
    let options = model.options(for: SampleData.speakerTwoID, query: "")
    XCTAssertEqual(options.first.flatMap(person)?.id, SampleData.personJeromeID)
    XCTAssertEqual(options.first?.tag, .soundsLike)
    XCTAssertTrue(
      options.contains { person($0)?.id == SampleData.personNicolaiID && $0.tag == .inThisMeeting })
    XCTAssertFalse(options.contains { createText($0) != nil }, "no create row without a query")

    let own = model.options(for: SampleData.speakerOneID, query: "")
    XCTAssertFalse(own.contains { person($0)?.id == SampleData.personNicolaiID })
    XCTAssertEqual(model.options(for: UUID(), query: ""), [])
  }

  func testPrefillIsTheSuggestedName() async throws {
    let environment = try await TestSupport.environment()
    let model = try await makeModel(environment)
    XCTAssertEqual(model.prefill(for: SampleData.speakerTwoID), "Jérôme")
    XCTAssertNil(model.prefill(for: SampleData.speakerOneID), "a confirmed speaker has none")
  }

  func testSelectingAPersonConfirmsAndRowsFollowTheExport() async throws {
    let environment = try await TestSupport.environment()
    let model = try await makeModel(environment)
    var writes = 0
    model.onWrite = { writes += 1 }

    await model.select(
      SpeakerOptions.Option(kind: .person(SampleData.persons()[0]), tag: .soundsLike),
      for: SampleData.speakerTwoID)

    XCTAssertNil(model.error, model.error ?? "")
    XCTAssertEqual(writes, 1)
    let speakers = try await environment.store.speakers(meetingID: SampleData.meetingID)
    XCTAssertEqual(
      speakers.first { $0.id == SampleData.speakerTwoID }?.assignment,
      .confirmed(personID: SampleData.personJeromeID))
    XCTAssertFalse(model.rows.last?.isConfirmed ?? true, "rows wait for the export")
    try await refresh(model, environment)
    XCTAssertTrue(model.rows.last?.isConfirmed ?? false)
    XCTAssertEqual(model.unconfirmedCount, 0)
  }

  func testSelectingCreateNamesANewPersonAndReusesAnExistingName() async throws {
    let environment = try await TestSupport.environment()
    let model = try await makeModel(environment)

    await model.select(
      SpeakerOptions.Option(kind: .create("  Anna ")), for: SampleData.speakerTwoID)

    XCTAssertNil(model.error, model.error ?? "")
    var speakers = try await environment.store.speakers(meetingID: SampleData.meetingID)
    let annaID = try XCTUnwrap(speakers.first { $0.id == SampleData.speakerTwoID }?.personID)
    let anna = try XCTUnwrap(try await environment.store.person(id: annaID))
    XCTAssertEqual(anna.displayName, "Anna")
    XCTAssertEqual(anna.sampleCount, 1, "the voice is the speaker's embedding")
    XCTAssertEqual(try await environment.store.persons().count, 3)

    try await refresh(model, environment)
    await model.select(SpeakerOptions.Option(kind: .create("jérôme")), for: SampleData.speakerTwoID)
    speakers = try await environment.store.speakers(meetingID: SampleData.meetingID)
    XCTAssertEqual(
      speakers.first { $0.id == SampleData.speakerTwoID }?.personID, SampleData.personJeromeID,
      "an existing name, ignoring case and accents, reuses the person")
    XCTAssertEqual(try await environment.store.persons().count, 3, "no fourth person")
  }

  func testSelectingTheOwnPersonIsANoOp() async throws {
    let environment = try await TestSupport.environment()
    let model = try await makeModel(environment)
    var writes = 0
    model.onWrite = { writes += 1 }
    let before = try await environment.store.speakers(meetingID: SampleData.meetingID)

    await model.select(
      SpeakerOptions.Option(kind: .person(SampleData.persons()[1])), for: SampleData.speakerOneID)

    XCTAssertEqual(writes, 0)
    XCTAssertEqual(try await environment.store.speakers(meetingID: SampleData.meetingID), before)
  }

  func testSelectingAPersonWhoOwnsAnotherSpeakerMerges() async throws {
    let environment = try await TestSupport.environment()
    let model = try await makeModel(environment)

    await model.select(
      SpeakerOptions.Option(kind: .person(SampleData.persons()[1]), tag: .inThisMeeting),
      for: SampleData.speakerTwoID)

    XCTAssertNil(model.error, model.error ?? "")
    let export = try await environment.store.export(meetingID: SampleData.meetingID)
    XCTAssertEqual(export.speakers.map(\.id), [SampleData.speakerOneID], "merged into Speaker 1")
    XCTAssertEqual(
      export.segments.compactMap(\.speakerID), [SampleData.speakerOneID, SampleData.speakerOneID])
    try await refresh(model, environment)
    XCTAssertEqual(model.rows.count, 1)
  }

  func testACreateOptionForAnAttendeeTakesTheEmail() async throws {
    let environment = try await TestSupport.environment()
    var participant = SampleData.participants()[0]
    participant.displayName = "Maya"
    participant.email = "maya@example.com"
    try await environment.store.save(participant)
    let model = try await makeModel(environment)

    let options = model.options(for: SampleData.speakerTwoID, query: "")
    let maya = try XCTUnwrap(options.first { createText($0) == "Maya" })
    XCTAssertEqual(maya.tag, .attendee)

    await model.select(maya, for: SampleData.speakerTwoID)
    XCTAssertNil(model.error, model.error ?? "")
    let speakers = try await environment.store.speakers(meetingID: SampleData.meetingID)
    let personID = try XCTUnwrap(speakers.first { $0.id == SampleData.speakerTwoID }?.personID)
    let person = try XCTUnwrap(try await environment.store.person(id: personID))
    XCTAssertEqual(person.displayName, "Maya")
    XCTAssertEqual(person.email, "maya@example.com")
  }

  func testBuildingOptionsWritesNothing() async throws {
    let environment = try await TestSupport.environment()
    let model = try await makeModel(environment)
    let before = try await environment.store.speakers(meetingID: SampleData.meetingID)
    _ = model.options(for: SampleData.speakerTwoID, query: "an")
    _ = model.options(for: SampleData.speakerOneID, query: "")
    XCTAssertEqual(try await environment.store.speakers(meetingID: SampleData.meetingID), before)
    XCTAssertEqual(try await environment.store.persons().count, 2)
  }

  /// The LLM's persisted guess is an option tagged "Mentioned" and applies
  /// nothing by itself; a blank guess shows nothing.
  func testLLMNameSuggestionIsAMentionedOption() async throws {
    let environment = try await TestSupport.environment()
    let meeting = try XCTUnwrap(try await environment.store.meeting(id: SampleData.meetingID))
    try await environment.store.replaceSummary(
      meeting, tasks: SampleData.tasks(), decisions: SampleData.decisions().map(\.text),
      speakerNames: [
        SpeakerNameSuggestion(
          speakerID: SampleData.speakerTwoID, name: "Anna", confidence: 0.8,
          evidence: "Nicolai says 'danke, Anna' at 00:02"),
        SpeakerNameSuggestion(
          speakerID: SampleData.speakerOneID, name: nil, confidence: 0, evidence: ""),
      ])
    let model = try await makeModel(environment)
    await TestSupport.waitUntil("suggestion loaded") { !model.suggestions.isEmpty }

    let options = model.options(for: SampleData.speakerTwoID, query: "")
    let anna = try XCTUnwrap(options.first { createText($0) == "Anna" })
    XCTAssertEqual(anna.tag, .mentioned)
    let speakers = try await environment.store.speakers(meetingID: SampleData.meetingID)
    XCTAssertEqual(
      speakers.first { $0.id == SampleData.speakerTwoID }?.assignment,
      .suggested(personID: SampleData.personJeromeID, similarity: 0.72),
      "listing the guess applies nothing")
    XCTAssertFalse(
      model.options(for: SampleData.speakerOneID, query: "").contains { $0.tag == .mentioned })
  }

  func testBlankCreateAndUnknownSpeakersAreIgnored() async throws {
    let environment = try await TestSupport.environment()
    let model = try await makeModel(environment)
    var writes = 0
    model.onWrite = { writes += 1 }

    await model.select(SpeakerOptions.Option(kind: .create("   ")), for: SampleData.speakerTwoID)
    XCTAssertNotNil(model.error, "a blank name is reported, not created")
    await model.select(SpeakerOptions.Option(kind: .create("Ghost")), for: UUID())

    XCTAssertEqual(writes, 0)
    XCTAssertEqual(try await environment.store.persons().count, 2, "no person was created")
  }

  func testPlayWithoutAClipFileReportsAnError() async throws {
    let environment = try await TestSupport.environment()
    let model = try await makeModel(environment)
    XCTAssertFalse(model.rows.last?.canPlay ?? true)
    model.play(SampleData.speakerTwoID)
    XCTAssertNotNil(model.error)
    XCTAssertNil(model.playing)
  }
}
