import Foundation
import GRDB
import Testing

@testable import StenoCore

@Suite struct MeetingStoreTests {
  /// A store holding the full sample meeting.
  static func populated() async throws -> MeetingStore {
    let store = try MeetingStore.inMemory()
    try await store.save(SampleData.meeting())
    for person in SampleData.persons() { try await store.save(person) }
    for participant in SampleData.participants() { try await store.save(participant) }
    try await store.replaceTranscript(
      SampleData.meeting(), segments: SampleData.segments(), speakers: SampleData.speakers())
    let output = SampleData.summaryOutput()
    try await store.replaceSummary(
      SampleData.meeting(), tasks: output.tasks, decisions: output.decisions)
    try await store.save(SampleData.audioAsset())
    return store
  }

  @Test func saveAndFetchMeeting() async throws {
    let store = try MeetingStore.inMemory()
    #expect(try await store.meeting(id: SampleData.meetingID) == nil)
    try await store.save(SampleData.meeting())
    #expect(try await store.meeting(id: SampleData.meetingID) == SampleData.meeting())
    var renamed = SampleData.meeting()
    renamed.title = "Renamed"
    try await store.save(renamed)
    #expect(try await store.meeting(id: SampleData.meetingID)?.title == "Renamed")
  }

  @Test func meetingsAreNewestFirstWithPaging() async throws {
    let store = try MeetingStore.inMemory()
    for n in 1...5 {
      var meeting = SampleData.meeting()
      meeting.id = SampleData.uuid(100 + n)
      meeting.startedAt = SampleData.startedAt.addingTimeInterval(Double(n) * 3600)
      meeting.summary = nil
      try await store.save(meeting)
    }
    let all = try await store.meetings()
    #expect(all.map(\.id) == (1...5).reversed().map { SampleData.uuid(100 + $0) })
    let page = try await store.meetings(limit: 2, offset: 1)
    #expect(page.map(\.id) == [SampleData.uuid(104), SampleData.uuid(103)])
  }

