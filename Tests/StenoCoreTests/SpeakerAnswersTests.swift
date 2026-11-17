import Foundation
import GRDB
import Testing

@testable import StenoCore

/// The user's answers about voices besides a name: No to a suggestion, Not
/// a person, and Remove name (schema v6). Rust:
/// `crates/steno-core/tests/speaker_answers.rs`.
@Suite struct SpeakerAnswersTests {
  static let now = SampleData.createdAt

  /// The sample's speakers come back from processing again, unchanged.
  static func processAgain(_ store: MeetingStore) async throws {
    try await store.replaceTranscript(
      SampleData.meeting(), segments: SampleData.segments(), speakers: SampleData.speakers())
  }

  static func speaker(_ id: UUID, _ store: MeetingStore) async throws -> Speaker {
    try #require(try await store.speakers(meetingID: SampleData.meetingID).first { $0.id == id })
  }

  @Test func noToASuggestionIsKeptAndNeverTouchesTheVoice() async throws {
    let store = try await MeetingStoreTests.populated()
    let suggested = SampleData.speakerTwoID
    let jerome = try #require(try await store.person(id: SampleData.personJeromeID))
    #expect(
      try await Self.speaker(suggested, store).assignment
        == .suggested(personID: jerome.id, similarity: 0.72))

