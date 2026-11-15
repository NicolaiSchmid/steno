import Foundation
import GRDB
import Testing

@testable import StenoCore

/// Undoable combining, paragraph moves and the transcript generation
/// (schema v7, `.plans/2026-11-14-speaker-identification.md` step 4). Rust:
/// the same rules in `crates/steno-core/src/store/people.rs`.
@Suite struct MoveTurnTests {
  static let one = SampleData.speakerOneID
  static let two = SampleData.speakerTwoID
  static let three = SampleData.uuid(22)
  static let meetingID = SampleData.meetingID
  static let anna = MeetingStoreTests.anna

  /// Segment 43 (6–8 s), a second paragraph of `speakerID` after the
  /// sample's three.
  static func segment43(_ speakerID: UUID?) -> TranscriptSegment {
    TranscriptSegment(
      id: SampleData.uuid(43), meetingID: meetingID, start: 6, end: 8, speakerID: speakerID,
      lane: .system, text: "Und die Nebenprojekte pausieren wir.",
      rawText: "und die nebenprojekte pausieren wir")
  }

  /// The sample meeting with its persons and participants, and `speakers`
  /// and `segments` as its transcript (generation 1).
  static func seeded(
    speakers: [Speaker] = SampleData.speakers(),
    segments: [TranscriptSegment] = SampleData.segments(), persons: [Person] = []
  ) async throws -> MeetingStore {
    let store = try MeetingStore.inMemory()
    try await store.save(SampleData.meeting())
    for person in SampleData.persons() + persons { try await store.save(person) }
    for participant in SampleData.participants() { try await store.save(participant) }
    try await store.replaceTranscript(SampleData.meeting(), segments: segments, speakers: speakers)
    return store
  }

  static func segments(_ store: MeetingStore) async throws -> [UUID: UUID?] {
    let export = try await store.export(meetingID: meetingID)
    return Dictionary(uniqueKeysWithValues: export.segments.map { ($0.id, $0.speakerID) })
  }

  static func move(_ ids: [Int], shown: UUID?) -> [TurnSegmentMove] {
    ids.map { TurnSegmentMove(segmentID: SampleData.uuid($0), shownSpeakerID: shown) }
  }

  // MARK: - Moves

  @Test func aParagraphMovesToTheVoiceThePersonAlreadyHas() async throws {
    let store = try await Self.seeded(segments: SampleData.segments() + [Self.segment43(Self.two)])
    let nicolai = try #require(try await store.person(id: SampleData.personNicolaiID))
    let speakers = try await store.speakers(meetingID: Self.meetingID)

    let destination = try await store.moveTurn(
      meetingID: Self.meetingID, generation: 1, segments: Self.move([41], shown: Self.two),
      to: nicolai)

    #expect(destination == Self.one)
    let segments = try await Self.segments(store)
    #expect(segments[SampleData.uuid(41)] == Self.one)
    #expect(segments[SampleData.uuid(43)] == Self.two, "the voice's other paragraph stays")
    #expect(try await store.speakers(meetingID: Self.meetingID) == speakers, "no row changes")
    #expect(try await store.person(id: nicolai.id) == nicolai, "a move trains nobody's voice")
    #expect(try await store.transcriptGeneration(meetingID: Self.meetingID) == 1)
  }