  @Test func setStateUpdatesStateAndTimestamp() async throws {
    let store = try MeetingStore.inMemory()
    try await store.save(SampleData.meeting(state: .queued))
    let later = SampleData.updatedAt.addingTimeInterval(60)
    try await store.setState(
      .failed(reason: "summarize: boom"), meetingID: SampleData.meetingID, now: later)
    let meeting = try #require(try await store.meeting(id: SampleData.meetingID))
    #expect(meeting.state == .failed(reason: "summarize: boom"))
    #expect(meeting.updatedAt == later)
    await #expect(throws: MeetingStoreError.meetingNotFound(SampleData.uuid(999))) {
      try await store.setState(.ready, meetingID: SampleData.uuid(999), now: later)
    }
  }

  @Test func updateReadsTheCurrentRowAndProcessingWritesKeepUserColumns() async throws {
    let store = try await Self.populated()
    let stale = SampleData.meeting()
    let later = SampleData.updatedAt.addingTimeInterval(60)

    // The user edits while a stage holds `stale`.
    let edited = try await store.update(meetingID: stale.id, now: later) {
      $0.scratchpad = "Neue Notizen."
      $0.tags = ["neu"]
    }
    #expect(edited.scratchpad == "Neue Notizen.")
    #expect(edited.updatedAt == later)
    #expect(try await store.meeting(id: stale.id) == edited)

    // The stage writes its results from the stale snapshot.
    var results = stale
    results.title = "Vom Modell"
    results.state = .ready
    results.updatedAt = later.addingTimeInterval(1)
    try await store.replaceTranscript(results, segments: [], speakers: [])
    let afterTranscript = try #require(try await store.meeting(id: stale.id))
    #expect(afterTranscript.title == "Vom Modell")
    #expect(afterTranscript.scratchpad == "Neue Notizen.", "not reverted by the stale snapshot")
    #expect(afterTranscript.tags == ["neu"])
    #expect(afterTranscript.updatedAt == results.updatedAt)

    results.templateID = "interview"
    try await store.replaceSummary(results, tasks: [], decisions: [])
    let afterSummary = try #require(try await store.meeting(id: stale.id))
    #expect(afterSummary.templateID == "interview")
    #expect(afterSummary.scratchpad == "Neue Notizen.")
    #expect(afterSummary.tags == ["neu"])
  }

  @Test func replaceTranscriptReplacesSpeakersAndSegments() async throws {
    let store = try await Self.populated()
    let newSpeaker = Speaker(
      id: SampleData.uuid(22), meetingID: SampleData.meetingID, clusterLabel: "Speaker 1",
      clusterConfidence: 0.5)
    let newSegment = TranscriptSegment(
      id: SampleData.uuid(43), meetingID: SampleData.meetingID, start: 0, end: 1,
      speakerID: newSpeaker.id, lane: .mixed, text: "Neu.", rawText: "neu")
    try await store.replaceTranscript(
      SampleData.meeting(), segments: [newSegment], speakers: [newSpeaker])
    let export = try await store.export(meetingID: SampleData.meetingID)
    #expect(export.speakers == [newSpeaker])
    #expect(export.segments == [newSegment])
    #expect(export.persons.map(\.id) == [SampleData.personJeromeID, SampleData.personNicolaiID])
    #expect(try await store.search("neunzig") == [])
  }

  @Test func replaceSummaryWritesSummaryTextTasksAndDecisions() async throws {
    let store = try await Self.populated()
    let export = try await store.export(meetingID: SampleData.meetingID)
    #expect(export.meeting.summary == SampleData.summaryDocument())
    #expect(export.meeting.templateID == "default")
    #expect(export.tasks == SampleData.tasks())
    #expect(export.decisions.map(\.text) == ["90/10-Aufteilung wird umgesetzt."])
    let summaryText = try await store.writer.read { db in
      try String.fetchOne(db, sql: "SELECT summaryText FROM meeting")
    }
    #expect(summaryText == SampleData.summaryDocument().plainText)
    #expect(try await store.search("Zeitplan").map(\.meetingID) == [SampleData.meetingID])

    var standup = SampleData.meeting()
    standup.templateID = "daily-standup"
    standup.summary?.templateID = "daily-standup"
    try await store.replaceSummary(standup, tasks: [], decisions: ["Nur eine."])
    let second = try await store.export(meetingID: SampleData.meetingID)
    #expect(second.tasks.isEmpty)
    #expect(second.decisions.map(\.text) == ["Nur eine."])
    #expect(second.meeting.templateID == "daily-standup")
    #expect(second.meeting.summary?.templateID == "daily-standup")
    #expect(
      export.decisions.first?.id == UUID(derivedFrom: SampleData.meetingID, salt: "decision-0")
    )
  }

  @Test func exportMatchesTheSampleExport() async throws {
    let store = try await Self.populated()
    let export = try await store.export(meetingID: SampleData.meetingID)
    #expect(export == SampleData.export())
    await #expect(throws: MeetingStoreError.meetingNotFound(SampleData.uuid(999))) {
      _ = try await store.export(meetingID: SampleData.uuid(999))
    }
  }

  @Test func assetsRoundTripAndExpire() async throws {
    let store = try MeetingStore.inMemory()
    try await store.save(SampleData.meeting())
    var asset = SampleData.audioAsset()
    try await store.save(asset)
    #expect(try await store.asset(id: asset.id) == asset)
    #expect(try await store.asset(meetingID: SampleData.meetingID) == asset)
    let expiry = try #require(asset.expiresAt)
    #expect(try await store.expiredAssets(now: expiry.addingTimeInterval(-1)).isEmpty)
    #expect(try await store.expiredAssets(now: expiry) == [asset])
    asset.retention = .keepForever
    asset.expiresAt = nil
    try await store.save(asset)
    #expect(try await store.expiredAssets(now: .distantFuture).isEmpty)
  }

  @Test func deliveriesAreOneRowPerDestination() async throws {
    let store = try MeetingStore.inMemory()
    try await store.save(SampleData.meeting())
    try await store.save(SampleData.delivery())
    var retry = SampleData.delivery()
    retry.status = .failed("vault missing")
    try await store.save(retry)
    let deliveries = try await store.deliveries(meetingID: SampleData.meetingID)
    #expect(deliveries == [retry], "the same pair derives the same id and upserts")
    #expect(
      retry.id == Delivery.id(meetingID: SampleData.meetingID, destinationID: "obsidian-folder"))
    let other = Delivery(
      meetingID: SampleData.meetingID, destinationID: "another", status: .pending)
    try await store.save(other)
    #expect(try await store.deliveries(meetingID: SampleData.meetingID).count == 2)
  }

  @Test func observeMeetingsYieldsAgainAfterASave() async throws {
    let store = try MeetingStore.inMemory()
    var iterator = store.observeMeetings().makeAsyncIterator()
    #expect(try await iterator.next() == [])
    try await store.save(SampleData.meeting())
    #expect(try await iterator.next() == [SampleData.meeting()])
  }

  @Test func observeMeetingYieldsTheExport() async throws {
    let store = try MeetingStore.inMemory()
    var iterator = store.observeMeeting(id: SampleData.meetingID).makeAsyncIterator()
    #expect(try await iterator.next() == .some(nil))
    try await store.save(SampleData.meeting())
    let export = try #require(try await iterator.next())
    #expect(export?.meeting == SampleData.meeting())
  }

  @Test func observeDeliveriesYieldsAgainAfterASave() async throws {
    let store = try MeetingStore.inMemory()
    try await store.save(SampleData.meeting())
    var iterator = store.observeDeliveries(meetingID: SampleData.meetingID).makeAsyncIterator()
    #expect(try await iterator.next() == [])
    try await store.save(SampleData.delivery())
    #expect(try await iterator.next() == [SampleData.delivery()])
  }

  @Test func mergePersonsRepointsAndRefreshesTheKeptVoice() async throws {
    let store = try await Self.populated()
    var task = SampleData.tasks()[0]
    task.assigneePersonID = SampleData.personNicolaiID
    try await store.replaceSummary(
      SampleData.meeting(), tasks: [task], decisions: SampleData.decisions().map(\.text))

    try await store.mergePersons(
      keep: SampleData.personJeromeID, remove: SampleData.personNicolaiID)

    let persons = try await store.persons()
    #expect(persons.map(\.id) == [SampleData.personJeromeID])
    let kept = try #require(persons.first)
    // Speaker 1 (axis 0) is now confirmed to Jérôme; Speaker 2 is only
    // suggested, so it does not count.
    #expect(kept.sampleCount == 1)
    #expect(kept.embedding == SampleData.embedding(axis: 0))
    #expect(kept.email == "nicolai@example.com")

    let export = try await store.export(meetingID: SampleData.meetingID)
    #expect(
      export.speakers.map(\.personID) == [SampleData.personJeromeID, SampleData.personJeromeID])
    #expect(export.participants.compactMap(\.personID) == [SampleData.personJeromeID])
    #expect(export.tasks.first?.assigneePersonID == SampleData.personJeromeID)
    await #expect(throws: MeetingStoreError.personNotFound(SampleData.uuid(999))) {
      try await store.mergePersons(keep: SampleData.personJeromeID, remove: SampleData.uuid(999))
    }
  }

  @Test func mergeSpeakersMovesSegmentsAndDeletesTheSource() async throws {
    let store = try await Self.populated()
    let directory = try Fixtures.temporaryDirectory()
    defer { try? FileManager.default.removeItem(at: directory) }
    let clip = directory.appendingPathComponent("source.wav")
    try Data([1, 2, 3]).write(to: clip)
    var speakers = SampleData.speakers()
    speakers[1].sampleClipURL = clip
    try await store.replaceTranscript(
      SampleData.meeting(), segments: SampleData.segments(), speakers: speakers)

    try await store.mergeSpeakers(
      SampleData.speakerTwoID, into: SampleData.speakerOneID, meetingID: SampleData.meetingID)

    let export = try await store.export(meetingID: SampleData.meetingID)
    #expect(export.speakers.map(\.id) == [SampleData.speakerOneID])
    #expect(
      export.segments.compactMap(\.speakerID) == [SampleData.speakerOneID, SampleData.speakerOneID])
    let merged = try #require(export.speakers.first)
    #expect(merged.assignment == .confirmed(personID: SampleData.personNicolaiID))
    let embedding = try #require(merged.embedding)
    #expect(abs(embedding.values[0] - 0.7071) < 0.001)
    #expect(abs(embedding.values[1] - 0.7071) < 0.001)
    #expect(!FileManager.default.fileExists(atPath: clip.path))
    await #expect(throws: MeetingStoreError.speakerNotFound(SampleData.speakerTwoID)) {
      try await store.mergeSpeakers(
        SampleData.speakerTwoID, into: SampleData.speakerOneID, meetingID: SampleData.meetingID)
    }
  }

  /// A store whose asset points at a master file that exists, so confirm
  /// keeps the clips; the clip file itself for Speaker 2.
  static func populatedWithAudio(in directory: URL) async throws -> (MeetingStore, URL) {
    let store = try await populated()
    let master = directory.appendingPathComponent("master.caf")
    try Data([9, 9, 9]).write(to: master)
    var asset = SampleData.audioAsset()
    asset.url = master
    try await store.save(asset)
    let clip = directory.appendingPathComponent("speaker-two.wav")
    try Data([1, 2, 3]).write(to: clip)
    return (store, clip)
  }

  static let anna = Person(
    id: SampleData.uuid(12), displayName: "Anna", createdAt: SampleData.createdAt)
  static let bea = Person(
    id: SampleData.uuid(13), displayName: "Bea", createdAt: SampleData.createdAt)

  @Test func confirmSetsConfirmedRefreshesTheVoiceAndKeepsTheClip() async throws {
    let directory = try Fixtures.temporaryDirectory()
    defer { try? FileManager.default.removeItem(at: directory) }
    let (store, clip) = try await Self.populatedWithAudio(in: directory)
    var speakers = SampleData.speakers()
    speakers[1].assignment = .unknown
    speakers[1].sampleClipURL = clip
    try await store.replaceTranscript(
      SampleData.meeting(), segments: SampleData.segments(), speakers: speakers)

    try await store.confirm(speakerID: SampleData.speakerTwoID, person: Self.anna)

    let speaker = try #require(try await store.speakers(meetingID: SampleData.meetingID).last)
    #expect(speaker.assignment == .confirmed(personID: Self.anna.id))
    #expect(speaker.sampleClipURL == clip, "the clip stays while the recording exists")
    #expect(FileManager.default.fileExists(atPath: clip.path))
    let stored = try #require(try await store.person(id: Self.anna.id))
    #expect(stored.displayName == "Anna")
    #expect(stored.sampleCount == 1)
    #expect(stored.embedding == SampleData.embedding(axis: 1), "the voice is the speaker's")
    #expect(
      try await store.export(meetingID: SampleData.meetingID).persons.map(\.displayName) == [
        "Anna", "Jérôme", "Nicolai",
      ])
  }

  @Test func confirmTwiceWithTheSamePersonIsANoOp() async throws {
    let store = try await Self.populated()
    try await store.confirm(speakerID: SampleData.speakerTwoID, person: Self.anna)
    let once = try await store.person(id: Self.anna.id)
    let speakersOnce = try await store.speakers(meetingID: SampleData.meetingID)

    try await store.confirm(speakerID: SampleData.speakerTwoID, person: Self.anna)

    #expect(try await store.person(id: Self.anna.id) == once)
    #expect(try await store.speakers(meetingID: SampleData.meetingID) == speakersOnce)
    #expect(once?.sampleCount == 1)
  }

  /// Anna already has a voice from another meeting; confirming Speaker 2 to
  /// her and then to Bea leaves Anna exactly as she was.
  @Test func confirmAThenBRestoresAExactly() async throws {
    let store = try await Self.populated()
    var earlier = SampleData.meeting()
    earlier.id = SampleData.uuid(2)
    earlier.startedAt = SampleData.startedAt.addingTimeInterval(-86_400)
    try await store.save(earlier)
    try await store.save(Self.anna)
    let annasVoice = Speaker(
      id: SampleData.uuid(30), meetingID: earlier.id, clusterLabel: "Speaker 1",
      assignment: .confirmed(personID: Self.anna.id), embedding: SampleData.embedding(axis: 5),
      clusterConfidence: 0.8)
    try await store.replaceTranscript(earlier, segments: [], speakers: [annasVoice])
    try await store.confirm(speakerID: annasVoice.id, person: Self.anna)  // no-op; voice as seeded
    try await store.mergePersons(keep: Self.anna.id, remove: Self.anna.id)
    // Force one refresh so `before` is the recomputed voice.
    try await store.confirm(speakerID: SampleData.speakerTwoID, person: Self.bea)
    try await store.confirm(speakerID: SampleData.speakerTwoID, person: Self.anna)
    let mixed = try #require(try await store.person(id: Self.anna.id))
    #expect(mixed.sampleCount == 2)
    let values = try #require(mixed.embedding?.values)
    #expect(abs(values[1] - values[5]) < 1e-6, "axis 1 and axis 5 at equal weight")

    try await store.confirm(speakerID: SampleData.speakerTwoID, person: Self.bea)

    let restored = try #require(try await store.person(id: Self.anna.id))
    #expect(restored.sampleCount == 1)
    #expect(restored.embedding == SampleData.embedding(axis: 5))
    let bea = try #require(try await store.person(id: Self.bea.id))
    #expect(bea.sampleCount == 1)
    #expect(bea.embedding == SampleData.embedding(axis: 1))
    #expect(
      try await store.speakers(meetingID: SampleData.meetingID).last?.assignment
        == .confirmed(personID: Self.bea.id))
  }

  @Test func confirmingAwayFromASuggestionLeavesTheSuggestedPersonUntouched() async throws {
    let store = try await Self.populated()
    try await store.confirm(speakerID: SampleData.speakerTwoID, person: Self.anna)
    #expect(try await store.person(id: SampleData.personJeromeID) == SampleData.persons()[0])
  }

  @Test func confirmIntoAPersonOwningAnotherSpeakerMerges() async throws {
    let store = try await Self.populated()

    try await store.confirm(speakerID: SampleData.speakerTwoID, person: SampleData.persons()[1])

    let export = try await store.export(meetingID: SampleData.meetingID)
    #expect(export.speakers.map(\.id) == [SampleData.speakerOneID], "Speaker 2 merged into 1")
    #expect(
      export.segments.compactMap(\.speakerID) == [SampleData.speakerOneID, SampleData.speakerOneID])
    let nicolai = try #require(export.person(id: SampleData.personNicolaiID))
    #expect(nicolai.sampleCount == 1, "one merged cluster")
    let values = try #require(nicolai.embedding?.values)
    #expect(abs(values[0] - 0.7071) < 0.001)
    #expect(abs(values[1] - 0.7071) < 0.001)
  }

  @Test func confirmDeletesTheClipWhenTheAudioIsGone() async throws {
    let store = try await Self.populated()  // the sample asset's master does not exist
    let directory = try Fixtures.temporaryDirectory()
    defer { try? FileManager.default.removeItem(at: directory) }
    let clip = directory.appendingPathComponent("speaker-two.wav")
    try Data([1, 2, 3]).write(to: clip)
    var speakers = SampleData.speakers()
    speakers[1].sampleClipURL = clip
    try await store.replaceTranscript(
      SampleData.meeting(), segments: SampleData.segments(), speakers: speakers)

    try await store.confirm(speakerID: SampleData.speakerTwoID, person: Self.anna)

    let speaker = try #require(try await store.speakers(meetingID: SampleData.meetingID).last)
    #expect(speaker.sampleClipURL == nil)
    #expect(!FileManager.default.fileExists(atPath: clip.path))
  }

  @Test func mergeIntoAnUnknownTargetKeepsTheVoice() async throws {
    let store = try await Self.populated()
    var speakers = SampleData.speakers()
    speakers[0].assignment = .unknown
    speakers[0].embedding = nil
    speakers[1].assignment = .confirmed(personID: SampleData.personJeromeID)
    try await store.replaceTranscript(
      SampleData.meeting(), segments: SampleData.segments(), speakers: speakers)

    try await store.mergeSpeakers(
      SampleData.speakerTwoID, into: SampleData.speakerOneID, meetingID: SampleData.meetingID)

    let kept = try #require(try await store.speakers(meetingID: SampleData.meetingID).first)
    #expect(kept.assignment == .confirmed(personID: SampleData.personJeromeID))
    let jerome = try #require(try await store.person(id: SampleData.personJeromeID))
    #expect(jerome.sampleCount == 1)
    #expect(jerome.embedding == SampleData.embedding(axis: 1))
  }

  /// Fifty-one confirmed speakers over fifty-one meetings plus one whose
  /// embedding has the wrong dimension: the voice is the newest fifty valid
  /// ones, so the oldest (axis 7) drops out and the odd one is skipped.
  @Test func refreshVoiceSkipsAMismatchedDimensionAndCapsAtFifty() async throws {
    let store = try MeetingStore.inMemory()
    try await store.save(Self.anna)
    var newest: Speaker?
    for index in 0..<52 {
      var meeting = SampleData.meeting()
      meeting.id = SampleData.uuid(100 + index)
      meeting.startedAt = SampleData.startedAt.addingTimeInterval(TimeInterval(index) * 60)
      try await store.save(meeting)
      let embedding: Embedding =
        switch index {
        case 0: SampleData.embedding(axis: 7)
        case 51: Embedding([1, 0, 0])
        default: SampleData.embedding(axis: 0)
        }
      let speaker = Speaker(
        id: SampleData.uuid(200 + index), meetingID: meeting.id, clusterLabel: "Speaker 1",
        assignment: index == 51 ? .unknown : .confirmed(personID: Self.anna.id),
        embedding: embedding, clusterConfidence: 0.9)
      try await store.replaceTranscript(meeting, segments: [], speakers: [speaker])
      if index == 51 { newest = speaker }
    }

    try await store.confirm(speakerID: try #require(newest).id, person: Self.anna)

    let anna = try #require(try await store.person(id: Self.anna.id))
    #expect(anna.sampleCount == 50)
    let values = try #require(anna.embedding?.values)
    #expect(values[7] == 0, "the fifty-first newest sample is outside the window")
    #expect(abs(values[0] - 1) < 1e-6)
  }

  @Test func recentPersonsOrdersByLatestConfirmedMeeting() async throws {
    let store = try await Self.populated()
    #expect(
      try await store.recentPersons().map(\.displayName) == ["Nicolai"],
      "Jérôme is only suggested")

    var later = SampleData.meeting()
    later.id = SampleData.uuid(2)
    later.startedAt = SampleData.startedAt.addingTimeInterval(3600)
    try await store.save(later)
    let speaker = Speaker(
      id: SampleData.uuid(30), meetingID: later.id, clusterLabel: "Speaker 1",
      assignment: .confirmed(personID: SampleData.personJeromeID),
      embedding: SampleData.embedding(axis: 1), clusterConfidence: 0.9)
    try await store.replaceTranscript(later, segments: [], speakers: [speaker])

    #expect(try await store.recentPersons().map(\.displayName) == ["Jérôme", "Nicolai"])
    #expect(try await store.persons().map(\.displayName) == ["Jérôme", "Nicolai"], "by name")
  }

  @Test func handoverRowsRoundTrip() async throws {
    let store = try MeetingStore.inMemory()
    let hash = Data(repeating: 9, count: 32)
    try await store.save(SampleData.pairedDevice(), tokenHash: hash)
    #expect(try await store.pairedDevices() == [SampleData.pairedDevice()])
    #expect(try await store.device(forTokenHash: hash) == SampleData.pairedDevice())
    #expect(try await store.device(forTokenHash: Data(repeating: 1, count: 32)) == nil)
    try await store.save(SampleData.meeting())
    try await store.save(SampleData.handoverReceipt())
    #expect(
      try await store.handoverReceipt(recordingID: SampleData.uuid(91))
        == SampleData.handoverReceipt())
    var receipt = SampleData.handoverReceipt()
    receipt.state = .failed("hash mismatch")
    try await store.save(receipt)
    #expect(
      try await store.handoverReceipt(recordingID: SampleData.uuid(91))?.state
        == .failed("hash mismatch"))
    try await store.delete(deviceID: SampleData.uuid(90))
    #expect(try await store.pairedDevices().isEmpty)
    #expect(try await store.handoverReceipt(recordingID: SampleData.uuid(91)) == nil)
  }

  @Test func rebuildSearchIndexRestoresMatches() async throws {
    let store = try await Self.populated()
    try await store.writer.write { db in
      try db.execute(
        sql: "INSERT INTO transcriptSegment_ft(transcriptSegment_ft) VALUES('delete-all')")
    }
    #expect(try await store.search("neunzig").isEmpty)
    try await store.rebuildSearchIndex()
    #expect(try await store.search("neunzig").count == 1)
  }

  @Test func onDiskStoreUsesWALAndMigrates() async throws {
    let directory = try Fixtures.temporaryDirectory()
    defer { try? FileManager.default.removeItem(at: directory) }
    let url = directory.appendingPathComponent("nested/steno.sqlite")
    let store = try MeetingStore.onDisk(at: url)
    try await store.save(SampleData.meeting())
    #expect(FileManager.default.fileExists(atPath: url.path))
    let mode = try await store.writer.read { db in
      try String.fetchOne(db, sql: "PRAGMA journal_mode")
    }
    #expect(mode?.lowercased() == "wal")
    let reopened = try MeetingStore.onDisk(at: url)
    #expect(try await reopened.meeting(id: SampleData.meetingID) == SampleData.meeting())
  }

  @Test func derivedIDsAreStableDistinctAndWellFormed() {
    let a = UUID(derivedFrom: SampleData.meetingID, salt: "decision-0")
    #expect(a == UUID(derivedFrom: SampleData.meetingID, salt: "decision-0"))
    #expect(a != UUID(derivedFrom: SampleData.meetingID, salt: "decision-1"))
    #expect(a != UUID(derivedFrom: SampleData.uuid(2), salt: "decision-0"))
    #expect(a.uuidString[a.uuidString.index(a.uuidString.startIndex, offsetBy: 14)] == "4")
  }

  @Test func mergeSpeakersTakesTheSourceAssignmentAndClipWhenTheTargetHasNone() async throws {
    let store = try await Self.populated()
    let directory = try Fixtures.temporaryDirectory()
    defer { try? FileManager.default.removeItem(at: directory) }
    let clip = directory.appendingPathComponent("source.wav")
    try Data([1, 2, 3]).write(to: clip)
    var speakers = SampleData.speakers()
    speakers[0].assignment = .unknown
    speakers[0].embedding = nil
    speakers[0].sampleClipRange = nil
    speakers[0].clusterConfidence = 0.3
    speakers[1].sampleClipURL = clip
    try await store.replaceTranscript(
      SampleData.meeting(), segments: SampleData.segments(), speakers: speakers)

    try await store.mergeSpeakers(
      SampleData.speakerTwoID, into: SampleData.speakerOneID, meetingID: SampleData.meetingID)

    let remaining = try await store.speakers(meetingID: SampleData.meetingID)
    #expect(remaining.map(\.id) == [SampleData.speakerOneID])
    let kept = try #require(remaining.first)
    #expect(kept.assignment == .suggested(personID: SampleData.personJeromeID, similarity: 0.72))
    #expect(kept.sampleClipRange == 3...5.5)
    #expect(kept.sampleClipURL == clip, "the clip moves with its range")
    #expect(FileManager.default.fileExists(atPath: clip.path))
    #expect(kept.clusterConfidence == 0.75)
    #expect(
      kept.embedding == SampleData.embedding(axis: 1), "a missing embedding takes the source's")
    #expect(kept.clusterLabel == "Speaker 1")
  }

  @Test func mergeSpeakersRefusesOtherMeetingsAndSelfMergesAreNoOps() async throws {
    let store = try await Self.populated()
    var other = SampleData.meeting()
    other.id = SampleData.uuid(2)
    try await store.save(other)
    let stranger = Speaker(
      id: SampleData.uuid(25), meetingID: other.id, clusterLabel: "Speaker 1",
      clusterConfidence: 0.5)
    try await store.replaceTranscript(other, segments: [], speakers: [stranger])

    await #expect(
      throws: MeetingStoreError.speakersInDifferentMeetings(stranger.id, SampleData.speakerOneID)
    ) {
      try await store.mergeSpeakers(
        stranger.id, into: SampleData.speakerOneID, meetingID: SampleData.meetingID)
    }
    await #expect(throws: MeetingStoreError.speakerNotFound(SampleData.uuid(999))) {
      try await store.mergeSpeakers(
        SampleData.speakerTwoID, into: SampleData.uuid(999), meetingID: SampleData.meetingID)
    }
    try await store.mergeSpeakers(
      SampleData.speakerOneID, into: SampleData.speakerOneID, meetingID: SampleData.meetingID)
    try await store.mergePersons(
      keep: SampleData.personNicolaiID, remove: SampleData.personNicolaiID)
    #expect(try await store.export(meetingID: SampleData.meetingID) == SampleData.export())
    #expect(try await store.speakers(meetingID: other.id) == [stranger])
  }

  @Test func confirmKeepsAnExistingPersonAndLeavesTheVoiceWithoutAnEmbedding() async throws {
    let store = try await Self.populated()
    var speakers = SampleData.speakers()
    speakers[1].assignment = .unknown
    speakers[1].sampleClipURL = nil
    var bare = speakers[0]
    bare.id = SampleData.uuid(22)
    bare.clusterLabel = "Speaker 3"
    bare.assignment = .unknown
    bare.embedding = nil
    bare.sampleClipRange = nil
    speakers.append(bare)
    try await store.replaceTranscript(
      SampleData.meeting(), segments: SampleData.segments(), speakers: speakers)
    var renamed = SampleData.persons()[1]
    renamed.displayName = "Somebody Else"
    renamed.email = nil

    try await store.confirm(speakerID: SampleData.speakerTwoID, person: renamed)

    let nicolai = try #require(try await store.person(id: SampleData.personNicolaiID))
    #expect(nicolai.displayName == "Nicolai", "an existing person row is not overwritten")
    #expect(nicolai.email == "nicolai@example.com")
    // Nicolai already owned Speaker 1, so Speaker 2 merged into it: one
    // cluster with the averaged embedding is his voice now.
    #expect(nicolai.sampleCount == 1)
    let values = try #require(nicolai.embedding?.values)
    #expect(abs(values[0] - values[1]) < 1e-6)
    #expect(try await store.persons().count == 2)
    #expect(
      try await store.speakers(meetingID: SampleData.meetingID).map(\.assignment) == [
        .confirmed(personID: SampleData.personNicolaiID), .unknown,
      ])

    try await store.confirm(speakerID: bare.id, person: SampleData.persons()[0])
    let jerome = try #require(try await store.person(id: SampleData.personJeromeID))
    #expect(jerome.embedding == nil, "no confirmed speaker with an embedding, no voice")
    #expect(jerome.sampleCount == 0)
    #expect(
      try await store.speakers(meetingID: SampleData.meetingID).last?.assignment
        == .confirmed(personID: SampleData.personJeromeID))
    await #expect(throws: MeetingStoreError.speakerNotFound(SampleData.uuid(999))) {
      try await store.confirm(speakerID: SampleData.uuid(999), person: renamed)
    }
  }

  @Test func resolvePersonReusesJeromeForJérôme() async throws {
    let store = try await Self.populated()
    let found = try await store.resolvePerson(named: "  jerome ", now: SampleData.createdAt)
    #expect(found == SampleData.persons()[0])
    let exact = try await store.resolvePerson(named: "Nicolai", now: SampleData.createdAt)
    #expect(exact.id == SampleData.personNicolaiID)
    #expect(try await store.persons().count == 2, "nothing is written")
  }

  @Test func resolvePersonCreatesWithEmail() async throws {
    let store = try await Self.populated()
    let created = try await store.resolvePerson(
      named: " Maya ", email: "maya@example.com", now: SampleData.createdAt)
    #expect(created.displayName == "Maya")
    #expect(created.email == "maya@example.com")
    #expect(created.sampleCount == 0)
    #expect(created.embedding == nil)
    #expect(created.createdAt == SampleData.createdAt)
    #expect(try await store.person(id: created.id) == nil, "unsaved until confirm")
  }

  @Test func resolvePersonIgnoresBlank() async throws {
    let store = try await Self.populated()
    await #expect(throws: MeetingStoreError.blankPersonName) {
      try await store.resolvePerson(named: " \n ", now: SampleData.createdAt)
    }
  }

  @Test func transcriptAndSummaryWritesNeedTheMeeting() async throws {
    let store = try MeetingStore.inMemory()
    await #expect(throws: MeetingStoreError.meetingNotFound(SampleData.meetingID)) {
      try await store.replaceTranscript(SampleData.meeting(), segments: [], speakers: [])
    }
    await #expect(throws: MeetingStoreError.meetingNotFound(SampleData.meetingID)) {
      try await store.replaceSummary(SampleData.meeting(), tasks: [], decisions: [])
    }
    await #expect(throws: MeetingStoreError.meetingNotFound(SampleData.meetingID)) {
      try await store.update(meetingID: SampleData.meetingID, now: SampleData.updatedAt) { _ in }
    }
  }
}
