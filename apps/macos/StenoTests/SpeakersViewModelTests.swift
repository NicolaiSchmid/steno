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
    XCTAssertEqual(
      first.excerpt, "…Wir setzen neunzig Prozent auf den Kern.",
      "a confirmed row quotes its clip too")
    let second = try XCTUnwrap(model.rows.last)
    XCTAssertFalse(second.isConfirmed)
    XCTAssertEqual(second.displayName, "Jérôme", "a suggested speaker reads as the person")
    XCTAssertTrue(
      second.excerpt.hasSuffix("Ich prüfe das Budget bis Freitag."), second.excerpt)
    XCTAssertFalse(second.canPlay, "the sample clip file does not exist")
    XCTAssertEqual(second.suggestionStrength, .likely, "0.72 is between the floor and strong")
    XCTAssertNil(first.suggestionStrength)
    XCTAssertEqual(model.unconfirmedCount, 1)
  }

  func testOptionsStartWithTheVoiceMatchAndNeverListTheOwnPerson() async throws {
    let environment = try await TestSupport.environment()
    let model = try await makeModel(environment)
    let options = model.options(for: SampleData.speakerTwoID, query: "")
    XCTAssertEqual(options.first.flatMap(person)?.id, SampleData.personJeromeID)
    XCTAssertEqual(options.first?.tag, .voice(.likely))
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

    try await model.select(
      SpeakerOptions.Option(kind: .person(SampleData.persons()[0]), tag: .voice(.likely)),
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

    try await model.select(
      SpeakerOptions.Option(kind: .create("  Anna ")), for: SampleData.speakerTwoID)

    XCTAssertNil(model.error, model.error ?? "")
    var speakers = try await environment.store.speakers(meetingID: SampleData.meetingID)
    let annaID = try XCTUnwrap(speakers.first { $0.id == SampleData.speakerTwoID }?.personID)
    let annaRow = try await environment.store.person(id: annaID)
    let anna = try XCTUnwrap(annaRow)
    XCTAssertEqual(anna.displayName, "Anna")
    XCTAssertEqual(anna.sampleCount, 1, "the voice is the speaker's embedding")
    let awaited2 = try await environment.store.persons().count
    XCTAssertEqual(awaited2, 3)

    try await refresh(model, environment)
    try await model.select(
      SpeakerOptions.Option(kind: .create("jérôme")), for: SampleData.speakerTwoID)
    speakers = try await environment.store.speakers(meetingID: SampleData.meetingID)
    XCTAssertEqual(
      speakers.first { $0.id == SampleData.speakerTwoID }?.personID, SampleData.personJeromeID,
      "an existing name, ignoring case and accents, reuses the person")
    let awaited3 = try await environment.store.persons().count
    XCTAssertEqual(awaited3, 3, "no fourth person")
  }

  func testSelectingTheOwnPersonIsANoOp() async throws {
    let environment = try await TestSupport.environment()
    let model = try await makeModel(environment)
    var writes = 0
    model.onWrite = { writes += 1 }
    let before = try await environment.store.speakers(meetingID: SampleData.meetingID)

    try await model.select(
      SpeakerOptions.Option(kind: .person(SampleData.persons()[1])), for: SampleData.speakerOneID)

    XCTAssertEqual(writes, 0)
    let awaited4 = try await environment.store.speakers(meetingID: SampleData.meetingID)
    XCTAssertEqual(awaited4, before)
  }

  func testSelectingAPersonWhoOwnsAnotherSpeakerMergesOnlyWithCombine() async throws {
    let environment = try await TestSupport.environment()
    let model = try await makeModel(environment)
    let nicolai = SpeakerOptions.Option(
      kind: .person(SampleData.persons()[1]), tag: .inThisMeeting)
    let before = try await environment.store.speakers(meetingID: SampleData.meetingID)

    do {
      try await model.select(nicolai, for: SampleData.speakerTwoID)
      XCTFail("a pick that would combine is refused without combine")
    } catch let refusal as SpeakersViewModel.SelectError {
      XCTAssertEqual(refusal, .wouldCombine(person: "Nicolai", voice: "Speaker 1"))
      XCTAssertEqual(
        refusal.description, "Nicolai already speaks as Speaker 1 in this meeting.")
    }
    let unchanged = try await environment.store.speakers(meetingID: SampleData.meetingID)
    XCTAssertEqual(unchanged, before, "a refused pick changes nothing")

    try await model.select(nicolai, for: SampleData.speakerTwoID, combine: true)

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
    XCTAssertEqual(
      maya.tag, .lastInvitee, "Speaker 2 is the only open voice and Maya the only invitee")

    try await model.select(maya, for: SampleData.speakerTwoID)
    XCTAssertNil(model.error, model.error ?? "")
    let speakers = try await environment.store.speakers(meetingID: SampleData.meetingID)
    let personID = try XCTUnwrap(speakers.first { $0.id == SampleData.speakerTwoID }?.personID)
    let personRow = try await environment.store.person(id: personID)
    let person = try XCTUnwrap(personRow)
    XCTAssertEqual(person.displayName, "Maya")
    XCTAssertEqual(person.email, "maya@example.com")
  }

  func testBuildingOptionsWritesNothing() async throws {
    let environment = try await TestSupport.environment()
    let model = try await makeModel(environment)
    let before = try await environment.store.speakers(meetingID: SampleData.meetingID)
    _ = model.options(for: SampleData.speakerTwoID, query: "an")
    _ = model.options(for: SampleData.speakerOneID, query: "")
    let awaited6 = try await environment.store.speakers(meetingID: SampleData.meetingID)
    XCTAssertEqual(awaited6, before)
    let awaited7 = try await environment.store.persons().count
    XCTAssertEqual(awaited7, 2)
  }

  /// The LLM's persisted guess is an option tagged "Mentioned" and applies
  /// nothing by itself; a blank guess shows nothing.
  func testLLMNameSuggestionIsAMentionedOption() async throws {
    let environment = try await TestSupport.environment()
    let meetingRow = try await environment.store.meeting(id: SampleData.meetingID)
    let meeting = try XCTUnwrap(meetingRow)
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

    do {
      try await model.select(
        SpeakerOptions.Option(kind: .create("   ")), for: SampleData.speakerTwoID)
      XCTFail("a blank name is reported, not created")
    } catch {}
    try await model.select(SpeakerOptions.Option(kind: .create("Ghost")), for: UUID())

    XCTAssertEqual(writes, 0)
    let awaited9 = try await environment.store.persons().count
    XCTAssertEqual(awaited9, 2, "no person was created")
  }

  func testPlayWithoutAClipFileReportsAnError() async throws {
    let environment = try await TestSupport.environment()
    let model = try await makeModel(environment)
    XCTAssertFalse(model.rows.last?.canPlay ?? true)
    model.play(SampleData.speakerTwoID)
    XCTAssertNotNil(model.error)
    XCTAssertNil(model.playing)
  }

  // MARK: - No, Remove name and Not a person

  private func storedSpeaker(_ id: UUID, _ environment: AppEnvironment) async throws -> Speaker {
    let speakers = try await environment.store.speakers(meetingID: SampleData.meetingID)
    return try XCTUnwrap(speakers.first { $0.id == id })
  }

  /// No to "Sounds like Jérôme": the voice is unnamed, Jérôme is never
  /// suggested for it again (only found when typed), and his voice is
  /// untouched.
  func testRejectingASuggestionRecordsItAndLeavesTheVoice() async throws {
    let environment = try await TestSupport.environment()
    let model = try await makeModel(environment)
    var writes = 0
    model.onWrite = { writes += 1 }
    let jeromeBefore = try await environment.store.person(id: SampleData.personJeromeID)

    try await model.reject(speakerID: SampleData.speakerTwoID, personID: SampleData.personJeromeID)

    XCTAssertEqual(writes, 1)
    let stored = try await storedSpeaker(SampleData.speakerTwoID, environment)
    XCTAssertEqual(stored.assignment, .unknown)
    let jeromeAfter = try await environment.store.person(id: SampleData.personJeromeID)
    XCTAssertEqual(jeromeAfter, jeromeBefore, "a No never touches voice memory")
    try await refresh(model, environment)
    await TestSupport.waitUntil("the No loaded") { !model.rejections.isEmpty }
    XCTAssertNil(model.prefill(for: SampleData.speakerTwoID))
    XCTAssertFalse(
      model.options(for: SampleData.speakerTwoID, query: "").contains {
        person($0)?.id == SampleData.personJeromeID
      }, "never suggested again")
    XCTAssertTrue(
      model.options(for: SampleData.speakerTwoID, query: "jér").contains {
        person($0)?.id == SampleData.personJeromeID
      }, "still found when typed")

    try await model.reject(speakerID: UUID(), personID: SampleData.personJeromeID)
    XCTAssertEqual(writes, 1, "an unlisted speaker is ignored")
    do {
      try await model.reject(speakerID: SampleData.speakerTwoID, personID: UUID())
      XCTFail("a No to a person the store does not know is reported")
    } catch {}
    XCTAssertEqual(writes, 1)
  }

  /// Remove name: the voice is unnamed in this meeting and the person keeps
  /// their name; an unnamed voice again changes nothing.
  func testClearUnnamesTheVoiceOnce() async throws {
    let environment = try await TestSupport.environment()
    let model = try await makeModel(environment)
    var writes = 0
    model.onWrite = { writes += 1 }

    try await model.clear(speakerID: SampleData.speakerOneID)

    XCTAssertEqual(writes, 1)
    let stored = try await storedSpeaker(SampleData.speakerOneID, environment)
    XCTAssertEqual(stored.assignment, .unknown)
    let nicolaiRow = try await environment.store.person(id: SampleData.personNicolaiID)
    let nicolai = try XCTUnwrap(nicolaiRow)
    XCTAssertEqual(nicolai.displayName, "Nicolai")
    XCTAssertEqual(nicolai.sampleCount, 0, "the voice forgets it")
    try await refresh(model, environment)
    XCTAssertEqual(model.row(SampleData.speakerOneID)?.displayName, "Speaker 1")

    try await model.clear(speakerID: SampleData.speakerOneID)
    try await model.clear(speakerID: UUID())
    XCTAssertEqual(writes, 1, "an unnamed or unlisted voice is ignored")
  }

  /// Not a person: the voice leaves the count; Restore brings it back
  /// unnamed; the same answer twice writes once.
  func testDismissAndRestore() async throws {
    let environment = try await TestSupport.environment()
    let model = try await makeModel(environment)
    var writes = 0
    model.onWrite = { writes += 1 }

    try await model.setDismissed(speakerID: SampleData.speakerTwoID, true)
    XCTAssertEqual(writes, 1)
    try await refresh(model, environment)
    let dismissed = try XCTUnwrap(model.row(SampleData.speakerTwoID))
    XCTAssertTrue(dismissed.speaker.dismissed)
    XCTAssertEqual(dismissed.speaker.assignment, .unknown)
    XCTAssertEqual(model.unconfirmedCount, 0, "a dismissed voice is nothing to review")
    try await model.setDismissed(speakerID: SampleData.speakerTwoID, true)
    XCTAssertEqual(writes, 1)

    try await model.setDismissed(speakerID: SampleData.speakerTwoID, false)
    XCTAssertEqual(writes, 2)
    try await refresh(model, environment)
    let restored = try XCTUnwrap(model.row(SampleData.speakerTwoID))
    XCTAssertFalse(restored.speaker.dismissed)
    XCTAssertEqual(restored.speaker.assignment, .unknown)
    XCTAssertEqual(model.unconfirmedCount, 1)

    try await model.setDismissed(speakerID: UUID(), true)
    XCTAssertEqual(writes, 2, "an unlisted speaker is ignored")
  }

  /// A voice match below the floor is not shown: the row reads as unknown,
  /// nothing is pre-filled or offered as the voice, and Remove name has
  /// nothing to remove.
  func testAVoiceMatchBelowTheFloorReadsAsUnknown() async throws {
    let environment = try await TestSupport.environment()
    var weak = try await storedSpeaker(SampleData.speakerTwoID, environment)
    weak.assignment = .suggested(personID: SampleData.personJeromeID, similarity: 0.5)
    try await environment.store.save(weak)
    let model = try await makeModel(environment)
    var writes = 0
    model.onWrite = { writes += 1 }

    let row = try XCTUnwrap(model.row(SampleData.speakerTwoID))
    XCTAssertEqual(row.speaker.assignment, .unknown)
    XCTAssertEqual(row.displayName, "Speaker 2")
    XCTAssertNil(row.suggestionStrength)
    XCTAssertNil(model.prefill(for: SampleData.speakerTwoID))
    XCTAssertFalse(
      model.options(for: SampleData.speakerTwoID, query: "").contains {
        if case .voice = $0.tag { true } else { false }
      })
    try await model.clear(speakerID: SampleData.speakerTwoID)
    XCTAssertEqual(writes, 0)
  }
}