  /// Anna has no voice here: the move makes one, the next free label after
  /// every row's, the combined Speaker 2's included.
  @Test func aParagraphMovesToANewVoiceForAPersonWithoutOne() async throws {
    let store = try await Self.seeded(segments: SampleData.segments() + [Self.segment43(Self.two)])
    try await store.mergeSpeakers(Self.two, into: Self.one, meetingID: Self.meetingID)

    let destination = try await store.moveTurn(
      meetingID: Self.meetingID, generation: 1, segments: Self.move([42], shown: nil),
      to: Self.anna)

    let created = try #require(
      try await store.speakers(meetingID: Self.meetingID).first { $0.id == destination })
    #expect(
      created
        == Speaker(
          id: destination, meetingID: Self.meetingID, clusterLabel: "Speaker 3",
          assignment: .confirmed(personID: Self.anna.id), clusterConfidence: 0))
    #expect(try await Self.segments(store)[SampleData.uuid(42)] == destination)
    let anna = try #require(try await store.person(id: Self.anna.id), "the person is saved")
    #expect(anna.sampleCount == 0)
    #expect(anna.embedding == nil)
    #expect(
      try await store.mergedSpeakers(meetingID: Self.meetingID).map(\.speaker.id) == [Self.two])
  }

  @Test func theNextLabelSkipsLabelsThatAreNotNumbered() async throws {
    let me = LaneMerger.meSpeaker(meetingID: Self.meetingID, personID: nil)
    let seven = Speaker(
      id: Self.three, meetingID: Self.meetingID, clusterLabel: "Speaker 7", clusterConfidence: 0.5)
    let odd = Speaker(
      id: SampleData.uuid(26), meetingID: Self.meetingID, clusterLabel: "Speaker 9b",
      clusterConfidence: 0.5)
    let store = try await Self.seeded(
      speakers: [me, seven, odd],
      segments: SampleData.segments().map { segment in
        var segment = segment
        segment.speakerID = nil
        return segment
      })

    let destination = try await store.moveTurn(
      meetingID: Self.meetingID, generation: 1, segments: Self.move([42], shown: nil),
      to: Self.anna)

    let created = try #require(
      try await store.speakers(meetingID: Self.meetingID).first { $0.id == destination })
    #expect(created.clusterLabel == "Speaker 8")
  }

  /// Speaker 2, confirmed as Jérôme with a clip and a No, loses its only
  /// paragraph: the row goes with its clip file and its No, and Jérôme's
  /// voice no longer has it.
  @Test func anEmptiedVoiceIsDeletedWithItsClipAndItsPersonsVoiceIsRecomputed() async throws {
    let directory = try Fixtures.temporaryDirectory()
    defer { try? FileManager.default.removeItem(at: directory) }
    let clip = directory.appendingPathComponent("speaker-two.wav")
    try Data([1, 2, 3]).write(to: clip)
    var speakers = SampleData.speakers()
    speakers[1].assignment = .confirmed(personID: SampleData.personJeromeID)
    speakers[1].sampleClipURL = clip
    let store = try await Self.seeded(speakers: speakers)
    try await store.save(Self.anna)
    try await store.rejectSuggestion(speakerID: Self.two, personID: Self.anna.id)
    let nicolai = try #require(try await store.person(id: SampleData.personNicolaiID))

    try await store.moveTurn(
      meetingID: Self.meetingID, generation: 1, segments: Self.move([41], shown: Self.two),
      to: nicolai)

    #expect(try await store.speakers(meetingID: Self.meetingID).map(\.id) == [Self.one])
    #expect(!FileManager.default.fileExists(atPath: clip.path))
    #expect(try await store.suggestionRejections(meetingID: Self.meetingID).isEmpty)
    let jerome = try #require(try await store.person(id: SampleData.personJeromeID))
    #expect(jerome.sampleCount == 0)
    #expect(jerome.embedding == nil)
    #expect(try await store.person(id: nicolai.id) == nicolai)
  }

  /// Speaker 3 (Anna's) is combined into Speaker 2 (Jérôme's); moving the
  /// whole turn away empties both: Speaker 3 goes, then the voice it was
  /// combined into. Both persons' voices are recomputed.
  @Test func pruningFollowsTheCombinedChain() async throws {
    var speakers = SampleData.speakers()
    speakers[1].assignment = .confirmed(personID: SampleData.personJeromeID)
    speakers.append(
      Speaker(
        id: Self.three, meetingID: Self.meetingID, clusterLabel: "Speaker 3",
        assignment: .confirmed(personID: Self.anna.id), embedding: SampleData.embedding(axis: 2),
        clusterConfidence: 0.6))
    let store = try await Self.seeded(
      speakers: speakers, segments: SampleData.segments() + [Self.segment43(Self.three)],
      persons: [Self.anna])
    try await store.mergeSpeakers(Self.three, into: Self.two, meetingID: Self.meetingID)
    let jeromeBefore = try #require(try await store.person(id: SampleData.personJeromeID))
    #expect(jeromeBefore.sampleCount == 2, "Speaker 3 counts for Jérôme while combined")
    let nicolai = try #require(try await store.person(id: SampleData.personNicolaiID))

    try await store.moveTurn(
      meetingID: Self.meetingID, generation: 1, segments: Self.move([41, 43], shown: Self.two),
      to: nicolai)

    #expect(try await store.speakers(meetingID: Self.meetingID).map(\.id) == [Self.one])
    #expect(try await store.mergedSpeakers(meetingID: Self.meetingID).isEmpty)
    let segments = try await Self.segments(store)
    #expect(segments[SampleData.uuid(41)] == Self.one)
    #expect(segments[SampleData.uuid(43)] == Self.one)
    #expect(try await store.person(id: SampleData.personJeromeID)?.sampleCount == 0)
    #expect(try await store.person(id: Self.anna.id)?.sampleCount == 0)
  }

  /// A paragraph moved off a dismissed voice works without restoring it.
  @Test func aParagraphMovesOffADismissedVoice() async throws {
    let store = try await Self.seeded(segments: SampleData.segments() + [Self.segment43(Self.two)])
    try await store.setDismissed(speakerID: Self.two, true)

    let destination = try await store.moveTurn(
      meetingID: Self.meetingID, generation: 1, segments: Self.move([43], shown: Self.two),
      to: Self.anna)

    #expect(try await Self.segments(store)[SampleData.uuid(43)] == destination)
    let two = try #require(
      try await store.speakers(meetingID: Self.meetingID).first { $0.id == Self.two })
    #expect(two.dismissed)
  }

  @Test func aStaleMoveIsRefusedAndChangesNothing() async throws {
    let store = try await Self.seeded()
    let before = try await store.export(meetingID: Self.meetingID)
    var other = SampleData.meeting()
    other.id = SampleData.uuid(2)
    try await store.save(other)
    let cases: [(UUID, Int, [TurnSegmentMove])] = [
      (Self.meetingID, 1, Self.move([41], shown: Self.one)),  // shown under another voice
      (Self.meetingID, 1, Self.move([42], shown: Self.two)),  // shown with a voice it lacks
      (Self.meetingID, 1, Self.move([41, 42], shown: Self.two)),  // one of two is off
      (Self.meetingID, 0, Self.move([41], shown: Self.two)),  // an older transcript
      (Self.meetingID, 2, Self.move([41], shown: Self.two)),  // a newer one
      (Self.meetingID, 1, Self.move([999], shown: Self.two)),  // no such segment
      (Self.meetingID, 1, []),
      (other.id, 0, Self.move([41], shown: Self.two)),  // the segment of another meeting
      (SampleData.uuid(998), 1, Self.move([41], shown: Self.two)),  // no such meeting
    ]
    for (meetingID, generation, segments) in cases {
      await #expect(throws: MeetingStoreError.staleTranscript) {
        try await store.moveTurn(
          meetingID: meetingID, generation: generation, segments: segments, to: Self.anna)
      }
    }
    #expect(try await store.export(meetingID: Self.meetingID) == before)
    #expect(try await store.person(id: Self.anna.id) == nil, "a refused move saves nobody")
  }

  // MARK: - Combine and Separate again

  /// Speaker 2 is suggested as Jérôme with a name suggestion, a No to
  /// Anna and its clip; confirming it as Nicolai combines it into Speaker
  /// 1, and separating gives it back exactly.
  @Test func combineThenSeparateRestoresTheSourceRowExactly() async throws {
    let directory = try Fixtures.temporaryDirectory()
    defer { try? FileManager.default.removeItem(at: directory) }
    let clip = directory.appendingPathComponent("speaker-two.wav")
    try Data([1, 2, 3]).write(to: clip)
    var speakers = SampleData.speakers()
    speakers[1].sampleClipURL = clip
    let store = try await Self.seeded(speakers: speakers)
    try await store.save(Self.anna)
    try await store.replaceSummary(
      SampleData.meeting(), tasks: [], decisions: [],
      speakerNames: [
        SpeakerNameSuggestion(
          speakerID: Self.two, name: "Jérôme", confidence: 0.8, evidence: "Danke, Jérôme.")
      ])
    try await store.rejectSuggestion(speakerID: Self.two, personID: Self.anna.id)
    let rowBefore = try #require(
      try await store.speakers(meetingID: Self.meetingID).first { $0.id == Self.two })
    let suggestionsBefore = try await store.nameSuggestions(meetingID: Self.meetingID)
    #expect(suggestionsBefore.map(\.speakerID) == [Self.two])
    let rejectionsBefore = try await store.suggestionRejections(meetingID: Self.meetingID)
    let nicolai = try #require(try await store.person(id: SampleData.personNicolaiID))

    try await store.confirm(speakerID: Self.two, person: nicolai)

    #expect(try await store.speakers(meetingID: Self.meetingID).map(\.id) == [Self.one])
    #expect(
      try await store.mergedSpeakers(meetingID: Self.meetingID)
        == [MergedSpeaker(speaker: rowBefore, into: Self.one, root: Self.one)])
    #expect(
      try await store.nameSuggestions(meetingID: Self.meetingID).isEmpty,
      "a combined voice's suggestion is kept, not listed")
    #expect(try await store.suggestionRejections(meetingID: Self.meetingID) == rejectionsBefore)
    #expect(FileManager.default.fileExists(atPath: clip.path))
    #expect(try await Self.segments(store)[SampleData.uuid(41)] == Self.one)
    #expect(try await store.person(id: nicolai.id)?.sampleCount == 2)

    try await store.separateSpeaker(Self.two)

    let rowAfter = try #require(
      try await store.speakers(meetingID: Self.meetingID).first { $0.id == Self.two })
    #expect(rowAfter == rowBefore)
    #expect(try await store.mergedSpeakers(meetingID: Self.meetingID).isEmpty)
    #expect(try await store.nameSuggestions(meetingID: Self.meetingID) == suggestionsBefore)
    #expect(try await store.suggestionRejections(meetingID: Self.meetingID) == rejectionsBefore)
    #expect(try await Self.segments(store)[SampleData.uuid(41)] == Self.two)
    let restored = try #require(try await store.person(id: nicolai.id))
    #expect(restored.sampleCount == 1)
    #expect(restored.embedding == SampleData.embedding(axis: 0))
  }

  /// The "me" voice, unnamed (it cannot be dismissed), combined into
  /// Nicolai's Speaker 1: its row stays as it was, and separating unnames
  /// the "me" participant again (the Rust host named it when it combined).
  @Test func separatingMeRestoresItsRowAndItsParticipant() async throws {
    let meID = LaneMerger.meSpeakerID(meetingID: Self.meetingID)
    var me = LaneMerger.meSpeaker(meetingID: Self.meetingID, personID: nil)
    me.embedding = SampleData.embedding(axis: 3)
    var segments = SampleData.segments()
    segments[2].speakerID = meID
    let store = try await Self.seeded(speakers: SampleData.speakers() + [me], segments: segments)
    var participant = try #require(SampleData.participants().first { $0.role == .me })
    participant.personID = nil
    participant.displayName = LaneMerger.meSpeakerLabel
    try await store.save(participant)
    let rowBefore = try #require(
      try await store.speakers(meetingID: Self.meetingID).first { $0.id == meID })
    let nicolai = try #require(try await store.person(id: SampleData.personNicolaiID))

    try await store.confirm(speakerID: meID, person: nicolai)
    #expect(try await store.mergedSpeakers(meetingID: Self.meetingID).map(\.speaker) == [rowBefore])
    #expect(
      try await store.person(id: nicolai.id)?.sampleCount == 2,
      "a combined row counts whatever its own assignment")
    var named = participant
    named.personID = nicolai.id
    named.displayName = nicolai.displayName
    try await store.save(named)

    try await store.separateSpeaker(meID)

    #expect(
      try await store.speakers(meetingID: Self.meetingID).first { $0.id == meID } == rowBefore)
    let meParticipant = try #require(
      try await store.participants(meetingID: Self.meetingID).first { $0.role == .me })
    #expect(meParticipant == participant)
    #expect(try await store.person(id: nicolai.id)?.sampleCount == 1)
  }

  /// A "me" voice confirmed as Jérôme names the participant Jérôme once
  /// it is its own voice again.
  @Test func separatingAConfirmedMeNamesTheParticipantAfterItsPerson() async throws {
    let meID = LaneMerger.meSpeakerID(meetingID: Self.meetingID)
    let me = LaneMerger.meSpeaker(meetingID: Self.meetingID, personID: SampleData.personJeromeID)
    var segments = SampleData.segments()
    segments[2].speakerID = meID
    let store = try await Self.seeded(speakers: SampleData.speakers() + [me], segments: segments)
    try await store.mergeSpeakers(meID, into: Self.one, meetingID: Self.meetingID)

    try await store.separateSpeaker(meID)

    let meParticipant = try #require(
      try await store.participants(meetingID: Self.meetingID).first { $0.role == .me })
    #expect(meParticipant.personID == SampleData.personJeromeID)
    #expect(meParticipant.displayName == "Jérôme")
  }

  /// After the combine, one of Speaker 2's paragraphs moves to Anna;
  /// Separate again gives Speaker 2 the paragraph it still has and keeps
  /// the move.
  @Test func separateAgainAfterALaterMoveKeepsTheMove() async throws {
    let store = try await Self.seeded(segments: SampleData.segments() + [Self.segment43(Self.two)])
    let nicolai = try #require(try await store.person(id: SampleData.personNicolaiID))
    try await store.confirm(speakerID: Self.two, person: nicolai)
    let annas = try await store.moveTurn(
      meetingID: Self.meetingID, generation: 1, segments: Self.move([43], shown: Self.one),
      to: Self.anna)

    try await store.separateSpeaker(Self.two)

    let segments = try await Self.segments(store)
    #expect(segments[SampleData.uuid(41)] == Self.two)
    #expect(segments[SampleData.uuid(43)] == annas)
    #expect(segments[SampleData.uuid(40)] == Self.one)
  }

  @Test func separateIsRefusedWhenNothingIsLeftOrNothingIsCombined() async throws {
    let silent = Speaker(
      id: Self.three, meetingID: Self.meetingID, clusterLabel: "Speaker 3",
      embedding: SampleData.embedding(axis: 2), clusterConfidence: 0.4)
    let store = try await Self.seeded(speakers: SampleData.speakers() + [silent])
    let nicolai = try #require(try await store.person(id: SampleData.personNicolaiID))

    // A combined voice without a paragraph of its own.
    try await store.mergeSpeakers(Self.three, into: Self.one, meetingID: Self.meetingID)
    await #expect(throws: MeetingStoreError.staleTranscript) {
      try await store.separateSpeaker(Self.three)
    }
    #expect(
      try await store.mergedSpeakers(meetingID: Self.meetingID).map(\.speaker.id) == [Self.three])

    // Its only paragraph moved away since: the move pruned the row.
    try await store.confirm(speakerID: Self.two, person: nicolai)
    try await store.moveTurn(
      meetingID: Self.meetingID, generation: 1, segments: Self.move([41], shown: Self.one),
      to: Self.anna)
    await #expect(throws: MeetingStoreError.notCombined(Self.two)) {
      try await store.separateSpeaker(Self.two)
    }
    await #expect(throws: MeetingStoreError.notCombined(Self.one)) {
      try await store.separateSpeaker(Self.one)
    }
    await #expect(throws: MeetingStoreError.notCombined(SampleData.uuid(999))) {
      try await store.separateSpeaker(SampleData.uuid(999))
    }
  }

  /// Speaker 3 is combined into Speaker 2, which is then combined into
  /// Speaker 1: Speaker 3 reads as Speaker 1 and counts for Nicolai.
  @Test func aVoiceCountsEveryRowCombinedIntoItAtAnyDepth() async throws {
    let deep = Speaker(
      id: Self.three, meetingID: Self.meetingID, clusterLabel: "Speaker 3",
      assignment: .confirmed(personID: SampleData.personJeromeID),
      embedding: SampleData.embedding(axis: 2), clusterConfidence: 0.4)
    let store = try await Self.seeded(
      speakers: SampleData.speakers() + [deep],
      segments: SampleData.segments() + [Self.segment43(Self.three)])
    try await store.mergeSpeakers(Self.three, into: Self.two, meetingID: Self.meetingID)
    try await store.mergeSpeakers(Self.two, into: Self.one, meetingID: Self.meetingID)

    let merged = try await store.mergedSpeakers(meetingID: Self.meetingID)
    #expect(merged.map(\.speaker.id) == [Self.two, Self.three])
    #expect(merged.map(\.into) == [Self.one, Self.two])
    #expect(merged.map(\.root) == [Self.one, Self.one])
    #expect(try await Self.segments(store)[SampleData.uuid(43)] == Self.one)
    #expect(try await store.person(id: SampleData.personNicolaiID)?.sampleCount == 3)
    #expect(
      try await store.person(id: SampleData.personJeromeID)?.sampleCount == 0,
      "a combined row never counts for its own person")
    #expect(try await store.recentPersons().map(\.id) == [SampleData.personNicolaiID])
  }

  // MARK: - Processing generation

  @Test func processingAgainDropsMergesAndRaisesTheGeneration() async throws {
    let store = try await Self.seeded()
    #expect(try await store.transcriptGeneration(meetingID: Self.meetingID) == 1)
    try await store.mergeSpeakers(Self.two, into: Self.one, meetingID: Self.meetingID)

    try await store.replaceTranscript(
      SampleData.meeting(), segments: SampleData.segments(), speakers: SampleData.speakers())

    #expect(try await store.transcriptGeneration(meetingID: Self.meetingID) == 2)
    #expect(try await store.mergedSpeakers(meetingID: Self.meetingID).isEmpty)
    #expect(try await store.speakers(meetingID: Self.meetingID) == SampleData.speakers())
    await #expect(throws: MeetingStoreError.staleTranscript) {
      try await store.moveTurn(
        meetingID: Self.meetingID, generation: 1, segments: Self.move([41], shown: Self.two),
        to: Self.anna)
    }
    await #expect(throws: MeetingStoreError.meetingNotFound(SampleData.uuid(998))) {
      try await store.transcriptGeneration(meetingID: SampleData.uuid(998))
    }
  }

  /// Saves write their own columns only: a combined speaker saved again
  /// stays combined, and a meeting saved or updated keeps its generation.
  @Test func savesLeaveTheCombineAndTheGenerationAlone() async throws {
    let store = try await Self.seeded()
    try await store.mergeSpeakers(Self.two, into: Self.one, meetingID: Self.meetingID)
    var two = try #require(try await store.mergedSpeakers(meetingID: Self.meetingID).first).speaker
    two.clusterConfidence = 0.5
    try await store.save(two)
    try await store.save(SampleData.meeting())
    try await store.rename(meetingID: Self.meetingID, title: "Neu", now: SampleData.updatedAt)

    #expect(
      try await store.mergedSpeakers(meetingID: Self.meetingID)
        == [MergedSpeaker(speaker: two, into: Self.one, root: Self.one)])
    #expect(try await store.transcriptGeneration(meetingID: Self.meetingID) == 1)
  }

  /// A database from before v7: its combines deleted their source, so
  /// there is no combined voice to show, and the generation starts at 0.
  @Test func aV6DatabaseShowsNoCombinedVoice() async throws {
    let queue = try DatabaseQueue()
    try Migrations.migrator(upTo: 6).migrate(queue)
    try await queue.write { db in
      try MeetingRow(SampleData.meeting()).insert(db)
      for person in SampleData.persons() { try PersonRow(person).insert(db) }
      try SpeakerRow(SampleData.speakers()[0]).insert(db)
      for var segment in SampleData.segments() {
        if segment.speakerID != nil { segment.speakerID = Self.one }
        try TranscriptSegmentRow(segment).insert(db)
      }
    }
    let store = try MeetingStore(writer: queue)

    #expect(try await store.mergedSpeakers(meetingID: Self.meetingID).isEmpty)
    #expect(try await store.speakers(meetingID: Self.meetingID) == [SampleData.speakers()[0]])
    #expect(try await store.transcriptGeneration(meetingID: Self.meetingID) == 0)
    #expect(
      try await Self.segments(store)
        == [SampleData.uuid(40): Self.one, SampleData.uuid(41): Self.one, SampleData.uuid(42): nil])
  }

  @Test func aCombinedVoiceQuotesItsOwnClipByTime() {
    var two = SampleData.speakers()[1]
    var segments = SampleData.segments()
    for index in segments.indices { segments[index].speakerID = Self.one }
    // A segment reaching outside the clip is context, not its quote.
    #expect(SpeakerExcerpts.clipText(for: two, in: segments) == nil)
    two.sampleClipRange = 2.5...5.5
    #expect(SpeakerExcerpts.clipText(for: two, in: segments) == "Ich prüfe das Budget bis Freitag.")
    two.sampleClipRange = nil
    #expect(SpeakerExcerpts.clipText(for: two, in: segments) == nil)
    two.sampleClipRange = 10...12
    #expect(SpeakerExcerpts.clipText(for: two, in: segments) == nil)
  }
}