    try await store.rejectSuggestion(speakerID: suggested, personID: jerome.id)
    #expect(try await Self.speaker(suggested, store).assignment == .unknown)
    #expect(
      try await store.suggestionRejections(meetingID: SampleData.meetingID)
        == [suggested: [jerome.id]])
    #expect(
      try await store.person(id: jerome.id) == jerome, "a No leaves the voice as it was")
    // Answering twice is one row.
    try await store.rejectSuggestion(speakerID: suggested, personID: jerome.id)
    #expect(try await store.suggestionRejections(meetingID: SampleData.meetingID).count == 1)

    // Processing again suggests Jérôme for Speaker 2 once more: dropped.
    try await Self.processAgain(store)
    #expect(try await Self.speaker(suggested, store).assignment == .unknown)
    #expect(
      try await store.suggestionRejections(meetingID: SampleData.meetingID)
        == [suggested: [jerome.id]],
      "the No survives processing again")

    // Naming the voice as that person after all answers the No.
    try await store.confirm(speakerID: suggested, person: jerome)
    #expect(try await store.suggestionRejections(meetingID: SampleData.meetingID).isEmpty)

    let ghost = UUID()
    await #expect(throws: MeetingStoreError.speakerNotFound(ghost)) {
      try await store.rejectSuggestion(speakerID: ghost, personID: jerome.id)
    }
    await #expect(throws: MeetingStoreError.personNotFound(ghost)) {
      try await store.rejectSuggestion(speakerID: suggested, personID: ghost)
    }
  }

  @Test func aNoToSomeoneElseLeavesTheSuggestion() async throws {
    let store = try await MeetingStoreTests.populated()
    let before = try await Self.speaker(SampleData.speakerTwoID, store)
    let anna = try await store.resolvePerson(named: "Anna", now: Self.now)
    try await store.save(anna)
    try await store.rejectSuggestion(speakerID: before.id, personID: anna.id)
    #expect(try await Self.speaker(before.id, store).assignment == before.assignment)
  }

  @Test func notAPersonLeavesVoiceMemoryAndKeepsTheText() async throws {
    let store = try await MeetingStoreTests.populated()
    let speakerID = SampleData.speakerTwoID
    let anna = try await store.resolvePerson(named: "Anna", now: Self.now)
    try await store.confirm(speakerID: speakerID, person: anna)
    #expect(try await store.person(id: anna.id)?.sampleCount == 1)
    let segmentsBefore = try await store.export(meetingID: SampleData.meetingID).segments

    try await store.setDismissed(speakerID: speakerID, true)
    let dismissed = try await Self.speaker(speakerID, store)
    #expect(dismissed.dismissed)
    #expect(dismissed.assignment == .unknown)
    let voice = try #require(try await store.person(id: anna.id))
    #expect(voice.sampleCount == 0, "the voice no longer learns from it")
    #expect(voice.embedding == nil)
    #expect(
      try await store.export(meetingID: SampleData.meetingID).segments == segmentsBefore,
      "the segments keep their text and their speaker")
    #expect(
      try await store.recentPersons().map(\.id) == [SampleData.personNicolaiID],
      "Anna owns no voice any more")

    // Processing again keeps the dismissal; the voice match is dropped.
    try await Self.processAgain(store)
    let again = try await Self.speaker(speakerID, store)
    #expect(again.dismissed)
    #expect(again.assignment == .unknown)

    try await store.setDismissed(speakerID: speakerID, false)
    let restored = try await Self.speaker(speakerID, store)
    #expect(!restored.dismissed)
    #expect(restored.assignment == .unknown)

    // Naming a dismissed voice makes it a person again.
    try await store.setDismissed(speakerID: speakerID, true)
    try await store.confirm(speakerID: speakerID, person: anna)
    let named = try await Self.speaker(speakerID, store)
    #expect(!named.dismissed)
    #expect(named.assignment == .confirmed(personID: anna.id))

    let ghost = UUID()
    await #expect(throws: MeetingStoreError.speakerNotFound(ghost)) {
      try await store.setDismissed(speakerID: ghost, true)
    }
    await #expect(throws: MeetingStoreError.speakerNotFound(ghost)) {
      try await store.setDismissed(speakerID: ghost, false)
    }
  }

  @Test func removeNameUnnamesTheVoiceAndRecomputesThePerson() async throws {
    let store = try await MeetingStoreTests.populated()
    let speakerID = SampleData.speakerTwoID
    let anna = try await store.resolvePerson(named: "Anna", now: Self.now)
    try await store.confirm(speakerID: speakerID, person: anna)

    try await store.clear(speakerID: speakerID)
    let cleared = try await Self.speaker(speakerID, store)
    #expect(cleared.assignment == .unknown)
    #expect(!cleared.dismissed)
    let person = try #require(try await store.person(id: anna.id))
    #expect(person.displayName == "Anna", "the person keeps their name")
    #expect(person.sampleCount == 0)
    #expect(try await store.suggestionRejections(meetingID: SampleData.meetingID).isEmpty)

    // An unknown voice stays unknown; a missing one is an error.
    try await store.clear(speakerID: speakerID)
    #expect(try await Self.speaker(speakerID, store) == cleared)
    let ghost = UUID()
    await #expect(throws: MeetingStoreError.speakerNotFound(ghost)) {
      try await store.clear(speakerID: ghost)
    }
  }

  /// This build never names the "me" participant itself (the merge stage
  /// reads the speaker's person from it); the sample's participant is named
  /// as the Rust host names it. Remove name on the "me" voice unnames it
  /// and keeps the person.
  @Test func removeNameOnMeUnnamesTheMeParticipantOnly() async throws {
    let store = try await MeetingStoreTests.populated()
    let meID = LaneMerger.meSpeakerID(meetingID: SampleData.meetingID)
    try await store.save(LaneMerger.meSpeaker(meetingID: SampleData.meetingID, personID: nil))
    let nicolai = try #require(try await store.person(id: SampleData.personNicolaiID))
    func meParticipant() async throws -> Participant {
      try #require(
        try await store.participants(meetingID: SampleData.meetingID).first { $0.role == .me })
    }

    // Another voice's Remove name leaves the "me" participant alone.
    try await store.clear(speakerID: SampleData.speakerOneID)
    #expect(try await meParticipant().personID == nicolai.id)
    #expect(try await meParticipant().displayName == "Nicolai")

    try await store.confirm(speakerID: meID, person: nicolai)
    #expect(try await Self.speaker(meID, store).assignment == .confirmed(personID: nicolai.id))
    try await store.clear(speakerID: meID)
    let unnamed = try await meParticipant()
    #expect(unnamed.personID == nil)
    #expect(unnamed.displayName == LaneMerger.meSpeakerLabel)
    #expect(unnamed.email == "nicolai@example.com")
    #expect(try await store.person(id: nicolai.id) != nil)

    // The clearing is kept for processing again, as a No to that person and
    // to the Rust app's own person; naming Me answers every one.
    let ben = try await store.resolvePerson(named: "Ben", now: Self.now)
    try await store.writer.write { db in
      try db.execute(
        sql: "INSERT INTO setting (key, value) VALUES ('selfPersonID', ?)",
        arguments: ["\"\(nicolai.id.uuidString)\""])
    }
    #expect(
      try await store.suggestionRejections(meetingID: SampleData.meetingID)
        == [meID: [nicolai.id]])
    try await store.confirm(speakerID: meID, person: ben)
    #expect(try await store.suggestionRejections(meetingID: SampleData.meetingID).isEmpty)
    try await store.clear(speakerID: meID)
    #expect(
      try await store.suggestionRejections(meetingID: SampleData.meetingID)
        == [meID: [nicolai.id, ben.id]])
    var withMe = SampleData.speakers()
    withMe.append(LaneMerger.meSpeaker(meetingID: SampleData.meetingID, personID: nil))
    try await store.replaceTranscript(
      SampleData.meeting(), segments: SampleData.segments(), speakers: withMe)
    #expect(try await Self.speaker(meID, store).assignment == .unknown)
    #expect(
      try await store.suggestionRejections(meetingID: SampleData.meetingID)
        == [meID: [nicolai.id, ben.id]])
  }

  /// The score each No was recorded with, by speaker then person.
  static func rejectionScores(_ store: MeetingStore) async throws -> [Float?] {
    try await store.writer.read { db in
      try Float?.fetchAll(
        db,
        sql: """
          SELECT similarity FROM speakerSuggestionRejection WHERE meetingID = ?
          ORDER BY speakerID, personID
          """,
        arguments: [SampleData.meetingID.uuidString])
    }
  }

  @Test func aNoKeepsTheVoiceMatchScore() async throws {
    let store = try await MeetingStoreTests.populated()
    try await store.rejectSuggestion(
      speakerID: SampleData.speakerTwoID, personID: SampleData.personJeromeID)
    #expect(try await Self.rejectionScores(store) == [0.72], "the suggestion's similarity")

    let store2 = try await MeetingStoreTests.populated()
    var anna = try await store2.resolvePerson(named: "Anna", now: Self.now)
    anna.embedding = SampleData.embedding(axis: 1)
    try await store2.save(anna)
    try await store2.rejectSuggestion(speakerID: SampleData.speakerTwoID, personID: anna.id)
    let scores = try await Self.rejectionScores(store2)
    #expect(scores.count == 1)
    let cosine = try #require(scores.first ?? nil)
    #expect(abs(cosine - 1) < 0.0001, "else the voices' cosine")
  }

  /// The score a Yes was recorded with (`speaker.confirmedSimilarity`).
  static func yesScore(_ speakerID: UUID, _ store: MeetingStore) async throws -> Float? {
    try await store.writer.read { db in
      try MeetingStore.confirmedSimilarity(speakerID: speakerID, db)
    }
  }

  /// Rust: `a_yes_keeps_the_voice_match_score`.
  @Test func aYesKeepsTheVoiceMatchScore() async throws {
    let store = try await MeetingStoreTests.populated()
    let jerome = try #require(try await store.person(id: SampleData.personJeromeID))
    let speakerID = SampleData.speakerTwoID
    try await store.confirm(speakerID: speakerID, person: jerome)
    #expect(try await Self.yesScore(speakerID, store) == 0.72, "the suggestion's similarity")

    // Kept across Process again with the confirmation.
    var rerun = SampleData.speakers()
    rerun[1].assignment = .unknown
    try await store.replaceTranscript(
      SampleData.meeting(), segments: SampleData.segments(), speakers: rerun)
    #expect(try await Self.yesScore(speakerID, store) == 0.72)

    // Someone else: the cosine with their stored voice.
    var anna = try await store.resolvePerson(named: "Anna", now: Self.now)
    anna.embedding = SampleData.embedding(axis: 1)
    try await store.save(anna)
    try await store.confirm(speakerID: speakerID, person: anna)
    let cosine = try #require(try await Self.yesScore(speakerID, store))
    #expect(abs(cosine - 1) < 0.0001)

    // Remove name takes the Yes back.
    try await store.clear(speakerID: speakerID)
    #expect(try await Self.yesScore(speakerID, store) == nil)
  }

  @Test func aNoCannotAnswerAConfirmedNameAndMeIsNeverDismissed() async throws {
    let store = try await MeetingStoreTests.populated()
    let speakerID = SampleData.speakerOneID
    let nicolai = SampleData.personNicolaiID
    await #expect(throws: MeetingStoreError.rejectedConfirmedPerson(speakerID, nicolai)) {
      try await store.rejectSuggestion(speakerID: speakerID, personID: nicolai)
    }
    #expect(try await store.suggestionRejections(meetingID: SampleData.meetingID).isEmpty)
    #expect(try await Self.speaker(speakerID, store).assignment == .confirmed(personID: nicolai))

    let meID = LaneMerger.meSpeakerID(meetingID: SampleData.meetingID)
    try await store.save(LaneMerger.meSpeaker(meetingID: SampleData.meetingID, personID: nil))
    await #expect(throws: MeetingStoreError.meSpeakerNotDismissable(meID)) {
      try await store.setDismissed(speakerID: meID, true)
    }
    #expect(try await !Self.speaker(meID, store).dismissed)
  }

  /// "Speaker n" counts by first speech, so another run can swap the
  /// labels: the answers follow the voice, not the id. Rust:
  /// `processing_again_carries_answers_to_the_same_voice_when_labels_swap`.
  @Test func processingAgainCarriesAnswersToTheSameVoiceWhenLabelsSwap() async throws {
    let store = try await MeetingStoreTests.populated()
    let nicolai = try #require(try await store.person(id: SampleData.personNicolaiID))
    let anna = try await store.resolvePerson(named: "Anna", now: Self.now)
    try await store.save(anna)
    // Speaker 1 is Nicolai's (confirmed in the sample); No to Anna for it.
    try await store.rejectSuggestion(speakerID: SampleData.speakerOneID, personID: anna.id)
    try await store.setDismissed(speakerID: SampleData.speakerTwoID, true)
    // Processing again as it was recomputes Nicolai's voice from his one row.
    try await Self.processAgain(store)
    let voice = try await store.person(id: nicolai.id)
    #expect(voice?.sampleCount == 1)

    // The re-run hears Speaker 1's voice second and suggests Anna for it.
    var swapped = SampleData.speakers()
    swapped[0].embedding = SampleData.embedding(axis: 1)
    swapped[0].assignment = .unknown
    swapped[1].embedding = SampleData.embedding(axis: 0)
    swapped[1].assignment = .suggested(personID: anna.id, similarity: 0.8)
    try await store.replaceTranscript(
      SampleData.meeting(), segments: SampleData.segments(), speakers: swapped)
    let first = try await Self.speaker(SampleData.speakerOneID, store)
    let second = try await Self.speaker(SampleData.speakerTwoID, store)
    #expect(first.dismissed, "the noise is still not a person")
    #expect(first.assignment == .unknown)
    #expect(!second.dismissed)
    #expect(
      second.assignment == .confirmed(personID: nicolai.id),
      "Nicolai's voice keeps his name, and the No drops Anna")
    #expect(
      try await store.suggestionRejections(meetingID: SampleData.meetingID)
        == [SampleData.speakerTwoID: [anna.id]])
    #expect(try await store.person(id: nicolai.id) == voice)
  }

  @Test func processingAgainDropsAnswersWhoseVoiceIsGone() async throws {
    let store = try await MeetingStoreTests.populated()
    let nicolai = SampleData.personNicolaiID
    try await store.setDismissed(speakerID: SampleData.speakerTwoID, true)
    try await store.rejectSuggestion(
      speakerID: SampleData.speakerTwoID, personID: SampleData.personJeromeID)

    // Speaker 2 is gone; Speaker 1 is still Nicolai's voice.
    var kept = [SampleData.speakers()[0]]
    kept[0].assignment = .unknown
    let segments = Array(SampleData.segments().prefix(1))
    try await store.replaceTranscript(SampleData.meeting(), segments: segments, speakers: kept)
    var stored = try await store.speakers(meetingID: SampleData.meetingID)
    #expect(stored.count == 1)
    #expect(stored.first?.assignment == .confirmed(personID: nicolai))
    #expect(stored.first?.dismissed == false)
    #expect(try await store.suggestionRejections(meetingID: SampleData.meetingID).isEmpty)

    // Then Speaker 1 is another voice: Nicolai's name is not handed to it.
    kept[0].embedding = SampleData.embedding(axis: 2)
    try await store.replaceTranscript(SampleData.meeting(), segments: segments, speakers: kept)
    stored = try await store.speakers(meetingID: SampleData.meetingID)
    #expect(stored.first?.assignment == .unknown, "shown for review")
    let voice = try #require(try await store.person(id: nicolai))
    #expect(voice.sampleCount == 0, "his voice no longer counts it")
    #expect(voice.embedding == nil)
  }

  /// `weights` of the sample's unit axes summed: each weight is the blend's
  /// cosine with that axis.
  static func blend(_ weights: [(Float, Int)]) -> Embedding {
    var values = [Float](repeating: 0, count: Embedding.dimension)
    for (weight, axis) in weights { values[axis] += weight }
    return Embedding(values)
  }

  /// Nicolai is confirmed on Speaker 1, Speaker 2 is unanswered. The re-run
  /// finds one voice that scores 0.66 with Nicolai's but is Speaker 2's: an
  /// answered row must not take a voice an unanswered one matches better.
  /// Rust: `processing_again_does_not_let_an_answer_take_another_voices_match`.
  @Test func processingAgainDoesNotLetAnAnswerTakeAnotherVoicesMatch() async throws {
    let store = try await MeetingStoreTests.populated()
    let speakerTwo = Self.blend([(0.66, 0), (0.75, 1)])
    var stored = SampleData.speakers()
    stored[1].embedding = speakerTwo
    stored[1].assignment = .unknown
    try await store.save(stored[1])

    var rerun = SampleData.speakers()
    rerun[0].embedding = speakerTwo
    rerun[0].assignment = .unknown
    rerun[1].embedding = SampleData.embedding(axis: 2)
    rerun[1].assignment = .unknown
    try await store.replaceTranscript(
      SampleData.meeting(), segments: SampleData.segments(), speakers: rerun)
    let speakers = try await store.speakers(meetingID: SampleData.meetingID)
    #expect(
      speakers.allSatisfy { !$0.assignment.isConfirmed },
      "Nicolai's name is not handed to Speaker 2's voice")
    #expect(try await store.person(id: SampleData.personNicolaiID)?.sampleCount == 0)
  }

  /// Two new voices score 0.80 and 0.75 with a confirmed one: too close to
  /// tell, so neither takes the name and both show for review. Rust:
  /// `processing_again_leaves_a_near_tie_for_the_user`.
  @Test func processingAgainLeavesANearTieForTheUser() async throws {
    let store = try await MeetingStoreTests.populated()
    try await store.setDismissed(speakerID: SampleData.speakerTwoID, true)
    var rerun = SampleData.speakers()
    rerun[0].embedding = Self.blend([(0.80, 0), (0.60, 1)])
    rerun[0].assignment = .unknown
    rerun[1].embedding = Self.blend([(0.75, 0), (0.66, 2)])
    rerun[1].assignment = .unknown
    try await store.replaceTranscript(
      SampleData.meeting(), segments: SampleData.segments(), speakers: rerun)
    var speakers = try await store.speakers(meetingID: SampleData.meetingID)
    #expect(
      speakers.allSatisfy { !$0.assignment.isConfirmed && !$0.dismissed },
      "a contested match carries nothing")

    // A clear match still carries: the same voice again, nothing close.
    let nicolai = try #require(try await store.person(id: SampleData.personNicolaiID))
    try await store.confirm(speakerID: SampleData.speakerOneID, person: nicolai)
    var clear = rerun
    clear[1].embedding = SampleData.embedding(axis: 2)
    try await store.replaceTranscript(
      SampleData.meeting(), segments: SampleData.segments(), speakers: clear)
    speakers = try await store.speakers(meetingID: SampleData.meetingID)
    #expect(
      speakers.first { $0.id == SampleData.speakerOneID }?.assignment
        == .confirmed(personID: nicolai.id))
  }

  /// A database from before v6 opens with every speaker a person and no
  /// answers recorded.
  @Test func aV5DatabaseReadsEveryVoiceAsNotDismissed() async throws {
    let queue = try DatabaseQueue()
    try Migrations.migrator(upTo: 5).migrate(queue)
    try await queue.write { db in
      try MeetingRow(SampleData.meeting()).insert(db)
      try db.execute(
        sql: """
          INSERT INTO speaker (id, meetingID, clusterLabel, assignment, clusterConfidence)
          VALUES (?, ?, 'Speaker 1', 'unknown', 0.9)
          """,
        arguments: [SampleData.speakerOneID.uuidString, SampleData.meetingID.uuidString])
    }
    let store = try MeetingStore(writer: queue)
    let speakers = try await store.speakers(meetingID: SampleData.meetingID)
    #expect(speakers.count == 1)
    #expect(speakers.first?.dismissed == false)
    #expect(try await store.suggestionRejections(meetingID: SampleData.meetingID).isEmpty)
  }
}
