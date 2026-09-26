import Foundation
import Testing

@testable import StenoCore

@Suite struct LocalRecordingIntakeTests {
  actor Enqueued {
    var calls: [(Meeting, AudioAsset)] = []
    func record(_ meeting: Meeting, _ asset: AudioAsset) { calls.append((meeting, asset)) }
  }

  struct Fixture {
    let store: MeetingStore
    let settingsStore: SettingsStore
    let enqueued = Enqueued()
    var settings = Settings()

    init(enqueueFailure: (any Error & Sendable)? = nil) async throws {
      store = try MeetingStore.inMemory()
      settingsStore = SettingsStore(writer: store.writer)
      settings.defaultRetention = .keepDays(3)
      settings.defaultTemplateID = "interview"
      try await settingsStore.save(settings)
    }

    func intake(enqueueFailure: (any Error & Sendable)? = nil) -> LocalRecordingIntake {
      let store = store
      let enqueued = enqueued
      return LocalRecordingIntake(
        store: store, settings: settingsStore,
        enqueue: { meeting, asset in
          if let enqueueFailure { throw enqueueFailure }
          // Like the pipeline's enqueue: the rows exist afterwards.
          try await store.save(meeting, asset: asset)
          await enqueued.record(meeting, asset)
        },
        now: { SampleData.createdAt })
    }
  }

  static func capturedAsset(meetingID: UUID = SampleData.uuid(999)) -> AudioAsset {
    let folder = URL(fileURLWithPath: "/tmp/steno/\(meetingID.uuidString)", isDirectory: true)
    return AudioAsset(
      id: SampleData.uuid(70), meetingID: meetingID, url: folder.appendingPathComponent("rec.caf"),
      format: .caf48kFloat32, lanes: [.mic, .system],
      sidecars16k: [.mic: folder.appendingPathComponent("mic.wav")],
      retention: .keepForever, expiresAt: SampleData.updatedAt)
  }

  @Test func beginWritesTheRecordingRowWithTitleAndDedupedAttendees() async throws {
    let fixture = try await Fixture()
    let intake = fixture.intake()
    let meeting = try await intake.begin(
      source: .macCall, title: "  Produktstrategie ", calendarEventID: "event-1",
      attendees: [
        .init(displayName: "Jérôme", email: "Jerome@Example.com"),
        .init(displayName: "Nicolai"),
        .init(displayName: "J. Dupont", email: "jerome@example.com"),
        .init(displayName: "nicolai"),
        .init(displayName: "   "),
      ],
      startedAt: SampleData.startedAt)

    #expect(meeting.title == "Produktstrategie")
    #expect(meeting.state == .recording)
    #expect(meeting.source == .macCall)
    #expect(meeting.calendarEventID == "event-1")
    #expect(meeting.duration == 0)
    #expect(meeting.startedAt == SampleData.startedAt)
    #expect(meeting.templateID == "interview")
    #expect(meeting.createdAt == SampleData.createdAt)
    #expect(meeting.updatedAt == SampleData.createdAt)
    #expect(try await fixture.store.meeting(id: meeting.id) == meeting)
    let participants = try await fixture.store.participants(meetingID: meeting.id)
    #expect(participants.map(\.displayName) == ["Jérôme", "Nicolai"])
    #expect(participants.map(\.email) == ["jerome@example.com", nil])
    #expect(participants.allSatisfy { $0.role == .them && $0.personID == nil })
    let derived: Set<UUID> = [
      UUID(derivedFrom: meeting.id, salt: "attendee-0"),
      UUID(derivedFrom: meeting.id, salt: "attendee-1"),
    ]
    #expect(Set(participants.map(\.id)) == derived, "ids derive from the meeting id")
    #expect(await fixture.enqueued.calls.isEmpty)
  }

