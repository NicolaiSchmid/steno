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
      meetingID: SampleData.meetingID, segments: SampleData.segments(),
      speakers: SampleData.speakers())
    try await store.replaceSummary(
      meetingID: SampleData.meetingID, output: SampleData.summaryOutput(),
      templateID: SummaryTemplate.defaultID, now: SampleData.updatedAt)
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
      try await store.setState(.ready, meetingID: SampleData.uuid(999))
    }
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
      meetingID: SampleData.meetingID, segments: [newSegment], speakers: [newSpeaker])
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

    var again = SampleData.summaryOutput()
    again.decisions = ["Nur eine."]
    again.tasks = []
    try await store.replaceSummary(
      meetingID: SampleData.meetingID, output: again, templateID: "daily-standup")
    let second = try await store.export(meetingID: SampleData.meetingID)
    #expect(second.tasks.isEmpty)
    #expect(second.decisions.map(\.text) == ["Nur eine."])
    #expect(second.meeting.templateID == "daily-standup")
    #expect(second.meeting.summary?.templateID == "daily-standup")
    #expect(
      export.decisions.first?.id == MeetingStore.derivedID(SampleData.meetingID, salt: "decision-0")
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

  @Test func deliveriesUpsertPerDestination() async throws {
    let store = try MeetingStore.inMemory()
    try await store.save(SampleData.meeting())
    try await store.save(SampleData.delivery())
    var retry = SampleData.delivery()
    retry.id = SampleData.uuid(82)
    retry.status = .failed("vault missing")
    try await store.save(retry)
    let deliveries = try await store.deliveries(meetingID: SampleData.meetingID)
    #expect(deliveries == [retry])
    var other = SampleData.delivery()
    other.id = SampleData.uuid(83)
    other.destinationID = "another"
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

  @Test func mergePersonsRepointsAndAverages() async throws {
    let store = try await Self.populated()
    var task = SampleData.tasks()[0]
    task.assigneePersonID = SampleData.personNicolaiID
    var output = SampleData.summaryOutput()
    output.tasks = [task]
    try await store.replaceSummary(
      meetingID: SampleData.meetingID, output: output, templateID: "default")

    try await store.mergePersons(
      keep: SampleData.personJeromeID, remove: SampleData.personNicolaiID)

    let persons = try await store.persons()
    #expect(persons.map(\.id) == [SampleData.personJeromeID])
    let kept = try #require(persons.first)
    #expect(kept.sampleCount == 4)
    let embedding = try #require(kept.embedding)
    // Jérôme (axis 1, weight 1) with Nicolai (axis 0, weight 3), renormalised.
    #expect(abs(embedding.values[0] - 0.9487) < 0.001)
    #expect(abs(embedding.values[1] - 0.3162) < 0.001)
    #expect(abs(embedding.magnitude - 1) < 0.0001)
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
      meetingID: SampleData.meetingID, segments: SampleData.segments(), speakers: speakers)

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

  @Test func confirmSetsConfirmedEnrolsOnceAndRemovesTheClip() async throws {
    let store = try await Self.populated()
    let directory = try Fixtures.temporaryDirectory()
    defer { try? FileManager.default.removeItem(at: directory) }
    let clip = directory.appendingPathComponent("speaker-two.wav")
    try Data([1, 2, 3]).write(to: clip)
    var speakers = SampleData.speakers()
    speakers[1].assignment = .unknown
    speakers[1].sampleClipURL = clip
    try await store.replaceTranscript(
      meetingID: SampleData.meetingID, segments: SampleData.segments(), speakers: speakers)
    let memory = InMemorySpeakerMemory(people: SampleData.persons())
    let newPerson = Person(
      id: SampleData.uuid(12), displayName: "Anna", createdAt: SampleData.createdAt)

    try await store.confirm(speakerID: SampleData.speakerTwoID, person: newPerson, memory: memory)

    let speaker = try #require(try await store.speakers(meetingID: SampleData.meetingID).last)
    #expect(speaker.assignment == .confirmed(personID: newPerson.id))
    #expect(speaker.sampleClipURL == nil)
    #expect(!FileManager.default.fileExists(atPath: clip.path))
    #expect(try await store.person(id: newPerson.id) == newPerson)
    let enrolments = await memory.enrolments
    #expect(enrolments == [.init(embedding: SampleData.embedding(axis: 1), personID: newPerson.id)])
    #expect(
      try await store.export(meetingID: SampleData.meetingID).persons.map(\.displayName) == [
        "Anna", "Jérôme", "Nicolai",
      ])
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
    #expect(try await store.receipt(SampleData.uuid(91)) == SampleData.handoverReceipt())
    var receipt = SampleData.handoverReceipt()
    receipt.state = .failed("hash mismatch")
    try await store.save(receipt)
    #expect(try await store.receipt(SampleData.uuid(91))?.state == .failed("hash mismatch"))
    try await store.delete(deviceID: SampleData.uuid(90))
    #expect(try await store.pairedDevices().isEmpty)
    #expect(try await store.receipt(SampleData.uuid(91)) == nil)
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

  @Test func derivedIDsAreStableAndDistinct() {
    let a = MeetingStore.derivedID(SampleData.meetingID, salt: "decision-0")
    #expect(a == MeetingStore.derivedID(SampleData.meetingID, salt: "decision-0"))
    #expect(a != MeetingStore.derivedID(SampleData.meetingID, salt: "decision-1"))
    #expect(a != MeetingStore.derivedID(SampleData.uuid(2), salt: "decision-0"))
  }
}