  @Test func beginWithoutATitleUsesTheDefaultForTheSource() async throws {
    let fixture = try await Fixture()
    let intake = fixture.intake()
    let call = try await intake.begin(source: .macCall, title: "", startedAt: SampleData.startedAt)
    let room = try await intake.begin(source: .macInPerson, startedAt: SampleData.startedAt)
    #expect(
      call.title == LocalRecordingIntake.defaultTitle(source: .macCall, startedAt: call.startedAt))
    #expect(call.title.hasPrefix("Call 2026-09-24"))
    #expect(room.title.hasPrefix("Meeting 2026-09-24"))
    #expect(call.id != room.id)
    let berlin = TimeZone(identifier: "Europe/Berlin")!
    #expect(
      LocalRecordingIntake.defaultTitle(
        source: .macCall, startedAt: SampleData.startedAt, timeZone: berlin)
        == "Call 2026-09-24 11:00")
    #expect(
      LocalRecordingIntake.defaultTitle(
        source: .macInPerson, startedAt: SampleData.startedAt, timeZone: berlin)
        == "Meeting 2026-09-24 11:00")
    #expect(
      LocalRecordingIntake.defaultTitle(
        source: .phone, startedAt: SampleData.startedAt, timeZone: berlin)
        == "Phone recording 2026-09-24 11:00")
  }

  @Test func completeWritesDurationRetentionAndEnqueuesQueued() async throws {
    let fixture = try await Fixture()
    let intake = fixture.intake()
    let begun = try await intake.begin(source: .macCall, startedAt: SampleData.startedAt)
    // The user types notes while recording; they must survive completion.
    try await fixture.store.update(meetingID: begun.id, now: SampleData.startedAt) {
      $0.scratchpad = "Notizen."
    }

    let completed = try await intake.complete(
      meetingID: begun.id, result: RecordingResult(asset: Self.capturedAsset(), duration: 61.5))

    let calls = await fixture.enqueued.calls
    #expect(calls.count == 1)
    let (meeting, asset) = try #require(calls.first)
    #expect(meeting == completed)
    #expect(meeting.id == begun.id)
    #expect(meeting.state == .queued)
    #expect(meeting.duration == 61.5)
    #expect(meeting.scratchpad == "Notizen.")
    #expect(meeting.updatedAt == SampleData.createdAt)
    #expect(asset.meetingID == begun.id, "the asset is re-pointed at the meeting")
    #expect(asset.id == SampleData.uuid(70))
    #expect(asset.retention == .keepDays(3), "from Settings at completion time")
    #expect(asset.expiresAt == nil, "the retention stage sets it")
    #expect(asset.url == Self.capturedAsset().url)
    #expect(asset.sidecars16k == Self.capturedAsset().sidecars16k)
    #expect(try await fixture.store.meeting(id: begun.id)?.state == .queued)
    #expect(try await fixture.store.asset(meetingID: begun.id) == asset)
  }

  @Test func completeTakesAnExplicitRetention() async throws {
    let fixture = try await Fixture()
    let intake = fixture.intake()
    let begun = try await intake.begin(source: .macInPerson, startedAt: SampleData.startedAt)
    try await intake.complete(
      meetingID: begun.id, result: RecordingResult(asset: Self.capturedAsset(), duration: 1),
      retention: .deleteAfterProcessing)
    let (_, asset) = try #require(await fixture.enqueued.calls.first)
    #expect(asset.retention == .deleteAfterProcessing)
    #expect(asset.expiresAt == nil)
  }

  @Test func completeRefusesAMeetingThatIsNotRecordingAndMarksNothing() async throws {
    let fixture = try await Fixture()
    let intake = fixture.intake()
    try await fixture.store.save(SampleData.meeting(state: .ready))
    let error = await #expect(throws: LocalRecordingIntakeError.self) {
      try await intake.complete(
        meetingID: SampleData.meetingID,
        result: RecordingResult(asset: Self.capturedAsset(), duration: 1))
    }
    #expect(error == .notRecording(SampleData.meetingID, .ready))
    #expect(error?.description.contains("ready") == true)
    #expect(try await fixture.store.meeting(id: SampleData.meetingID) == SampleData.meeting())
    #expect(await fixture.enqueued.calls.isEmpty)
    await #expect(throws: MeetingStoreError.meetingNotFound(SampleData.uuid(999))) {
      try await intake.complete(
        meetingID: SampleData.uuid(999),
        result: RecordingResult(asset: Self.capturedAsset(), duration: 1))
    }
  }

  @Test func aFailedEnqueueMarksTheMeetingFailedAndRethrows() async throws {
    struct Boom: Error {}
    let fixture = try await Fixture()
    let begun = try await fixture.intake().begin(source: .macCall, startedAt: SampleData.startedAt)
    let failing = fixture.intake(enqueueFailure: Boom())
    await #expect(throws: Boom.self) {
      try await failing.complete(
        meetingID: begun.id, result: RecordingResult(asset: Self.capturedAsset(), duration: 9))
    }
    let stored = try #require(try await fixture.store.meeting(id: begun.id))
    guard case .failed(let reason) = stored.state else {
      Issue.record("expected .failed, got \(stored.state)")
      return
    }
    #expect(reason.hasPrefix("Recording could not be saved: "))
    #expect(reason.contains("Boom"))
    #expect(stored.duration == 9, "the duration was written before the enqueue")
    #expect(try await fixture.store.asset(meetingID: begun.id) == nil)
  }

  @Test func failRecordsTheReason() async throws {
    let fixture = try await Fixture()
    let intake = fixture.intake()
    let begun = try await intake.begin(source: .macCall, startedAt: SampleData.startedAt)
    try await intake.fail(meetingID: begun.id, reason: "Recording could not start: no input")
    let stored = try #require(try await fixture.store.meeting(id: begun.id))
    #expect(stored.state == .failed(reason: "Recording could not start: no input"))
    #expect(stored.updatedAt == SampleData.createdAt)
    await #expect(throws: MeetingStoreError.meetingNotFound(SampleData.uuid(999))) {
      try await intake.fail(meetingID: SampleData.uuid(999), reason: "x")
    }
  }

  @Test func failInterruptedRecordingsMarksOnlyRecordingRowsOldestFirst() async throws {
    let store = try MeetingStore.inMemory()
    func save(_ n: Int, _ state: MeetingState, hoursAgo: Double) async throws -> UUID {
      var meeting = SampleData.meeting(state: state)
      meeting.id = SampleData.uuid(100 + n)
      meeting.startedAt = SampleData.startedAt.addingTimeInterval(-hoursAgo * 3600)
      try await store.save(meeting)
      return meeting.id
    }
    let newer = try await save(1, .recording, hoursAgo: 1)
    let older = try await save(2, .recording, hoursAgo: 5)
    let queued = try await save(3, .queued, hoursAgo: 2)
    let ready = try await save(4, .ready, hoursAgo: 3)
    let later = SampleData.updatedAt.addingTimeInterval(600)

    let failed = try await store.failInterruptedRecordings(now: later)

    #expect(failed == [older, newer])
    for id in [older, newer] {
      let meeting = try #require(try await store.meeting(id: id))
      #expect(meeting.state == .failed(reason: "Recording was interrupted before it finished."))
      #expect(meeting.updatedAt == later)
    }
    #expect(try await store.meeting(id: queued)?.state == .queued)
    #expect(try await store.meeting(id: ready)?.state == .ready)
    #expect(try await store.failInterruptedRecordings(now: later).isEmpty)
    #expect(
      try await store.meetings(inStates: [.queued, .processing]).map(\.id) == [queued],
      "oldest first, only the asked-for states")
    #expect(try await store.meetings(inStates: [.failed]).map(\.id) == [older, newer])
    #expect(try await store.meetings(inStates: []).isEmpty)
  }
}
